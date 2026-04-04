//! MCP server implementation using `rmcp`.
//!
//! `McpServer` is the main entry point. It registers tools with rmcp,
//! starts the requested transports, and handles graceful shutdown.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use seshat_core::ServerConfig;

use serde::Serialize;

use crate::call_logger::{self, CallLogEntry, CallLogger};
use crate::envelope::{ErrorCode, ErrorEnvelope};
use crate::scope::{self, ProjectConnection};
use crate::tools::project_context::{self, ProjectContextRequest};
use crate::tools::query_convention::{self, QueryConventionRequest};
use crate::tools::record_decision::{self, RecordDecisionRequest};
use crate::tools::remove_decision::{self, RemoveDecisionRequest};
use crate::tools::update_decision::{self, UpdateDecisionRequest};

// ── Common request trait ─────────────────────────────────────

/// Shared routing fields present on every MCP tool request.
///
/// Implemented by all five request types so the validation/scope-resolution/
/// logging boilerplate can live in a single generic helper.
trait ToolRequest: Serialize {
    fn repo(&self) -> Option<&str>;
    fn scope(&self) -> Option<&str>;
    fn file_path(&self) -> Option<&str>;
}

macro_rules! impl_tool_request {
    ($ty:ty) => {
        impl ToolRequest for $ty {
            fn repo(&self) -> Option<&str> {
                self.repo.as_deref()
            }
            fn scope(&self) -> Option<&str> {
                self.scope.as_deref()
            }
            fn file_path(&self) -> Option<&str> {
                self.file_path.as_deref()
            }
        }
    };
}

impl_tool_request!(ProjectContextRequest);
impl_tool_request!(QueryConventionRequest);
impl_tool_request!(RecordDecisionRequest);
impl_tool_request!(UpdateDecisionRequest);
impl_tool_request!(RemoveDecisionRequest);

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
    /// Optional call logger for recording MCP tool calls to a JSONL file.
    call_logger: Option<Arc<CallLogger>>,
}

impl McpServer {
    /// Create a new `McpServer` with root + submodule connections.
    ///
    /// For single-project mode, pass an empty `HashMap` for `submodules`.
    /// When `call_log_path` is `Some`, every tool call is logged to the
    /// specified JSONL file. When `None`, no logging overhead is incurred.
    pub fn new(
        config: ServerConfig,
        root: ProjectConnection,
        submodules: HashMap<String, ProjectConnection>,
        call_log_path: Option<PathBuf>,
    ) -> Self {
        let mut mount_paths: Vec<String> = submodules.keys().cloned().collect();
        mount_paths.sort();

        let call_logger = call_log_path.and_then(|path| match CallLogger::new(&path) {
            Ok(logger) => {
                tracing::info!(
                    path = %path.display(),
                    session = logger.session_id(),
                    "Call logger enabled"
                );
                Some(Arc::new(logger))
            }
            Err(err) => {
                tracing::warn!("Failed to create call logger at {}: {err}", path.display());
                None
            }
        });

        Self {
            tool_router: Self::tool_router(),
            config,
            root,
            submodules,
            mount_paths,
            call_logger,
        }
    }

    /// Validate the `repo` parameter if present.
    ///
    /// If `req.repo` is `Some` and doesn't match `self.root.name`
    /// (case-insensitive), returns an error envelope string. Otherwise `Ok(())`.
    fn validate_repo(&self, tool: &str, repo: Option<&str>) -> Result<(), String> {
        if let Some(req_repo) = repo {
            if !req_repo.eq_ignore_ascii_case(&self.root.name) {
                let envelope = ErrorEnvelope::new(
                    tool,
                    &self.root.name,
                    ErrorCode::RepoNotFound,
                    format!(
                        "Repository '{}' not found. The loaded project is '{}'",
                        req_repo, self.root.name
                    ),
                    format!(
                        "Use repo='{}' or omit the repo parameter for auto-detection",
                        self.root.name
                    ),
                );
                return Err(serde_json::to_string(&envelope).unwrap_or_default());
            }
        }
        Ok(())
    }

    /// Resolve which `ProjectConnection` should handle a request.
    ///
    /// Delegates to [`scope::resolve_scope`] and converts errors to JSON
    /// error envelope strings.
    fn resolve_scope(
        &self,
        tool: &str,
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

    /// Validate repo + resolve scope + call handler + log the call.
    ///
    /// Captures the full validate → resolve → execute → log pipeline that
    /// every tool handler shares. The `handler` closure receives the resolved
    /// `ProjectConnection` and scope name and returns the JSON response string.
    fn execute_tool<R: ToolRequest>(
        &self,
        tool: &str,
        req: R,
        handler: impl FnOnce(&ProjectConnection, &str, R) -> String,
    ) -> String {
        // Snapshot input for logging *before* req is moved into the handler.
        let log_ctx = self.call_logger.as_ref().map(|_| {
            (
                serde_json::to_value(&req).unwrap_or_default(),
                Instant::now(),
            )
        });

        let response = (|| {
            self.validate_repo(tool, req.repo())?;
            let (pc, scope_name) = self.resolve_scope(tool, req.scope(), req.file_path())?;
            Ok(handler(pc, &scope_name, req))
        })()
        .unwrap_or_else(|e: String| e);

        if let Some((input, start)) = log_ctx {
            self.log_tool_call(tool, input, start, &response);
        }
        response
    }

    /// Log a tool call to the JSONL call log (if enabled).
    ///
    /// Constructs a [`CallLogEntry`] from the given parameters and writes it
    /// to the log file. Write failures are logged as warnings and do **not**
    /// propagate to the caller. No-op when logging is disabled.
    fn log_tool_call(
        &self,
        tool: &str,
        input: serde_json::Value,
        start: Instant,
        response_json: &str,
    ) {
        let logger = match &self.call_logger {
            Some(l) => l,
            None => return,
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let parsed: serde_json::Value = serde_json::from_str(response_json).unwrap_or_default();

        let is_error = parsed.get("status").and_then(|v| v.as_str()) == Some("error");

        let result = if is_error {
            None
        } else {
            let data = parsed.get("data").cloned().unwrap_or_default();
            Some(match tool {
                "query_project_context" => call_logger::project_context_result(&data),
                "query_convention" => call_logger::query_convention_result(&data),
                "record_decision" | "update_decision" | "remove_decision" => {
                    let node_id = parsed
                        .get("metadata")
                        .and_then(|m| m.get("node_id"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    call_logger::decision_result(node_id)
                }
                _ => serde_json::Value::Null,
            })
        };

        let error_code = if is_error {
            parsed
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_owned())
        } else {
            None
        };

        let entry = CallLogEntry {
            ts: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            session: logger.session_id().to_owned(),
            seq: logger.next_seq(),
            tool: tool.to_owned(),
            input,
            duration_ms,
            status: if is_error { "error" } else { "ok" }.to_owned(),
            result,
            error_code,
        };

        if let Err(err) = logger.log_call(&entry) {
            tracing::warn!("call log write failed: {err}");
        }
    }
}

#[tool_router]
impl McpServer {
    #[tool(
        description = "Get a high-level overview of the project: languages, dependency domains, convention confidence, and golden files (top convention-compliant exemplars). Call this FIRST before writing any code. Use the optional focus_area parameter (e.g. 'logging', 'testing') to narrow results to a specific domain. Follow up with query_convention to deep-dive into specific conventions. Use the optional file_path parameter (e.g. 'src/components/Button.tsx') for automatic submodule scope detection."
    )]
    fn query_project_context(&self, Parameters(req): Parameters<ProjectContextRequest>) -> String {
        const TOOL: &str = "query_project_context";
        tracing::info!(tool = TOOL, focus_area = ?req.focus_area, "Handling query_project_context");

        self.execute_tool(TOOL, req, |pc, scope_name, req| {
            project_context::handle(&pc.conn, &pc.name, &pc.branch, scope_name, req)
        })
    }

    #[tool(
        description = "Search conventions by topic (e.g. 'error handling', 'logging', 'naming'). Returns matching conventions with adoption rate, trend (rising/stable/declining), confidence, and code examples. Use AFTER query_project_context to deep-dive before generating code. Covers both auto-detected patterns and user-recorded decisions. The required topic parameter is searched via full-text search on convention descriptions. Use the optional file_path parameter for automatic submodule scope detection."
    )]
    fn query_convention(&self, Parameters(req): Parameters<QueryConventionRequest>) -> String {
        const TOOL: &str = "query_convention";
        tracing::info!(tool = TOOL, topic = %req.topic, "Handling query_convention");

        self.execute_tool(TOOL, req, |pc, scope_name, req| {
            query_convention::handle(&pc.conn, &pc.name, &pc.branch, scope_name, req)
        })
    }

    #[tool(
        description = "Record a convention, architectural decision, or coding rule that auto-detection missed. Use AFTER work when you discover a pattern worth preserving — e.g. wrapper facades, team style agreements, or architectural constraints. Required: description. Optional: nature ('decision'|'convention'|'preference'), weight ('rule'|'strong'), category, examples [{file, line, end_line, snippet}], reason, file_path (for automatic submodule scope detection). Immediately searchable via query_convention. Never overwritten by re-scans."
    )]
    fn record_decision(&self, Parameters(req): Parameters<RecordDecisionRequest>) -> String {
        const TOOL: &str = "record_decision";
        tracing::info!(tool = TOOL, description = %req.description, "Handling record_decision");

        self.execute_tool(TOOL, req, |pc, scope_name, req| {
            record_decision::handle(&pc.conn, &pc.name, &pc.branch, scope_name, req)
        })
    }

    #[tool(
        description = "Update a previously recorded user decision. Use when a convention evolves or needs correction — e.g. changing the description, reclassifying nature/weight, or adding evidence. Required: id (from record_decision response or query_convention results). Optional: description, nature, weight, category, examples, reason, file_path (for automatic submodule scope detection) — only provided fields are changed. Only user-recorded decisions can be updated; auto-detected conventions return NOT_USER_DECISION error."
    )]
    fn update_decision(&self, Parameters(req): Parameters<UpdateDecisionRequest>) -> String {
        const TOOL: &str = "update_decision";
        tracing::info!(tool = TOOL, node_id = req.id, "Handling update_decision");

        self.execute_tool(TOOL, req, |pc, scope_name, req| {
            update_decision::handle(&pc.conn, &pc.name, &pc.branch, scope_name, req)
        })
    }

    #[tool(
        description = "Soft-delete a previously recorded user decision that is no longer relevant or has been superseded. The record is preserved for audit trail but hidden from query_convention and query_project_context results. Required: id (node ID), reason (why it is being removed). Optional: file_path (for automatic submodule scope detection). Only user-recorded decisions can be removed; auto-detected conventions return NOT_USER_DECISION error."
    )]
    fn remove_decision(&self, Parameters(req): Parameters<RemoveDecisionRequest>) -> String {
        const TOOL: &str = "remove_decision";
        tracing::info!(tool = TOOL, node_id = req.id, reason = %req.reason, "Handling remove_decision");

        self.execute_tool(TOOL, req, |pc, scope_name, req| {
            remove_decision::handle(&pc.conn, &pc.name, &pc.branch, scope_name, req)
        })
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
    call_log_path: Option<PathBuf>,
) -> Result<(), crate::McpError> {
    let server = McpServer::new(config, root, submodules, call_log_path);

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
    call_log_path: Option<PathBuf>,
    shutdown: impl std::future::Future<Output = ()>,
    drain_timeout: std::time::Duration,
) -> Result<(), crate::McpError> {
    let server = McpServer::new(config, root, submodules, call_log_path);

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

    use crate::test_helpers::make_conn;

    fn test_root() -> ProjectConnection {
        make_conn("test-project", "main")
    }

    fn test_server() -> McpServer {
        McpServer::new(ServerConfig::default(), test_root(), HashMap::new(), None)
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

        let server = McpServer::new(ServerConfig::default(), root, HashMap::new(), None);

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

        let server = McpServer::new(ServerConfig::default(), root, HashMap::new(), None);

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

        let server = McpServer::new(ServerConfig::default(), root, submodules, None);

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

        let server = McpServer::new(ServerConfig::default(), root, submodules, None);

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

        let server = McpServer::new(ServerConfig::default(), root, submodules, None);

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

        let server = McpServer::new(ServerConfig::default(), root, submodules, None);

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

        let server = McpServer::new(ServerConfig::default(), root, submodules, None);

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

    // ── US-012: repo parameter validation ──────────────────────

    #[test]
    fn wrong_repo_returns_repo_not_found_query_project_context() {
        let server = test_server();

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "query_project_context");
        assert_eq!(parsed["repo"], "test-project");
        assert!(
            parsed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("wrong-project")
        );
        assert!(
            parsed["error"]["suggestion"]
                .as_str()
                .unwrap()
                .contains("test-project")
        );
    }

    #[test]
    fn wrong_repo_returns_repo_not_found_query_convention() {
        let server = test_server();

        let result = server.query_convention(Parameters(QueryConventionRequest {
            topic: "error".to_owned(),
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "query_convention");
    }

    #[test]
    fn wrong_repo_returns_repo_not_found_record_decision() {
        let server = test_server();

        let result = server.record_decision(Parameters(RecordDecisionRequest {
            description: "Some decision".to_owned(),
            nature: None,
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "record_decision");
    }

    #[test]
    fn wrong_repo_returns_repo_not_found_update_decision() {
        let server = test_server();

        let result = server.update_decision(Parameters(UpdateDecisionRequest {
            id: 1,
            description: Some("Updated".to_owned()),
            nature: None,
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "update_decision");
    }

    #[test]
    fn wrong_repo_returns_repo_not_found_remove_decision() {
        let server = test_server();

        let result = server.remove_decision(Parameters(RemoveDecisionRequest {
            id: 1,
            reason: "No longer needed".to_owned(),
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "remove_decision");
    }

    #[test]
    fn correct_repo_passes_validation() {
        let server = test_server();

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: Some("test-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "test-project");
    }

    #[test]
    fn correct_repo_case_insensitive() {
        let server = test_server();

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: Some("Test-Project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["repo"], "test-project");
    }

    #[test]
    fn none_repo_passes_validation() {
        let server = test_server();

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
    }

    // ── US-005: Call logging integration tests ─────────────────

    use std::io::Read;
    use tempfile::TempDir;

    /// Parse all JSONL lines from a file.
    fn read_jsonl(path: &std::path::Path) -> Vec<serde_json::Value> {
        let mut contents = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSON line"))
            .collect()
    }

    #[test]
    fn call_log_tool_call_writes_jsonl_entry() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("call-log.jsonl");

        let server = McpServer::new(
            ServerConfig::default(),
            test_root(),
            HashMap::new(),
            Some(log_path.clone()),
        );

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        // Tool should succeed.
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");

        // Log file should exist with one JSONL line.
        assert!(log_path.exists(), "log file should be created");
        let entries = read_jsonl(&log_path);
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry["tool"], "query_project_context");
        assert_eq!(entry["status"], "ok");
        assert_eq!(entry["seq"], 0);

        // ts is a valid ISO 8601 string.
        let ts = entry["ts"].as_str().unwrap();
        assert!(ts.ends_with('Z'), "timestamp should end with Z");
        assert!(ts.contains('T'), "timestamp should contain T separator");

        // Session is 8 hex chars.
        let session = entry["session"].as_str().unwrap();
        assert_eq!(session.len(), 8);
        assert!(session.chars().all(|c| c.is_ascii_hexdigit()));

        // Input captured.
        assert!(entry["input"].is_object(), "input should be an object");

        // Result summary matches query_project_context schema.
        let result_summary = &entry["result"];
        assert!(result_summary.get("language_count").is_some());
        assert!(result_summary.get("convention_count").is_some());
        assert!(result_summary.get("golden_file_count").is_some());

        // Duration is a non-negative number.
        assert!(entry["duration_ms"].as_u64().is_some());

        // error_code should be absent on success.
        assert!(entry.get("error_code").is_none());
    }

    #[test]
    fn call_log_disabled_no_file_created() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("should-not-exist.jsonl");

        let server = McpServer::new(
            ServerConfig::default(),
            test_root(),
            HashMap::new(),
            None, // logging disabled
        );

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");

        // Log file must NOT exist.
        assert!(
            !log_path.exists(),
            "log file should not be created when logging is disabled"
        );
    }

    #[test]
    fn call_log_multiple_calls_sequential_seq() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("call-log.jsonl");

        let server = McpServer::new(
            ServerConfig::default(),
            test_root(),
            HashMap::new(),
            Some(log_path.clone()),
        );

        // Call three different tools.
        server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        server.query_convention(Parameters(QueryConventionRequest {
            topic: "naming".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
        }));

        server.record_decision(Parameters(RecordDecisionRequest {
            description: "Use snake_case for Rust".to_owned(),
            nature: Some("convention".to_owned()),
            weight: None,
            category: None,
            examples: None,
            reason: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        // All three should be logged.
        let entries = read_jsonl(&log_path);
        assert_eq!(entries.len(), 3, "should have 3 log entries");

        // Sequential seq values: 0, 1, 2.
        assert_eq!(entries[0]["seq"], 0);
        assert_eq!(entries[1]["seq"], 1);
        assert_eq!(entries[2]["seq"], 2);

        // All share the same session.
        let session = entries[0]["session"].as_str().unwrap();
        assert_eq!(entries[1]["session"], session);
        assert_eq!(entries[2]["session"], session);

        // Tool names.
        assert_eq!(entries[0]["tool"], "query_project_context");
        assert_eq!(entries[1]["tool"], "query_convention");
        assert_eq!(entries[2]["tool"], "record_decision");
    }

    #[test]
    fn call_log_error_case_logs_status_error_and_error_code() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("call-log.jsonl");

        let server = McpServer::new(
            ServerConfig::default(),
            test_root(),
            HashMap::new(),
            Some(log_path.clone()),
        );

        // query_convention with empty topic triggers EMPTY_TOPIC error.
        let result = server.query_convention(Parameters(QueryConventionRequest {
            topic: "".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "EMPTY_TOPIC");

        // Verify the log entry.
        let entries = read_jsonl(&log_path);
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry["tool"], "query_convention");
        assert_eq!(entry["status"], "error");
        assert_eq!(entry["error_code"], "EMPTY_TOPIC");

        // result should be absent on error.
        assert!(
            entry.get("result").is_none(),
            "result should be absent on error"
        );

        // Input should still be captured.
        assert_eq!(entry["input"]["topic"], "");

        // seq should be 0 (first call).
        assert_eq!(entry["seq"], 0);
    }

    #[test]
    fn call_log_directory_created_via_create_dir_all() {
        let dir = TempDir::new().unwrap();
        let log_path = dir
            .path()
            .join("nested")
            .join("dirs")
            .join("call-log.jsonl");

        assert!(
            !log_path.parent().unwrap().exists(),
            "parent directory should not exist yet"
        );

        let server = McpServer::new(
            ServerConfig::default(),
            test_root(),
            HashMap::new(),
            Some(log_path.clone()),
        );

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");

        // Log file and parent directories should now exist.
        assert!(
            log_path.exists(),
            "log file should be created in nested directory"
        );

        let entries = read_jsonl(&log_path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["tool"], "query_project_context");
        assert_eq!(entries[0]["status"], "ok");
    }
}
