//! MCP server implementation using `rmcp`.
//!
//! `McpServer` is the main entry point. It registers tools with rmcp,
//! starts the requested transports, and handles graceful shutdown.

use std::collections::HashMap;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use seshat_core::ServerConfig;

use crate::envelope::{ErrorCode, ErrorEnvelope};
use crate::scope::{self, ProjectConnection};
use crate::tools::project_context::{self, ProjectContextRequest};
use crate::tools::query_convention::{self, QueryConventionRequest};
use crate::tools::record_decision::{self, RecordDecisionRequest};
use crate::tools::remove_decision::{self, RemoveDecisionRequest};
use crate::tools::update_decision::{self, UpdateDecisionRequest};

/// The Seshat MCP server.
///
/// Registers tools via `rmcp`'s `#[tool_router]` macro and implements
/// `ServerHandler` for protocol compliance. Holds root + submodule database
/// connections so tools can route queries to the correct knowledge graph.
#[derive(Debug, Clone)]
pub struct McpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    #[allow(dead_code)]
    config: ServerConfig,
    /// Root project connection (always present).
    root: ProjectConnection,
    /// Submodule connections keyed by mount path (e.g. `"vendor/libfoo"`).
    submodules: HashMap<String, ProjectConnection>,
    /// Sorted mount paths for file_path prefix matching (longest first).
    mount_paths: Vec<String>,
}

impl McpServer {
    /// Create a new `McpServer` with root + submodule connections.
    ///
    /// For single-project mode, pass an empty `HashMap` for `submodules`.
    pub fn new(
        config: ServerConfig,
        root: ProjectConnection,
        submodules: HashMap<String, ProjectConnection>,
    ) -> Self {
        let mut mount_paths: Vec<String> = submodules.keys().cloned().collect();
        mount_paths.sort();
        Self {
            tool_router: Self::tool_router(),
            config,
            root,
            submodules,
            mount_paths,
        }
    }

    /// Resolve which `ProjectConnection` should handle a request.
    ///
    /// Delegates to [`scope::resolve_scope`] and converts errors to JSON
    /// error envelope strings.
    fn resolve_scope(
        &self,
        scope: Option<&str>,
        file_path: Option<&str>,
    ) -> Result<(&ProjectConnection, String), String> {
        scope::resolve_scope(
            scope,
            file_path,
            &self.root,
            &self.submodules,
            &self.mount_paths,
        )
        .map_err(|code| {
            let tool = ""; // will be overridden by caller
            let envelope = ErrorEnvelope::new(
                tool,
                &self.root.name,
                code,
                match code {
                    ErrorCode::UnknownScope => format!(
                        "Unknown scope '{}'. Available scopes: root, {}",
                        scope.unwrap_or(""),
                        self.mount_paths.join(", ")
                    ),
                    _ => format!("Scope resolution failed: {code}"),
                },
                "Use scope='root' or one of the available submodule mount paths",
            );
            serde_json::to_string(&envelope).unwrap_or_default()
        })
    }
}

#[tool_router]
impl McpServer {
    #[tool(
        description = "Get a high-level overview of the project: languages, dependency domains, convention confidence, and golden files (top convention-compliant exemplars). Call this FIRST before writing any code. Use the optional focus_area parameter (e.g. 'logging', 'testing') to narrow results to a specific domain. Follow up with query_convention to deep-dive into specific conventions. Use the optional file_path parameter (e.g. 'src/components/Button.tsx') for automatic submodule scope detection."
    )]
    fn query_project_context(&self, Parameters(req): Parameters<ProjectContextRequest>) -> String {
        tracing::info!(
            tool = "query_project_context",
            focus_area = ?req.focus_area,
            "Handling query_project_context"
        );

        let (pc, scope_name) =
            match self.resolve_scope(req.scope.as_deref(), req.file_path.as_deref()) {
                Ok(r) => r,
                Err(e) => return e,
            };

        project_context::handle(&pc.conn, &pc.name, &pc.branch, &scope_name, req)
    }

    #[tool(
        description = "Search conventions by topic (e.g. 'error handling', 'logging', 'naming'). Returns matching conventions with adoption rate, trend (rising/stable/declining), confidence, and code examples. Use AFTER query_project_context to deep-dive before generating code. Covers both auto-detected patterns and user-recorded decisions. The required topic parameter is searched via full-text search on convention descriptions. Use the optional file_path parameter for automatic submodule scope detection."
    )]
    fn query_convention(&self, Parameters(req): Parameters<QueryConventionRequest>) -> String {
        tracing::info!(
            tool = "query_convention",
            topic = %req.topic,
            "Handling query_convention"
        );

        let (pc, scope_name) =
            match self.resolve_scope(req.scope.as_deref(), req.file_path.as_deref()) {
                Ok(r) => r,
                Err(e) => return e,
            };

        query_convention::handle(&pc.conn, &pc.name, &pc.branch, &scope_name, req)
    }

    #[tool(
        description = "Record a convention, architectural decision, or coding rule that auto-detection missed. Use AFTER work when you discover a pattern worth preserving — e.g. wrapper facades, team style agreements, or architectural constraints. Required: description. Optional: nature ('decision'|'convention'|'preference'), weight ('rule'|'strong'), category, examples [{file, line, end_line, snippet}], reason, file_path (for automatic submodule scope detection). Immediately searchable via query_convention. Never overwritten by re-scans."
    )]
    fn record_decision(&self, Parameters(req): Parameters<RecordDecisionRequest>) -> String {
        tracing::info!(
            tool = "record_decision",
            description = %req.description,
            "Handling record_decision"
        );

        let (pc, scope_name) =
            match self.resolve_scope(req.scope.as_deref(), req.file_path.as_deref()) {
                Ok(r) => r,
                Err(e) => return e,
            };

        record_decision::handle(&pc.conn, &pc.name, &pc.branch, &scope_name, req)
    }

    #[tool(
        description = "Update a previously recorded user decision. Use when a convention evolves or needs correction — e.g. changing the description, reclassifying nature/weight, or adding evidence. Required: id (from record_decision response or query_convention results). Optional: description, nature, weight, category, examples, reason, file_path (for automatic submodule scope detection) — only provided fields are changed. Only user-recorded decisions can be updated; auto-detected conventions return NOT_USER_DECISION error."
    )]
    fn update_decision(&self, Parameters(req): Parameters<UpdateDecisionRequest>) -> String {
        tracing::info!(
            tool = "update_decision",
            node_id = req.id,
            "Handling update_decision"
        );

        let (pc, scope_name) =
            match self.resolve_scope(req.scope.as_deref(), req.file_path.as_deref()) {
                Ok(r) => r,
                Err(e) => return e,
            };

        update_decision::handle(&pc.conn, &pc.name, &pc.branch, &scope_name, req)
    }

    #[tool(
        description = "Soft-delete a previously recorded user decision that is no longer relevant or has been superseded. The record is preserved for audit trail but hidden from query_convention and query_project_context results. Required: id (node ID), reason (why it is being removed). Optional: file_path (for automatic submodule scope detection). Only user-recorded decisions can be removed; auto-detected conventions return NOT_USER_DECISION error."
    )]
    fn remove_decision(&self, Parameters(req): Parameters<RemoveDecisionRequest>) -> String {
        tracing::info!(
            tool = "remove_decision",
            node_id = req.id,
            reason = %req.reason,
            "Handling remove_decision"
        );

        let (pc, scope_name) =
            match self.resolve_scope(req.scope.as_deref(), req.file_path.as_deref()) {
                Ok(r) => r,
                Err(e) => return e,
            };

        remove_decision::handle(&pc.conn, &pc.name, &pc.branch, &scope_name, req)
    }
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Seshat — convention-aware project intelligence for AI agents.\n\
             \n\
             Protocol (understand → work → update):\n\
             1. BEFORE writing code: call query_project_context to understand the project stack, \
             then query_convention for the specific area you are working in \
             (e.g. 'error handling', 'logging', 'naming').\n\
             2. WRITE code following the discovered conventions.\n\
             3. AFTER work: if you discover a new convention not already captured \
             (e.g. a wrapper/facade pattern, an architectural decision, or a team style agreement), \
             call record_decision to persist it for future sessions.\n\
             \n\
             Use update_decision to correct or evolve a previously recorded decision, \
             and remove_decision to retire decisions that no longer apply.",
        )
    }
}

/// Start the MCP server on stdio transport.
///
/// This function blocks until the server is shut down (e.g., via Ctrl+C
/// or when the client closes the connection).
pub async fn start_stdio(
    config: ServerConfig,
    root: ProjectConnection,
    submodules: HashMap<String, ProjectConnection>,
) -> Result<(), crate::McpError> {
    let server = McpServer::new(config, root, submodules);

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
    root: ProjectConnection,
    submodules: HashMap<String, ProjectConnection>,
    shutdown: impl std::future::Future<Output = ()>,
    drain_timeout: std::time::Duration,
) -> Result<(), crate::McpError> {
    let server = McpServer::new(config, root, submodules);

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

    fn test_root() -> ProjectConnection {
        let db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        ProjectConnection::new(db.connection().clone(), "test-project", "main")
    }

    fn test_server() -> McpServer {
        McpServer::new(ServerConfig::default(), test_root(), HashMap::new())
    }

    #[test]
    fn server_creates_with_default_config() {
        let server = test_server();
        let info = server.get_info();
        // ServerInfo should have tools capability enabled.
        assert!(info.capabilities.tools.is_some());
    }

    #[test]
    fn query_project_context_tool_returns_success_envelope() {
        let server = test_server();

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

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
        let root = test_root();

        // Insert a convention so there's something to filter.
        {
            use seshat_core::{BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, NodeId};
            use seshat_storage::{NodeRepository, SqliteNodeRepository};

            let repo = SqliteNodeRepository::new(root.conn.clone());
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

        let server = McpServer::new(ServerConfig::default(), root, HashMap::new());

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: Some("naming".to_owned()),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["conventions_count"], 1);
    }

    #[test]
    fn query_convention_tool_returns_success_envelope() {
        let root = test_root();

        // Insert a convention and rebuild FTS5 index.
        {
            use seshat_core::{BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, NodeId};
            use seshat_storage::{NodeRepository, SqliteNodeRepository};

            let repo = SqliteNodeRepository::new(root.conn.clone());
            let mut ext = serde_json::Map::new();
            ext.insert("source".into(), "auto_detected".into());
            ext.insert("detector_name".into(), "error_handling".into());
            ext.insert("trend".into(), "stable".into());

            let node = KnowledgeNode {
                id: NodeId(0),
                branch_id: BranchId::from("main"),
                nature: KnowledgeNature::Convention,
                weight: KnowledgeWeight::Strong,
                confidence: 0.9,
                adoption_count: 9,
                total_count: 10,
                description: "Uses thiserror for error handling (Rust)".to_owned(),
                ext_data: Some(serde_json::Value::Object(ext)),
            };
            repo.insert(&node).unwrap();
            seshat_graph::rebuild_fts_index(&root.conn).unwrap();
        }

        let server = McpServer::new(ServerConfig::default(), root, HashMap::new());

        let result = server.query_convention(Parameters(QueryConventionRequest {
            topic: "error".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_convention");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert!(parsed["data"]["conventions"].is_array());
        assert!(!parsed["data"]["conventions"].as_array().unwrap().is_empty());
        assert_eq!(parsed["metadata"]["search_type"], "fts5");
    }

    #[test]
    fn query_convention_tool_empty_topic_returns_error() {
        let server = test_server();

        let result = server.query_convention(Parameters(QueryConventionRequest {
            topic: "".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "EMPTY_TOPIC");
    }

    #[test]
    fn record_decision_tool_returns_success_envelope() {
        let server = test_server();

        let result = server.record_decision(Parameters(RecordDecisionRequest {
            description: "Always use Result for fallible operations".to_owned(),
            nature: Some("decision".to_owned()),
            weight: None,
            category: Some("error-handling".to_owned()),
            examples: None,
            reason: Some("Explicit error handling".to_owned()),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "record_decision");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert!(parsed["data"]["id"].as_i64().unwrap() > 0);
        assert_eq!(
            parsed["data"]["description"],
            "Always use Result for fallible operations"
        );
        assert_eq!(parsed["data"]["nature"], "decision");
        assert_eq!(parsed["data"]["weight"], "strong");
        assert!(parsed["metadata"]["node_id"].as_i64().unwrap() > 0);
    }

    #[test]
    fn record_decision_tool_empty_description_returns_error() {
        let server = test_server();

        let result = server.record_decision(Parameters(RecordDecisionRequest {
            description: "".to_owned(),
            nature: None,
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn update_decision_tool_returns_success_envelope() {
        let server = test_server();

        // First record a decision.
        let record_result = server.record_decision(Parameters(RecordDecisionRequest {
            description: "Original decision for update test".to_owned(),
            nature: Some("decision".to_owned()),
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: None,
        }));
        let record_parsed: serde_json::Value = serde_json::from_str(&record_result).unwrap();
        let node_id = record_parsed["data"]["id"].as_i64().unwrap();

        // Update it.
        let result = server.update_decision(Parameters(UpdateDecisionRequest {
            id: node_id,
            description: Some("Updated decision description".to_owned()),
            nature: Some("convention".to_owned()),
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "update_decision");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert_eq!(parsed["data"]["id"], node_id);
        assert_eq!(
            parsed["data"]["description"],
            "Updated decision description"
        );
        assert_eq!(parsed["data"]["nature"], "convention");
        assert_eq!(parsed["data"]["weight"], "strong"); // unchanged default
        assert_eq!(parsed["metadata"]["node_id"], node_id);
    }

    #[test]
    fn update_decision_tool_nonexistent_returns_error() {
        let server = test_server();

        let result = server.update_decision(Parameters(UpdateDecisionRequest {
            id: 99999,
            description: Some("Should fail".to_owned()),
            nature: None,
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "NODE_NOT_FOUND");
    }

    #[test]
    fn remove_decision_tool_returns_success_envelope() {
        let server = test_server();

        // First record a decision.
        let record_result = server.record_decision(Parameters(RecordDecisionRequest {
            description: "Decision to be removed".to_owned(),
            nature: Some("decision".to_owned()),
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: None,
        }));
        let record_parsed: serde_json::Value = serde_json::from_str(&record_result).unwrap();
        let node_id = record_parsed["data"]["id"].as_i64().unwrap();

        // Remove it.
        let result = server.remove_decision(Parameters(RemoveDecisionRequest {
            id: node_id,
            reason: "No longer relevant".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "remove_decision");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert_eq!(parsed["data"]["id"], node_id);
        assert!(
            parsed["data"]["message"]
                .as_str()
                .unwrap()
                .contains("removed successfully")
        );
        assert_eq!(parsed["metadata"]["node_id"], node_id);
    }

    #[test]
    fn remove_decision_tool_empty_reason_returns_error() {
        let server = test_server();

        // First record a decision.
        let record_result = server.record_decision(Parameters(RecordDecisionRequest {
            description: "Decision for empty reason test".to_owned(),
            nature: Some("decision".to_owned()),
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: None,
        }));
        let record_parsed: serde_json::Value = serde_json::from_str(&record_result).unwrap();
        let node_id = record_parsed["data"]["id"].as_i64().unwrap();

        let result = server.remove_decision(Parameters(RemoveDecisionRequest {
            id: node_id,
            reason: "".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn scope_routes_to_submodule_connection() {
        let root = test_root();

        // Create a submodule connection.
        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(ServerConfig::default(), root, submodules);

        // Query with explicit scope targeting the submodule.
        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: Some("vendor/libfoo".to_owned()),
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "vendor/libfoo");
        assert_eq!(parsed["branch"], "develop");
        assert_eq!(parsed["scope"], "vendor/libfoo");
    }

    #[test]
    fn unknown_scope_returns_error() {
        let server = test_server();

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: Some("nonexistent".to_owned()),
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "UNKNOWN_SCOPE");
    }

    #[test]
    fn empty_submodules_backward_compatible() {
        // Single-project mode: empty submodules HashMap.
        let server = test_server();

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["scope"], "root");
    }

    #[test]
    fn file_path_auto_routes_to_submodule() {
        let root = test_root();

        // Create a submodule connection.
        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(ServerConfig::default(), root, submodules);

        // Query with file_path pointing into the submodule (no explicit scope).
        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: Some("vendor/libfoo/src/lib.rs".to_owned()),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "vendor/libfoo");
        assert_eq!(parsed["branch"], "develop");
        assert_eq!(parsed["scope"], "vendor/libfoo");
    }

    #[test]
    fn file_path_in_root_stays_root() {
        let root = test_root();

        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(ServerConfig::default(), root, submodules);

        // file_path in root project — should stay on root connection.
        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: Some("src/main.rs".to_owned()),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
    }

    #[test]
    fn file_path_with_leading_dot_slash_normalized() {
        let root = test_root();

        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(ServerConfig::default(), root, submodules);

        // file_path with leading `./` should be normalized and still match.
        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: Some("./vendor/libfoo/src/lib.rs".to_owned()),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "vendor/libfoo");
        assert_eq!(parsed["scope"], "vendor/libfoo");
    }

    #[test]
    fn record_decision_with_file_path_routes_to_submodule() {
        let root = test_root();

        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(ServerConfig::default(), root, submodules);

        // record_decision with file_path pointing into submodule.
        let result = server.record_decision(Parameters(RecordDecisionRequest {
            description: "Use snake_case in libfoo".to_owned(),
            nature: None,
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: Some("vendor/libfoo/src/naming.rs".to_owned()),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "vendor/libfoo");
        assert_eq!(parsed["branch"], "develop");
        assert_eq!(parsed["scope"], "vendor/libfoo");
        assert!(parsed["data"]["id"].as_i64().unwrap() > 0);
    }
}
