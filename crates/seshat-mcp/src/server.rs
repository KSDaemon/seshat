//! MCP server implementation using `rmcp`.
//!
//! `McpServer` is the main entry point. It registers tools with rmcp,
//! starts the requested transports, and handles graceful shutdown.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use seshat_core::ServerConfig;

use serde::Serialize;

use crate::call_logger::{self, CallLogEntry, CallLogger};
use crate::envelope::{ErrorCode, ErrorEnvelope};
use crate::scope::{self, ProjectConnection};
use crate::tools::diff_impact::{self, MapDiffImpactRequest};
use crate::tools::project_context::{self, ProjectContextRequest};
use crate::tools::query_code_pattern::{self, QueryCodePatternRequest};
use crate::tools::query_convention::{self, QueryConventionRequest};
use crate::tools::query_dependencies::{self, QueryDependenciesRequest};
use crate::tools::record_decision::{self, RecordDecisionRequest};
use crate::tools::remove_decision::{self, RemoveDecisionRequest};
use crate::tools::update_decision::{self, UpdateDecisionRequest};
use crate::tools::validate_approach::{self, ValidateApproachRequest};

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

impl_tool_request!(MapDiffImpactRequest);
impl_tool_request!(ProjectContextRequest);
impl_tool_request!(QueryCodePatternRequest);
impl_tool_request!(QueryConventionRequest);
impl_tool_request!(QueryDependenciesRequest);
impl_tool_request!(RecordDecisionRequest);
impl_tool_request!(UpdateDecisionRequest);
impl_tool_request!(RemoveDecisionRequest);
impl_tool_request!(ValidateApproachRequest);

/// Thread-safe scan state tracking for auto-scan on first `seshat serve`.
///
/// Tracks whether a background scan is in progress, completed, or failed,
/// and provides a synchronous `wait_for_scan()` using `Condvar` so that
/// MCP tool calls can block until the scan completes before proceeding.
///
/// Must be `Send + Sync + Clone` (held by `McpServer` which is `Clone`).
#[derive(Debug, Clone)]
pub struct ScanState {
    inner: Arc<std::sync::Mutex<ScanStateInner>>,
    condvar: Arc<std::sync::Condvar>,
    /// Tracks whether the first success response metadata has been sent.
    first_response_sent: Arc<AtomicBool>,
}

#[derive(Debug)]
enum ScanStateInner {
    NotNeeded,
    InProgress,
    Complete { auto_scanned: bool },
    Failed { error_message: String },
}

impl ScanState {
    /// Constructor for the case where a DB already exists (no auto-scan needed).
    pub fn not_needed() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(ScanStateInner::NotNeeded)),
            condvar: Arc::new(std::sync::Condvar::new()),
            first_response_sent: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Constructor for the case where an auto-scan will run.
    pub fn in_progress() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(ScanStateInner::InProgress)),
            condvar: Arc::new(std::sync::Condvar::new()),
            first_response_sent: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Transition from InProgress → Complete, notifying all waiters.
    pub fn mark_complete(&self) {
        let mut inner = self.inner.lock().expect("ScanState lock");
        *inner = ScanStateInner::Complete { auto_scanned: true };
        self.condvar.notify_all();
    }

    /// Transition from InProgress → Failed, notifying all waiters.
    pub fn mark_failed(&self, msg: String) {
        let mut inner = self.inner.lock().expect("ScanState lock");
        *inner = ScanStateInner::Failed { error_message: msg };
        self.condvar.notify_all();
    }

    /// Synchronously wait for the scan to complete.
    /// Returns immediately if scan is not needed, already complete, or failed.
    /// Uses `Condvar` so this is safe to call from sync contexts.
    pub fn wait_for_scan(&self) {
        let mut inner = self.inner.lock().expect("ScanState lock");
        while matches!(&*inner, ScanStateInner::InProgress) {
            inner = self.condvar.wait(inner).expect("Condvar wait");
        }
    }

    /// Returns `true` if the auto-scan completed successfully.
    /// Returns `false` if scan is not needed, still in progress, or failed.
    pub fn auto_scanned(&self) -> bool {
        let inner = self.inner.lock().expect("ScanState lock");
        matches!(&*inner, ScanStateInner::Complete { auto_scanned: true })
    }

    /// Returns `true` if an auto-scan was attempted (completed or failed).
    pub fn scan_attempted(&self) -> bool {
        let inner = self.inner.lock().expect("ScanState lock");
        matches!(
            &*inner,
            ScanStateInner::Complete { .. } | ScanStateInner::Failed { .. }
        )
    }

    /// Returns `true` if this was the first scan ever for this project (auto-scanned successfully).
    pub fn is_first_run(&self) -> bool {
        self.auto_scanned()
    }

    /// Returns the error message if the scan failed, `None` otherwise.
    pub fn error_message(&self) -> Option<String> {
        let inner = self.inner.lock().expect("ScanState lock");
        match &*inner {
            ScanStateInner::Failed { error_message } => Some(error_message.clone()),
            _ => None,
        }
    }

    /// Atomically check and set the first-response-sent flag.
    /// Returns `true` if this was the first call (flag was false, now set to true).
    pub fn take_first_response_flag(&self) -> bool {
        !self.first_response_sent.swap(true, Ordering::Relaxed)
    }
}

/// The Seshat MCP server.
///
/// Registers tools via `rmcp`'s `#[tool_router]` macro and implements
/// `ServerHandler` for protocol compliance. Holds root + submodule database
/// connections so tools can route queries to the correct knowledge graph.
#[derive(Debug, Clone)]
pub struct McpServer {
    #[allow(dead_code)]
    config: ServerConfig,
    /// Root project connection (always present).
    root: ProjectConnection,
    /// Submodule connections keyed by mount path (e.g. `"vendor/libfoo"`).
    submodules: HashMap<String, ProjectConnection>,
    /// Submodule mount paths for scope resolution (resolve_scope finds longest prefix match).
    mount_paths: Vec<String>,
    /// Optional call logger for recording MCP tool calls to a JSONL file.
    call_logger: Option<Arc<CallLogger>>,
    /// Optional embedding provider for vector/semantic search in `query_code_pattern`.
    /// When `Some`, `query_code_pattern` performs cosine similarity search in addition
    /// to keyword matching. When `None`, only keyword (FTS5) search is used.
    embedding_provider: Option<Arc<dyn seshat_embedding::EmbeddingProvider>>,
    /// Scan state for auto-scan on first serve.
    scan_state: ScanState,
    /// Whether a background diff-based sync is currently in progress.
    /// Set by `serve.rs` around `background_sync()` calls.
    sync_in_progress: Arc<AtomicBool>,
    /// Whether this project uses snapshot-based branch switching (ADR-14).
    snapshot_based: bool,
    /// Whether the current branch is a detached HEAD (commit hash).
    detached_head: bool,
    /// Filesystem path to the git repository root.
    /// Used as the default for `map_diff_impact` when no `repo_path` is supplied.
    project_root: PathBuf,
}

impl McpServer {
    /// Create a new `McpServer` with root + submodule connections.
    ///
    /// For single-project mode, pass an empty `HashMap` for `submodules`.
    /// When `call_log_path` is `Some`, every tool call is logged to the
    /// specified JSONL file. When `None`, no logging overhead is incurred.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: ServerConfig,
        root: ProjectConnection,
        submodules: HashMap<String, ProjectConnection>,
        call_log_path: Option<PathBuf>,
        scan_state: ScanState,
        sync_in_progress: Arc<AtomicBool>,
        snapshot_based: bool,
        detached_head: bool,
        project_root: PathBuf,
    ) -> Self {
        Self::with_embedding(
            config,
            root,
            submodules,
            call_log_path,
            None,
            scan_state,
            sync_in_progress,
            snapshot_based,
            detached_head,
            project_root,
        )
    }

    /// Create a new `McpServer` with an optional embedding provider for
    /// vector/semantic search in `query_code_pattern`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_embedding(
        config: ServerConfig,
        root: ProjectConnection,
        submodules: HashMap<String, ProjectConnection>,
        call_log_path: Option<PathBuf>,
        embedding_provider: Option<Arc<dyn seshat_embedding::EmbeddingProvider>>,
        scan_state: ScanState,
        sync_in_progress: Arc<AtomicBool>,
        snapshot_based: bool,
        detached_head: bool,
        project_root: PathBuf,
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
            config,
            root,
            submodules,
            mount_paths,
            call_logger,
            embedding_provider,
            scan_state,
            sync_in_progress,
            snapshot_based,
            detached_head,
            project_root,
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
        handler: impl FnOnce(&ProjectConnection, R) -> String,
    ) -> String {
        // Wait for auto-scan to complete before processing any tool call.
        self.scan_state.wait_for_scan();
        if let Some(err_msg) = self.scan_state.error_message() {
            let envelope = ErrorEnvelope::new(
                tool,
                &self.root.name,
                ErrorCode::AutoScanFailed,
                format!("Auto-scan failed: {err_msg}"),
                "Run: seshat scan --verbose".to_string(),
            );
            return serde_json::to_string(&envelope).unwrap_or_default();
        }

        // Snapshot input for logging *before* req is moved into the handler.
        let log_ctx = self.call_logger.as_ref().map(|_| {
            (
                serde_json::to_value(&req).unwrap_or_default(),
                Instant::now(),
            )
        });

        let mut response = (|| {
            self.validate_repo(tool, req.repo())?;
            let (pc, _scope_name) = self.resolve_scope(tool, req.scope(), req.file_path())?;
            Ok(handler(pc, req))
        })()
        .unwrap_or_else(|e: String| e);

        let syncing = self.sync_in_progress.load(Ordering::Relaxed);
        if syncing || self.detached_head {
            if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&response) {
                if parsed.get("status").and_then(|v| v.as_str()) == Some("success") {
                    let meta = parsed.get_mut("metadata").and_then(|m| m.as_object_mut());
                    if let Some(meta_obj) = meta {
                        let reserved = meta_obj
                            .entry("_metadata")
                            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                        if let Some(reserved_obj) = reserved.as_object_mut() {
                            if syncing {
                                reserved_obj
                                    .insert("syncing".to_owned(), serde_json::Value::Bool(true));
                                reserved_obj.insert(
                                    "snapshot_based".to_owned(),
                                    serde_json::Value::Bool(self.snapshot_based),
                                );
                            }
                            if self.detached_head {
                                reserved_obj.insert(
                                    "detached_head".to_owned(),
                                    serde_json::Value::Bool(true),
                                );
                            }
                        }
                    }
                    response = serde_json::to_string(&parsed).unwrap_or(response);
                }
            }
        }

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
                "query_code_pattern" => call_logger::code_pattern_result(&data),
                "query_convention" => call_logger::query_convention_result(&data),
                "query_dependencies" => call_logger::dependencies_result(&data),
                "validate_approach" => call_logger::validate_approach_result(&data),
                "map_diff_impact" => call_logger::diff_impact_result(&data),
                "record_decision" | "update_decision" | "remove_decision" => {
                    let description_hash = parsed
                        .get("metadata")
                        .and_then(|m| m.get("description_hash"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    call_logger::decision_result(&description_hash)
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

        self.execute_tool(TOOL, req, |pc, req| {
            project_context::handle(&pc.conn, &pc.name, &pc.branch, req)
        })
    }

    #[tool(
        description = "Search conventions by topic (e.g. 'error handling', 'logging', 'naming'). Returns matching conventions with adoption rate, trend (rising/stable/declining), confidence, and code examples. Use AFTER query_project_context to deep-dive before generating code. Covers both auto-detected patterns and user-recorded decisions. The required topic parameter is searched via full-text search on convention descriptions. Use the optional file_path parameter for automatic submodule scope detection."
    )]
    fn query_convention(&self, Parameters(req): Parameters<QueryConventionRequest>) -> String {
        const TOOL: &str = "query_convention";
        tracing::info!(tool = TOOL, topic = %req.topic, "Handling query_convention");

        self.execute_tool(TOOL, req, |pc, req| {
            query_convention::handle(&pc.conn, &pc.name, &pc.branch, req)
        })
    }

    #[tool(
        description = "Search for existing code patterns (functions, types, exports) by name in the project's IR. Returns scored results (exact > prefix > contains) plus related conventions. Use BEFORE writing new code to find existing implementations you can reuse or extend. Supports optional kind filter ('function', 'type', 'export', 'all'). Follow up with query_dependencies on matched files to understand blast radius, or validate_approach to check convention compliance."
    )]
    fn query_code_pattern(&self, Parameters(req): Parameters<QueryCodePatternRequest>) -> String {
        const TOOL: &str = "query_code_pattern";
        tracing::info!(tool = TOOL, query = %req.query, kind = ?req.kind, "Handling query_code_pattern");

        let provider = self.embedding_provider.clone();
        self.execute_tool(TOOL, req, |pc, req| {
            query_code_pattern::handle(&pc.conn, &pc.name, &pc.branch, req, provider.as_deref())
        })
    }

    #[tool(
        description = "Analyze import/export relationships for a file: returns dependencies (files it imports from), dependents (files that import from it, optionally transitive), external package dependencies, and a blast radius classification (low/medium/high). Use AFTER query_code_pattern to understand the impact of changes to matched files. The path parameter is the file path relative to the project root. The optional depth parameter controls transitive dependents traversal: 1 = direct only, 2..=10 = breadth-first transitive expansion; default is 3 (1st-, 2nd-, and 3rd-order dependents). Follow up with validate_approach to check convention compliance before making changes."
    )]
    fn query_dependencies(&self, Parameters(req): Parameters<QueryDependenciesRequest>) -> String {
        const TOOL: &str = "query_dependencies";
        tracing::info!(tool = TOOL, path = %req.path, "Handling query_dependencies");

        self.execute_tool(TOOL, req, |pc, req| {
            query_dependencies::handle(&pc.conn, &pc.name, &pc.branch, req)
        })
    }

    #[tool(
        description = "Record a convention, architectural decision, or coding rule that auto-detection missed. Use AFTER work when you discover a pattern worth preserving — e.g. wrapper facades, team style agreements, or architectural constraints. Required: description. Optional: nature ('decision'|'convention'|'preference'), weight ('rule'|'strong'), category, examples [{file, line, end_line, snippet}], reason, file_path (for automatic submodule scope detection). Immediately searchable via query_convention. Never overwritten by re-scans."
    )]
    fn record_decision(&self, Parameters(req): Parameters<RecordDecisionRequest>) -> String {
        const TOOL: &str = "record_decision";
        tracing::info!(tool = TOOL, description = %req.description, "Handling record_decision");

        self.execute_tool(TOOL, req, |pc, req| {
            record_decision::handle(&pc.conn, &pc.name, &pc.branch, req)
        })
    }

    #[tool(
        description = "Update a previously recorded user decision. Use when a convention evolves or needs correction — e.g. changing the description, reclassifying nature/weight, or adding evidence. Required: description_hash (from record_decision response or DecisionEntry in validate_approach results). Optional: description, nature, weight, category, examples, reason, file_path (for automatic submodule scope detection) — only provided fields are changed. Returns DECISION_NOT_FOUND if no decision row matches the hash."
    )]
    fn update_decision(&self, Parameters(req): Parameters<UpdateDecisionRequest>) -> String {
        const TOOL: &str = "update_decision";
        tracing::info!(
            tool = TOOL,
            description_hash = %req.description_hash,
            "Handling update_decision"
        );

        self.execute_tool(TOOL, req, |pc, req| {
            update_decision::handle(&pc.conn, &pc.name, &pc.branch, req)
        })
    }

    #[tool(
        description = "Hard-delete a previously recorded user decision that is no longer relevant or has been superseded. The decision row is fully removed from the project-wide decisions table. Required: description_hash (from record_decision response or DecisionEntry in validate_approach results), reason (why it is being removed; logged for audit). Optional: file_path (for automatic submodule scope detection). Returns DECISION_NOT_FOUND if no decision row matches the hash."
    )]
    fn remove_decision(&self, Parameters(req): Parameters<RemoveDecisionRequest>) -> String {
        const TOOL: &str = "remove_decision";
        tracing::info!(
            tool = TOOL,
            description_hash = %req.description_hash,
            reason = %req.reason,
            "Handling remove_decision"
        );

        self.execute_tool(TOOL, req, |pc, req| {
            remove_decision::handle(&pc.conn, &pc.name, &pc.branch, req)
        })
    }

    #[tool(
        description = "Validate a proposed approach against project rules, conventions, and existing code patterns. Returns a graduated response with verdict (approved/info_only/warnings_found/rules_violated), evidence gating (ready: true/false), and actionable suggestions. Checks: rules (must-fix violations), contradictions, duplicate code patterns, conventions, user decisions, and observations. Use BEFORE writing code to verify your approach aligns with the project's established patterns. Follow up with query_code_pattern to explore duplicates or query_dependencies to understand blast radius."
    )]
    fn validate_approach(&self, Parameters(req): Parameters<ValidateApproachRequest>) -> String {
        const TOOL: &str = "validate_approach";
        tracing::info!(tool = TOOL, description = %req.description, "Handling validate_approach");

        self.execute_tool(TOOL, req, |pc, req| {
            validate_approach::handle(&pc.conn, &pc.name, &pc.branch, req)
        })
    }

    #[tool(
        description = "Map uncommitted git changes to affected symbols, dependents, blast radius, and convention risks in a single call. Call BEFORE committing or during code review to understand the impact of your changes. Returns changed files with statuses (modified/added/deleted/untracked/conflicted), symbols affected by changes with dependent counts and blast radius (low/medium/high), convention risks where changed files are evidence for conventions, an aggregated blast radius summary, and actionable next steps. Parameters: staged_only (bool, default false — show only staged changes), base (optional commitish, mutually exclusive with staged_only), repo_path (optional — path to git repo root; defaults to the project root the server was started in)."
    )]
    fn map_diff_impact(&self, Parameters(req): Parameters<MapDiffImpactRequest>) -> String {
        const TOOL: &str = "map_diff_impact";
        tracing::info!(tool = TOOL, staged_only = ?req.staged_only, base = ?req.base, repo_path = ?req.repo_path, "Handling map_diff_impact");

        let project_root = self.project_root.clone();
        self.execute_tool(TOOL, req, move |pc, req| {
            diff_impact::handle(&pc.conn, &pc.name, &pc.branch, req, &project_root)
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
              (e.g. 'error handling', 'logging', 'naming'). \
              Use query_code_pattern to find existing implementations you can reuse or extend. \
               Use validate_approach to verify your proposed changes align with project rules and conventions.\n\
               2. WRITE code following the discovered conventions. \
               Use query_dependencies to understand the blast radius of changes to specific files. \
               Use map_diff_impact before committing or during code review to see how your \
               uncommitted changes affect symbols, dependents, and conventions.\n\
               3. AFTER work: if you discover a new convention not already captured \
              (e.g. a wrapper/facade pattern, an architectural decision, or a team style agreement), \
              call record_decision to persist it for future sessions.\n\
              \n\
              Use update_decision to correct or evolve a previously recorded decision, \
              and remove_decision to retire decisions that no longer apply.\n\
              \n\
              Scoping (monorepo / submodules):\n\
              All tools accept an optional 'scope' parameter and an optional 'file_path' parameter. \
              Scope resolution priority: explicit scope > file_path prefix match > root (default).\n\
              - Omit scope and file_path to query the root project.\n\
              - Pass file_path (relative to project root) to auto-route to the correct submodule \
              (e.g. file_path='vendor/libfoo/src/lib.rs' automatically targets the vendor/libfoo submodule).\n\
              - Pass scope explicitly as the submodule mount path relative to the project root \
              (e.g. scope='vendor/libfoo'). Short names (e.g. scope='libfoo') work when unambiguous.\n\
              - Use scope='root' to force querying the root project even when file_path points to a submodule.",
        )
    }
}

/// Start the MCP server on stdio transport.
///
/// This function blocks until the server is shut down (e.g., via Ctrl+C
/// or when the client closes the connection).
#[allow(clippy::too_many_arguments)]
pub async fn start_stdio(
    config: ServerConfig,
    root: ProjectConnection,
    submodules: HashMap<String, ProjectConnection>,
    call_log_path: Option<PathBuf>,
    embedding_provider: Option<Arc<dyn seshat_embedding::EmbeddingProvider>>,
    scan_state: ScanState,
    sync_in_progress: Arc<AtomicBool>,
    snapshot_based: bool,
    detached_head: bool,
    project_root: PathBuf,
) -> Result<(), crate::McpError> {
    let server = McpServer::with_embedding(
        config,
        root,
        submodules,
        call_log_path,
        embedding_provider,
        scan_state,
        sync_in_progress,
        snapshot_based,
        detached_head,
        project_root,
    );

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
#[allow(clippy::too_many_arguments)]
pub async fn start_stdio_with_shutdown(
    config: ServerConfig,
    root: ProjectConnection,
    submodules: HashMap<String, ProjectConnection>,
    call_log_path: Option<PathBuf>,
    embedding_provider: Option<Arc<dyn seshat_embedding::EmbeddingProvider>>,
    scan_state: ScanState,
    sync_in_progress: Arc<AtomicBool>,
    snapshot_based: bool,
    detached_head: bool,
    project_root: PathBuf,
    shutdown: impl std::future::Future<Output = ()>,
    drain_timeout: std::time::Duration,
) -> Result<(), crate::McpError> {
    let server = McpServer::with_embedding(
        config,
        root,
        submodules,
        call_log_path,
        embedding_provider,
        scan_state,
        sync_in_progress,
        snapshot_based,
        detached_head,
        project_root,
    );

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

    fn sync_flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn test_server() -> McpServer {
        McpServer::new(
            ServerConfig::default(),
            test_root(),
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        )
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
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
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

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

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

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

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
        assert!(parsed["branch"].is_null());
        assert!(parsed["data"]["conventions"].is_array());
        assert!(!parsed["data"]["conventions"].as_array().unwrap().is_empty());
        assert!(parsed["metadata"]["search_type"].is_null());
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
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
        let hash = parsed["data"]["description_hash"].as_str().unwrap();
        assert!(!hash.is_empty(), "description_hash must be populated");
        assert_eq!(
            parsed["data"]["description"],
            "Always use Result for fallible operations"
        );
        assert_eq!(parsed["data"]["nature"], "decision");
        assert_eq!(parsed["data"]["weight"], "strong");
        assert_eq!(parsed["metadata"]["description_hash"], hash);
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
        let hash = record_parsed["data"]["description_hash"]
            .as_str()
            .unwrap()
            .to_owned();

        // Update it.
        let result = server.update_decision(Parameters(UpdateDecisionRequest {
            description_hash: hash.clone(),
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
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
        // H1: PK migrates with the description change.
        let expected_new_hash =
            seshat_graph::compute_description_hash("Updated decision description");
        assert_eq!(parsed["data"]["description_hash"], expected_new_hash);
        assert_ne!(parsed["data"]["description_hash"], hash);
        assert_eq!(
            parsed["data"]["description"],
            "Updated decision description"
        );
        assert_eq!(parsed["data"]["nature"], "convention");
        assert_eq!(parsed["data"]["weight"], "strong"); // unchanged default
        assert_eq!(parsed["metadata"]["description_hash"], expected_new_hash);
    }

    #[test]
    fn update_decision_tool_nonexistent_returns_error() {
        let server = test_server();

        let result = server.update_decision(Parameters(UpdateDecisionRequest {
            description_hash: "deadbeefcafebabe".to_owned(),
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
        assert_eq!(parsed["error"]["code"], "DECISION_NOT_FOUND");
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
        let hash = record_parsed["data"]["description_hash"]
            .as_str()
            .unwrap()
            .to_owned();

        // Remove it.
        let result = server.remove_decision(Parameters(RemoveDecisionRequest {
            description_hash: hash.clone(),
            reason: "No longer relevant".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "remove_decision");
        assert_eq!(parsed["repo"], "test-project");
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
        assert_eq!(parsed["data"]["description_hash"], hash);
        assert!(
            parsed["data"]["message"]
                .as_str()
                .unwrap()
                .contains("removed successfully")
        );
        assert_eq!(parsed["metadata"]["description_hash"], hash);
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
        let hash = record_parsed["data"]["description_hash"]
            .as_str()
            .unwrap()
            .to_owned();

        let result = server.remove_decision(Parameters(RemoveDecisionRequest {
            description_hash: hash,
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

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            submodules,
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

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

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            submodules,
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

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
    }

    #[test]
    fn file_path_in_root_stays_root() {
        let root = test_root();

        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            submodules,
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

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
    }

    #[test]
    fn file_path_with_leading_dot_slash_normalized() {
        let root = test_root();

        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            submodules,
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

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
    }

    #[test]
    fn record_decision_with_file_path_routes_to_submodule() {
        let root = test_root();

        let sub_db = seshat_storage::Database::open(":memory:").expect("in-memory DB");
        let sub_conn =
            ProjectConnection::new(sub_db.connection().clone(), "vendor/libfoo", "develop");

        let mut submodules = HashMap::new();
        submodules.insert("vendor/libfoo".to_owned(), sub_conn);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            submodules,
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

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
        assert!(
            !parsed["data"]["description_hash"]
                .as_str()
                .unwrap()
                .is_empty()
        );
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
            description_hash: "deadbeefcafebabe".to_owned(),
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
            description_hash: "deadbeefcafebabe".to_owned(),
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
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
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
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
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
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
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
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
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
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
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

    // ── US-002: query_code_pattern integration tests ──────────

    use crate::tools::query_code_pattern::QueryCodePatternRequest;

    /// Helper: insert an IR file into the database for integration tests,
    /// keeping the V13 symbol-index tables in sync with `files_ir`.
    ///
    /// The SQL-backed `query_code_pattern` path (US-009) reads from
    /// `symbol_definitions`; tests that only seed `files_ir` would see an
    /// empty keyword result set after the cutover.
    fn insert_ir_for_server(
        conn: &Arc<std::sync::Mutex<rusqlite::Connection>>,
        branch_id: &str,
        file: &seshat_core::ProjectFile,
    ) {
        {
            let c = conn.lock().unwrap();
            let ir_data = seshat_storage::serialize_ir(file).expect("serialize IR");
            let file_path = file.path.to_string_lossy();
            c.execute(
                "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    branch_id,
                    file_path.as_ref(),
                    file.language.as_str(),
                    file.content_hash,
                    ir_data,
                ],
            )
            .expect("insert IR");
        }

        let defs = seshat_storage::extract_definitions(file);
        let imps = seshat_storage::extract_imports(file);
        let repo = seshat_storage::SqliteSymbolIndexRepository::new(conn.clone());
        seshat_storage::SymbolIndexRepository::replace_file(
            &repo,
            &seshat_core::BranchId::from(branch_id),
            &file.path.to_string_lossy(),
            &defs,
            &imps,
        )
        .expect("replace symbol-index rows");
    }

    /// Sample project file for integration tests.
    fn sample_ir_file() -> seshat_core::ProjectFile {
        use seshat_core::*;

        ProjectFile {
            path: std::path::PathBuf::from("src/handler.rs"),
            language: Language::Rust,
            content_hash: "int_test_hash".to_owned(),
            imports: Vec::new(),
            exports: vec![Export {
                name: "handle_request".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: vec![Function {
                name: "handle_request".to_owned(),
                is_public: true,
                is_async: true,
                line: 10,
                end_line: 50,
                parameters: vec!["req".to_owned()],
                doc_comment: None,
            }],
            types: vec![TypeDef {
                name: "RequestHandler".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 5,
                end_line: 5,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(ir::RustIR::default()),
            file_doc: None,
        }
    }

    #[test]
    fn query_code_pattern_tool_returns_success_envelope() {
        let root = test_root();
        insert_ir_for_server(&root.conn, "main", &sample_ir_file());

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_code_pattern(Parameters(QueryCodePatternRequest {
            query: "handle_request".to_owned(),
            kind: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_code_pattern");
        assert_eq!(parsed["repo"], "test-project");
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
        assert!(parsed["data"]["patterns"].is_array());
        assert!(!parsed["data"]["patterns"].as_array().unwrap().is_empty());
        assert!(parsed["data"]["related_conventions"].is_array());
        assert!(parsed["metadata"]["pattern_count"].is_null());
        assert!(parsed["metadata"]["search_type"].is_null());
    }

    #[test]
    fn query_code_pattern_tool_empty_query_returns_error() {
        let server = test_server();

        let result = server.query_code_pattern(Parameters(QueryCodePatternRequest {
            query: "".to_owned(),
            kind: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn query_code_pattern_tool_no_results_returns_empty_arrays() {
        let root = test_root();
        insert_ir_for_server(&root.conn, "main", &sample_ir_file());

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_code_pattern(Parameters(QueryCodePatternRequest {
            query: "nonexistent_symbol_xyz_999".to_owned(),
            kind: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["patterns"].as_array().unwrap().len(), 0);
        assert!(parsed["metadata"]["pattern_count"].is_null());
    }

    #[test]
    fn query_code_pattern_tool_wrong_repo_returns_error() {
        let server = test_server();

        let result = server.query_code_pattern(Parameters(QueryCodePatternRequest {
            query: "handle".to_owned(),
            kind: None,
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "query_code_pattern");
    }

    #[test]
    fn query_code_pattern_call_log_records_summary() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("call-log.jsonl");

        let root = test_root();
        insert_ir_for_server(&root.conn, "main", &sample_ir_file());

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            Some(log_path.clone()),
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_code_pattern(Parameters(QueryCodePatternRequest {
            query: "handle_request".to_owned(),
            kind: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");

        let entries = read_jsonl(&log_path);
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry["tool"], "query_code_pattern");
        assert_eq!(entry["status"], "ok");
        assert!(entry["result"]["pattern_count"].as_u64().unwrap() > 0);
        assert!(entry["result"].get("convention_count").is_some());
        // Verify pattern_count comes from data.patterns array, not removed metadata
        assert!(parsed["metadata"]["pattern_count"].is_null());
    }

    // ── US-004: query_dependencies integration tests ──────────

    use crate::tools::query_dependencies::QueryDependenciesRequest;

    /// Sample IR files for dependency integration tests.
    fn sample_dependency_files() -> (seshat_core::ProjectFile, seshat_core::ProjectFile) {
        use seshat_core::*;

        let handler = ProjectFile {
            path: std::path::PathBuf::from("src/handler.rs"),
            language: Language::Rust,
            content_hash: "dep_handler_hash".to_owned(),
            imports: vec![Import {
                module: "./utils".to_owned(),
                names: vec!["format_response".to_owned()],
                is_type_only: false,
                line: 3,
            }],
            exports: vec![Export {
                name: "handle_request".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: vec![Function {
                name: "handle_request".to_owned(),
                is_public: true,
                is_async: true,
                line: 10,
                end_line: 50,
                parameters: vec!["req".to_owned()],
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: vec![DependencyUsage {
                package: "serde".to_owned(),
                import_path: "serde::Serialize".to_owned(),
                line: 1,
            }],
            language_ir: LanguageIR::Rust(ir::RustIR::default()),
            file_doc: None,
        };

        let utils = ProjectFile {
            path: std::path::PathBuf::from("src/utils.rs"),
            language: Language::Rust,
            content_hash: "dep_utils_hash".to_owned(),
            imports: Vec::new(),
            exports: vec![Export {
                name: "format_response".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: vec![Function {
                name: "format_response".to_owned(),
                is_public: true,
                is_async: false,
                line: 5,
                end_line: 20,
                parameters: vec!["data".to_owned()],
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(ir::RustIR::default()),
            file_doc: None,
        };

        (handler, utils)
    }

    #[test]
    fn query_dependencies_tool_returns_success_envelope() {
        let root = test_root();
        let (handler, utils) = sample_dependency_files();
        insert_ir_for_server(&root.conn, "main", &handler);
        insert_ir_for_server(&root.conn, "main", &utils);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/handler.rs".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
            depth: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_dependencies");
        assert_eq!(parsed["repo"], "test-project");
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
        assert!(parsed["data"]["dependencies"].is_array());
        assert!(parsed["data"]["dependents"].is_array());
        assert!(parsed["data"]["blast_radius"].is_string());
        assert!(parsed["data"]["external_dependencies"].is_array());
        // Duplicate fields no longer in metadata
        assert!(parsed["metadata"]["target"].is_null());
        assert!(parsed["metadata"]["dependent_count"].is_null());
        assert!(parsed["metadata"]["dependency_count"].is_null());
        assert!(parsed["metadata"]["blast_radius"].is_null());
        assert!(parsed["metadata"]["next_steps"].is_array());
    }

    #[test]
    fn query_dependencies_tool_target_not_found_returns_error() {
        let root = test_root();
        let (_, utils) = sample_dependency_files();
        insert_ir_for_server(&root.conn, "main", &utils);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/nonexistent.rs".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
            depth: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "NODE_NOT_FOUND");
        assert_eq!(parsed["tool"], "query_dependencies");
    }

    #[test]
    fn query_dependencies_tool_wrong_repo_returns_error() {
        let server = test_server();

        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/handler.rs".to_owned(),
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
            depth: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "query_dependencies");
    }

    #[test]
    fn query_dependencies_call_log_records_summary() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("call-log.jsonl");

        let root = test_root();
        let (handler, utils) = sample_dependency_files();
        insert_ir_for_server(&root.conn, "main", &handler);
        insert_ir_for_server(&root.conn, "main", &utils);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            Some(log_path.clone()),
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/handler.rs".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
            depth: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");

        let entries = read_jsonl(&log_path);
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry["tool"], "query_dependencies");
        assert_eq!(entry["status"], "ok");
        assert!(entry["result"].get("dependent_count").is_some());
        assert!(entry["result"].get("dependency_count").is_some());
        assert!(entry["result"].get("blast_radius").is_some());
        assert!(entry["result"].get("transitive_dependent_count").is_some());
        // depth=None resolves to DEFAULT_TRANSITIVE_DEPTH (=3) at the
        // tool layer, so the recorded summary should reflect that.
        assert_eq!(entry["result"]["requested_depth"], 3);
    }

    /// 3-file chain for transitive dependents tests:
    /// `utils.rs` ← `handler.rs` ← `main.rs`. Querying `utils.rs` at
    /// depth ≥ 2 must surface `main.rs` as a transitive dependent.
    fn sample_dependency_chain() -> (
        seshat_core::ProjectFile,
        seshat_core::ProjectFile,
        seshat_core::ProjectFile,
    ) {
        use seshat_core::*;

        let (handler, utils) = sample_dependency_files();
        let main = ProjectFile {
            path: std::path::PathBuf::from("src/main.rs"),
            language: Language::Rust,
            content_hash: "dep_main_hash".to_owned(),
            imports: vec![Import {
                module: "./handler".to_owned(),
                names: vec!["handle_request".to_owned()],
                is_type_only: false,
                line: 2,
            }],
            exports: Vec::new(),
            functions: vec![Function {
                name: "main".to_owned(),
                is_public: true,
                is_async: false,
                line: 5,
                end_line: 15,
                parameters: Vec::new(),
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(ir::RustIR::default()),
            file_doc: None,
        };
        (utils, handler, main)
    }

    #[test]
    fn query_dependencies_tool_default_depth_returns_transitive() {
        let root = test_root();
        let (utils, handler, main) = sample_dependency_chain();
        insert_ir_for_server(&root.conn, "main", &utils);
        insert_ir_for_server(&root.conn, "main", &handler);
        insert_ir_for_server(&root.conn, "main", &main);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        // Default depth (None) → DEFAULT_TRANSITIVE_DEPTH = 3.
        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/utils.rs".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
            depth: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["requested_depth"], 3);

        // The chain main.rs → handler.rs → utils.rs means utils.rs has
        // handler.rs at depth=1 and main.rs at depth=2 in its dependents.
        let deps = parsed["data"]["dependents"].as_array().unwrap();
        let paths: Vec<&str> = deps
            .iter()
            .map(|d| d["file_path"].as_str().unwrap())
            .collect();
        assert!(
            paths.iter().any(|p| p.ends_with("src/handler.rs")),
            "expected handler.rs (direct) in dependents; got {paths:?}",
        );
        assert!(
            paths.iter().any(|p| p.ends_with("src/main.rs")),
            "expected main.rs (transitive at depth=2) in dependents; got {paths:?}",
        );

        // The transitive count surfaces in the data envelope.
        let transitive = parsed["data"]["transitive_dependent_count"]
            .as_u64()
            .unwrap();
        assert!(
            transitive >= 2,
            "expected at least 2 transitive dependents, got {transitive}",
        );

        // Per-entry depth is populated for direct entries; via remains
        // empty for direct entries and contains the chain for transitive.
        let main_entry = deps
            .iter()
            .find(|d| d["file_path"].as_str().unwrap().ends_with("src/main.rs"))
            .expect("main.rs should appear in dependents");
        assert_eq!(main_entry["depth"], 2);
        let via = main_entry["via"].as_array().unwrap();
        assert_eq!(
            via.len(),
            1,
            "expected single intermediate (handler.rs) in via"
        );
        assert!(
            via[0].as_str().unwrap().ends_with("src/handler.rs"),
            "via should reference handler.rs",
        );
    }

    #[test]
    fn query_dependencies_tool_depth_one_returns_direct_only() {
        let root = test_root();
        let (utils, handler, main) = sample_dependency_chain();
        insert_ir_for_server(&root.conn, "main", &utils);
        insert_ir_for_server(&root.conn, "main", &handler);
        insert_ir_for_server(&root.conn, "main", &main);

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/utils.rs".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
            depth: Some(1),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["requested_depth"], 1);

        let deps = parsed["data"]["dependents"].as_array().unwrap();
        let paths: Vec<&str> = deps
            .iter()
            .map(|d| d["file_path"].as_str().unwrap())
            .collect();
        assert!(
            paths.iter().any(|p| p.ends_with("src/handler.rs")),
            "depth=1 must still surface handler.rs (direct dependent); got {paths:?}",
        );
        assert!(
            !paths.iter().any(|p| p.ends_with("src/main.rs")),
            "depth=1 must NOT surface main.rs (transitive); got {paths:?}",
        );
    }

    #[test]
    fn query_dependencies_tool_depth_zero_returns_invalid_input() {
        let server = test_server();

        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/handler.rs".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
            depth: Some(0),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
        assert_eq!(parsed["tool"], "query_dependencies");
        assert!(
            parsed["error"]["suggestion"]
                .as_str()
                .unwrap()
                .contains(&format!(
                    "Use depth between 1 and {}",
                    seshat_graph::MAX_TRANSITIVE_DEPTH
                )),
            "depth=0 should be rejected with the canonical suggestion",
        );
    }

    #[test]
    fn query_dependencies_tool_depth_above_max_returns_invalid_input() {
        let server = test_server();

        let result = server.query_dependencies(Parameters(QueryDependenciesRequest {
            path: "src/handler.rs".to_owned(),
            repo: None,
            scope: None,
            file_path: None,
            depth: Some(11),
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
        assert_eq!(parsed["tool"], "query_dependencies");
        assert!(
            parsed["error"]["suggestion"]
                .as_str()
                .unwrap()
                .contains(&format!(
                    "Use depth between 1 and {}",
                    seshat_graph::MAX_TRANSITIVE_DEPTH
                )),
            "depth>10 should be rejected with the canonical suggestion",
        );
    }

    // ── US-006: validate_approach integration tests ───────────

    use crate::tools::validate_approach::ValidateApproachRequest;

    #[test]
    fn validate_approach_tool_returns_success_envelope() {
        let root = test_root();
        insert_ir_for_server(&root.conn, "main", &sample_ir_file());

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.validate_approach(Parameters(ValidateApproachRequest {
            description: "add new unique_widget_zzz component".to_owned(),
            file_context: None,
            approach_type: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "validate_approach");
        assert_eq!(parsed["repo"], "test-project");
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
        assert!(parsed["data"]["verdict"].is_string());
        assert!(parsed["data"]["ready"].is_boolean());
        assert!(parsed["data"]["rules"].is_array());
        assert!(parsed["data"]["contradictions"].is_array());
        assert!(parsed["data"]["duplicates"].is_array());
        assert!(parsed["data"]["conventions"].is_array());
        assert!(parsed["data"]["decisions"].is_array());
        assert!(parsed["data"]["observations"].is_array());
        assert!(parsed["data"]["summary"].is_string());
        assert!(parsed["data"]["what_would_help"].is_array());
        // Duplicate fields no longer in metadata
        assert!(parsed["metadata"]["verdict"].is_null());
        assert!(parsed["metadata"]["rule_count"].is_null());
        assert!(parsed["metadata"]["duplicate_count"].is_null());
        assert!(parsed["metadata"]["convention_count"].is_null());
        assert!(parsed["metadata"]["ready"].is_null());
        assert!(parsed["metadata"]["next_steps"].is_array());
    }

    #[test]
    fn validate_approach_tool_empty_description_returns_error() {
        let server = test_server();

        let result = server.validate_approach(Parameters(ValidateApproachRequest {
            description: "".to_owned(),
            file_context: None,
            approach_type: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
        assert_eq!(parsed["tool"], "validate_approach");
    }

    #[test]
    fn validate_approach_tool_wrong_repo_returns_error() {
        let server = test_server();

        let result = server.validate_approach(Parameters(ValidateApproachRequest {
            description: "some approach".to_owned(),
            file_context: None,
            approach_type: None,
            repo: Some("wrong-project".to_owned()),
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "REPO_NOT_FOUND");
        assert_eq!(parsed["tool"], "validate_approach");
    }

    #[test]
    fn validate_approach_call_log_records_summary() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("call-log.jsonl");

        let root = test_root();
        insert_ir_for_server(&root.conn, "main", &sample_ir_file());

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            Some(log_path.clone()),
            ScanState::not_needed(),
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.validate_approach(Parameters(ValidateApproachRequest {
            description: "add unique_widget_zzz component".to_owned(),
            file_context: None,
            approach_type: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");

        let entries = read_jsonl(&log_path);
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry["tool"], "validate_approach");
        assert_eq!(entry["status"], "ok");
        assert!(entry["result"].get("verdict").is_some());
        assert!(entry["result"].get("rule_count").is_some());
        assert!(entry["result"].get("duplicate_count").is_some());
        assert!(entry["result"].get("convention_count").is_some());
        assert!(entry["result"].get("ready").is_some());
    }

    #[test]
    fn scan_state_not_needed_returns_immediately() {
        let state = ScanState::not_needed();
        state.wait_for_scan();
        assert!(!state.auto_scanned());
        assert!(state.error_message().is_none());
    }

    #[test]
    fn scan_state_in_progress_waits_for_complete() {
        let state = ScanState::in_progress();
        let state_clone = state.clone();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            state_clone.mark_complete();
        });

        state.wait_for_scan();
        assert!(state.auto_scanned());
        assert!(state.is_first_run());
        assert!(state.error_message().is_none());

        handle.join().expect("thread join");
    }

    #[test]
    fn scan_state_in_progress_waits_for_failed() {
        let state = ScanState::in_progress();
        let state_clone = state.clone();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            state_clone.mark_failed("scan error".to_owned());
        });

        state.wait_for_scan();
        assert!(state.scan_attempted());
        assert!(!state.auto_scanned());
        assert_eq!(state.error_message(), Some("scan error".to_owned()));

        handle.join().expect("thread join");
    }

    #[test]
    fn scan_state_failed_returns_error_message() {
        let state = ScanState::in_progress();
        state.mark_failed("disk full".to_owned());
        assert_eq!(state.error_message(), Some("disk full".to_owned()));
        assert!(state.scan_attempted());
        assert!(!state.auto_scanned());
    }

    #[test]
    fn scan_state_auto_scanned_flag() {
        let state = ScanState::in_progress();
        assert!(!state.auto_scanned());
        state.mark_complete();
        assert!(state.auto_scanned());
        assert!(state.is_first_run());
    }

    #[test]
    fn auto_scan_failed_returns_error_on_tool_call() {
        let root = test_root();
        let scan_state = ScanState::in_progress();
        scan_state.mark_failed("disk full".to_owned());

        let server = McpServer::new(
            ServerConfig::default(),
            root,
            HashMap::new(),
            None,
            scan_state,
            sync_flag(),
            false,
            false,
            PathBuf::new(),
        );

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "AUTO_SCAN_FAILED");
        assert!(
            parsed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("disk full")
        );
    }

    #[test]
    fn sync_in_progress_flag_injects_metadata_into_response() {
        let sync_flag = Arc::new(AtomicBool::new(true));
        let server = McpServer::new(
            ServerConfig::default(),
            test_root(),
            HashMap::new(),
            None,
            ScanState::not_needed(),
            sync_flag,
            true,
            false,
            PathBuf::new(),
        );

        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["metadata"]["_metadata"]["syncing"], true);
        assert_eq!(parsed["metadata"]["_metadata"]["snapshot_based"], true);
    }

    #[test]
    fn sync_not_in_progress_omits_metadata() {
        let server = test_server();
        let result = server.query_project_context(Parameters(ProjectContextRequest {
            focus_area: None,
            repo: None,
            scope: None,
            file_path: None,
        }));

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert!(parsed["metadata"]["_metadata"].is_null());
    }
}
