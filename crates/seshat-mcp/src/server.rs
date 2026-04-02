//! MCP server implementation using `rmcp`.
//!
//! `McpServer` is the main entry point. It registers tools with rmcp,
//! starts the requested transports, and handles graceful shutdown.

use std::sync::{Arc, Mutex};

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_router,
};
use rusqlite::Connection;
use seshat_core::ServerConfig;

use crate::tools::project_context::{self, ProjectContextRequest};

/// The Seshat MCP server.
///
/// Registers tools via `rmcp`'s `#[tool_router]` macro and implements
/// `ServerHandler` for protocol compliance. Holds a shared database
/// connection and repo metadata so tools can query the knowledge graph.
#[derive(Debug, Clone)]
pub struct McpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    #[allow(dead_code)]
    config: ServerConfig,
    /// Shared database connection for graph queries.
    conn: Arc<Mutex<Connection>>,
    /// Human-readable project name (derived from DB filename).
    repo_name: String,
    /// Current branch in the database.
    branch: String,
}

impl McpServer {
    /// Create a new `McpServer` with the given configuration and database connection.
    pub fn new(
        config: ServerConfig,
        conn: Arc<Mutex<Connection>>,
        repo_name: String,
        branch: String,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config,
            conn,
            repo_name,
            branch,
        }
    }
}

#[tool_router]
impl McpServer {
    /// Query project context — languages, dependencies, conventions, golden files.
    ///
    /// Call this first to understand the project's stack, structure, and coding
    /// conventions before generating code. Returns language breakdown, dependency
    /// domains with canonical packages, confidence summary, and top convention-
    /// compliant files.
    #[tool(
        description = "Query project context: languages, dependencies, conventions, golden files. Call first to understand the project before generating code."
    )]
    fn query_project_context(&self, Parameters(req): Parameters<ProjectContextRequest>) -> String {
        tracing::info!(
            tool = "query_project_context",
            focus_area = ?req.focus_area,
            "Handling query_project_context"
        );

        project_context::handle(&self.conn, &self.repo_name, &self.branch, req)
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Seshat — convention-aware project intelligence for AI agents. \
                 Use query_project_context to understand the project, \
                 query_convention to look up coding conventions, \
                 and record_decision to capture team agreements.",
        )
    }
}

/// Start the MCP server on stdio transport.
///
/// This function blocks until the server is shut down (e.g., via Ctrl+C
/// or when the client closes the connection).
pub async fn start_stdio(
    config: ServerConfig,
    conn: Arc<Mutex<Connection>>,
    repo_name: String,
    branch: String,
) -> Result<(), crate::McpError> {
    let server = McpServer::new(config, conn, repo_name, branch);

    tracing::info!("Starting MCP server on stdio transport");

    let transport = rmcp::transport::io::stdio();

    let service = server
        .serve(transport)
        .await
        .map_err(|e| crate::McpError::Transport(format!("{e}")))?;

    tracing::info!("MCP server running — waiting for client");

    // Wait for the service to complete (client disconnects or shutdown signal).
    service
        .waiting()
        .await
        .map_err(|e| crate::McpError::Transport(format!("{e}")))?;

    tracing::info!("MCP server stopped");

    Ok(())
}

/// Start the MCP server on stdio transport with an external shutdown signal.
///
/// Returns when either:
/// - The MCP client disconnects normally
/// - The `shutdown` future resolves (e.g., from Ctrl+C), after which the
///   server waits up to `drain_timeout` for the service to finish.
pub async fn start_stdio_with_shutdown(
    config: ServerConfig,
    conn: Arc<Mutex<Connection>>,
    repo_name: String,
    branch: String,
    shutdown: impl std::future::Future<Output = ()>,
    drain_timeout: std::time::Duration,
) -> Result<(), crate::McpError> {
    let server = McpServer::new(config, conn, repo_name, branch);

    tracing::info!("Starting MCP server on stdio transport");

    let transport = rmcp::transport::io::stdio();

    let service = server
        .serve(transport)
        .await
        .map_err(|e| crate::McpError::Transport(format!("{e}")))?;

    tracing::info!("MCP server running — waiting for client");

    // Pin the shutdown future so we can poll it in select!.
    tokio::pin!(shutdown);

    tokio::select! {
        result = service.waiting() => {
            result.map_err(|e| crate::McpError::Transport(format!("{e}")))?;
            tracing::info!("MCP server stopped (client disconnected)");
        }
        _ = &mut shutdown => {
            tracing::info!("Shutdown signal received, waiting up to {drain_timeout:?} for drain");
            // Give active requests time to complete, then return.
            tokio::time::sleep(drain_timeout).await;
            tracing::info!("Drain period complete, shutting down");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Arc<Mutex<Connection>> {
        let db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        db.connection().clone()
    }

    #[test]
    fn server_creates_with_default_config() {
        let conn = test_conn();
        let server = McpServer::new(
            ServerConfig::default(),
            conn,
            "test-project".to_owned(),
            "main".to_owned(),
        );
        let info = server.get_info();
        // ServerInfo should have tools capability enabled.
        assert!(info.capabilities.tools.is_some());
    }

    #[test]
    fn query_project_context_tool_returns_success_envelope() {
        let conn = test_conn();
        let server = McpServer::new(
            ServerConfig::default(),
            conn,
            "test-project".to_owned(),
            "main".to_owned(),
        );

        let result =
            server.query_project_context(Parameters(ProjectContextRequest { focus_area: None }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_project_context");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert!(parsed["data"]["languages"].is_array());
        assert!(parsed["data"]["golden_files"].is_array());
        assert!(parsed["data"]["submodules"].is_array());
    }

    #[test]
    fn query_project_context_tool_with_focus_area() {
        let conn = test_conn();

        // Insert a convention so there's something to filter.
        {
            use seshat_core::{BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, NodeId};
            use seshat_storage::{NodeRepository, SqliteNodeRepository};

            let repo = SqliteNodeRepository::new(conn.clone());
            let mut ext = serde_json::Map::new();
            ext.insert("source".into(), "auto_detected".into());
            ext.insert("detector_name".into(), "naming".into());

            let node = KnowledgeNode {
                id: NodeId(0),
                branch_id: BranchId::from("main"),
                nature: KnowledgeNature::Convention,
                weight: KnowledgeWeight::Strong,
                confidence: 0.95,
                adoption_count: 9,
                total_count: 10,
                description: "snake_case naming (Rust)".to_owned(),
                ext_data: Some(serde_json::Value::Object(ext)),
            };
            repo.insert(&node).unwrap();
        }

        let server = McpServer::new(
            ServerConfig::default(),
            conn,
            "test-project".to_owned(),
            "main".to_owned(),
        );

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: Some("naming".to_owned()),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["conventions_count"], 1);
    }
}
