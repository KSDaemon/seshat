//! Thin handler for the `query_dependencies` MCP tool.
//!
//! Parses MCP input, validates the path parameter, calls
//! `seshat_graph::query_dependencies`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rusqlite::Connection;
use seshat_graph::MAX_TRANSITIVE_DEPTH;

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

/// Default transitive `depth` used when the MCP request omits it.
///
/// Resolves 1st-, 2nd-, and 3rd-order dependents in a single tool call,
/// which matches the PRD's "AI agent wants ripple impact at a glance"
/// shape. Callers can opt back into direct-only by passing `depth: 1`.
pub const DEFAULT_TRANSITIVE_DEPTH: u32 = 3;

/// Request parameters for `query_dependencies`.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct QueryDependenciesRequest {
    /// File path relative to project root to analyze dependencies for.
    /// This is the target file whose imports (dependencies) and importers
    /// (dependents) will be returned.
    #[schemars(
        description = "File path relative to project root to analyze dependencies for (e.g., 'src/handler.rs')"
    )]
    pub path: String,

    /// Repository name or path. Auto-detected in single-repo mode.
    /// Required in multi-repo daemon mode.
    #[schemars(
        description = "Repository name. Auto-detected in project mode, required in daemon mode."
    )]
    pub repo: Option<String>,

    /// Scope within the repository: `'root'` (default) or the submodule mount
    /// path relative to the project root (e.g. `'vendor/libfoo'`). Short names
    /// (last path segment, e.g. `'libfoo'`) work when unambiguous. Omit to
    /// auto-detect from `file_path`, or default to root.
    #[schemars(
        description = "Scope: 'root' (default) or submodule mount path relative to project root (e.g. 'vendor/libfoo'). Short names work if unambiguous. Omit to auto-detect from file_path."
    )]
    pub scope: Option<String>,

    /// File path relative to project root for automatic scope detection.
    /// If the file belongs to a submodule, the query targets that submodule's
    /// knowledge graph.
    #[schemars(
        description = "File path relative to project root. Used for automatic scope detection — if the file belongs to a submodule, the query targets that submodule's knowledge graph."
    )]
    pub file_path: Option<String>,

    /// Transitive dependents traversal depth. `1` returns only direct
    /// dependents (preserves the historical contract); `2..=10` enables
    /// breadth-first transitive expansion. Omit to use the default of
    /// `3`, which surfaces 1st-, 2nd-, and 3rd-order dependents.
    #[schemars(
        description = "Transitive dependents depth. 1 = direct only; 2..=10 = transitive BFS. Default is 3 (1st-, 2nd-, and 3rd-order dependents)."
    )]
    pub depth: Option<u32>,
}

/// Execute the `query_dependencies` tool.
///
/// Thin layer: validate input -> call `seshat_graph::query_dependencies` ->
/// wrap in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    req: QueryDependenciesRequest,
) -> String {
    let tool = "query_dependencies";

    // Normalize the path: trim whitespace, strip leading `./`, replace backslashes.
    let mut path = req.path.trim().replace('\\', "/");
    while path.starts_with("./") {
        path = path[2..].to_owned();
    }

    // Reject path traversal attempts: check each path component for "..".
    if path.split('/').any(|c| c == "..") {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "Paths containing .. are not allowed",
            "Use a resolved path like src/handler.rs without parent directory references",
        );
        return serde_json::to_string(&err).unwrap_or_else(|_| {
            r#"{"status":"error","tool":"query_dependencies","repo":"","error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error","suggestion":"Report this issue"}}"#.to_owned()
        });
    }

    // Reject empty paths.
    if path.is_empty() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "The path parameter must not be empty",
            "Provide a relative file path like 'src/handler.rs'",
        );
        return serde_json::to_string(&err).unwrap_or_else(|_| {
            r#"{"status":"error","tool":"query_dependencies","repo":"","error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error","suggestion":"Report this issue"}}"#.to_owned()
        });
    }

    // Reject absolute paths.
    if path.starts_with('/') {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "Absolute paths are not allowed — provide a path relative to the project root",
            "Use a relative path like 'src/handler.rs' instead of '/src/handler.rs'",
        );
        return serde_json::to_string(&err).unwrap_or_else(|_| {
            r#"{"status":"error","tool":"query_dependencies","repo":"","error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error","suggestion":"Report this issue"}}"#.to_owned()
        });
    }

    // Resolve the requested depth (default = DEFAULT_TRANSITIVE_DEPTH) and
    // reject out-of-range values up-front with a domain-appropriate
    // suggestion. The graph layer also validates, but mapping its
    // generic suggestion text would lose the "1..=10" hint.
    let depth = req.depth.unwrap_or(DEFAULT_TRANSITIVE_DEPTH);
    if depth == 0 || depth > MAX_TRANSITIVE_DEPTH {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            format!("depth must be between 1 and {MAX_TRANSITIVE_DEPTH} (got {depth})"),
            "Use depth between 1 and 10",
        );
        return serde_json::to_string(&err).unwrap_or_else(|_| {
            r#"{"status":"error","tool":"query_dependencies","repo":"","error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error","suggestion":"Report this issue"}}"#.to_owned()
        });
    }

    let result = seshat_graph::query_dependencies(
        conn,
        branch,
        &path,
        seshat_graph::QueryDependenciesOptions { depth },
    );

    match result {
        Ok(data) => {
            let dependent_count = data.dependents.len();
            let dependency_count = data.dependencies.len();

            let mut next_steps = Vec::new();
            if dependent_count > 0 {
                next_steps.push("Review dependents before changing public API".to_owned());
                next_steps
                    .push("Call validate_approach to check for convention violations".to_owned());
            }
            if dependency_count > 0 {
                next_steps
                    .push("Review dependencies to understand what this file relies on".to_owned());
            }
            if dependent_count == 0 && dependency_count == 0 {
                next_steps
                    .push("This file has no known dependencies or dependents in the IR".to_owned());
            }

            let metadata = ResponseMetadata::new(next_steps);

            let envelope = ResponseEnvelope::success(tool, repo_name, data, metadata);

            serialize_response(tool, repo_name, &envelope)
        }
        Err(e) => map_graph_error(tool, repo_name, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use seshat_core::{
        DependencyUsage, Export, Function, Import, Language, LanguageIR, ProjectFile, RustIR,
    };

    use crate::test_helpers::{insert_ir, test_conn};

    /// Helper: create a target file that imports from utils.
    fn target_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/handler.rs"),
            language: Language::Rust,
            content_hash: "target_hash".to_owned(),
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
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    /// Helper: create a utils file that the target imports from.
    fn utils_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/utils.rs"),
            language: Language::Rust,
            content_hash: "utils_hash".to_owned(),
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
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    /// Helper: create a consumer file that imports from the target.
    fn consumer_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            content_hash: "consumer_hash".to_owned(),
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
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    #[test]
    fn handle_returns_success_with_dependencies_and_dependents() {
        let conn = test_conn();
        insert_ir(&conn, "main", &target_file());
        insert_ir(&conn, "main", &utils_file());
        insert_ir(&conn, "main", &consumer_file());

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryDependenciesRequest {
                path: "src/handler.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
                depth: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_dependencies");
        assert_eq!(parsed["repo"], "test-project");
        assert!(parsed["data"]["dependencies"].is_array());
        assert!(parsed["data"]["dependents"].is_array());
        assert!(parsed["data"]["blast_radius"].is_string());
        // Verify duplicate metadata extras are absent
        assert!(parsed["metadata"]["dependent_count"].is_null());
        assert!(parsed["metadata"]["dependency_count"].is_null());
        assert!(parsed["metadata"]["blast_radius"].is_null());
        assert!(parsed["metadata"]["target"].is_null());
        assert!(parsed["branch"].is_null());
        assert!(parsed["duration_ms"].is_null());
    }

    #[test]
    fn handle_target_not_found_returns_error() {
        let conn = test_conn();
        // Insert some IR so the branch exists, but query a non-existent file.
        insert_ir(&conn, "main", &utils_file());

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryDependenciesRequest {
                path: "src/nonexistent.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
                depth: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "NODE_NOT_FOUND");
    }

    #[test]
    fn handle_path_traversal_rejected() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryDependenciesRequest {
                path: "../etc/passwd".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
                depth: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
        assert!(parsed["error"]["message"].as_str().unwrap().contains(".."));
    }

    #[test]
    fn handle_empty_path_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryDependenciesRequest {
                path: "".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
                depth: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        // Empty path is an InvalidInput, not NodeNotFound.
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn handle_external_dependencies_included() {
        let conn = test_conn();
        insert_ir(&conn, "main", &target_file());

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryDependenciesRequest {
                path: "src/handler.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
                depth: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        let external = parsed["data"]["external_dependencies"].as_array().unwrap();
        assert!(!external.is_empty());
        assert_eq!(external[0]["package"], "serde");
    }

    #[test]
    fn handle_response_envelope_structure() {
        let conn = test_conn();
        insert_ir(&conn, "main", &target_file());
        insert_ir(&conn, "main", &utils_file());

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryDependenciesRequest {
                path: "src/handler.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
                depth: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_dependencies");

        // data has expected fields
        assert!(parsed["data"]["target"].is_string());
        assert!(parsed["data"]["dependencies"].is_array());
        assert!(parsed["data"]["dependents"].is_array());
        assert!(parsed["data"]["external_dependencies"].is_array());
        assert!(parsed["data"]["blast_radius"].is_string());

        // top-level metadata has only next_steps (no duplicate data fields)
        assert!(parsed["metadata"]["next_steps"].is_array());
        assert!(parsed["metadata"]["target"].is_null());
        assert!(parsed["metadata"]["dependent_count"].is_null());
        assert!(parsed["metadata"]["dependency_count"].is_null());
        assert!(parsed["metadata"]["blast_radius"].is_null());
    }
}
