//! Thin handler for the `query_code_pattern` MCP tool.
//!
//! Parses MCP input, validates the query parameter, calls
//! `seshat_graph::query_code_pattern`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

/// Request parameters for `query_code_pattern`.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct QueryCodePatternRequest {
    /// Search query string — matched against function, type, and export names
    /// in the project's IR. Supports multi-token queries.
    #[schemars(
        description = "Search query for code patterns (e.g., 'handleRequest', 'Error', 'parse config')"
    )]
    pub query: String,

    /// Optional kind filter: "function", "type", "export", or "all" (default).
    /// When set, only patterns of the specified kind are returned.
    #[schemars(
        description = "Filter by pattern kind: 'function', 'type', 'export', or 'all' (default)"
    )]
    pub kind: Option<String>,

    /// Repository name or path. Auto-detected in single-repo mode.
    /// Required in multi-repo daemon mode.
    #[schemars(
        description = "Repository name. Auto-detected in project mode, required in daemon mode."
    )]
    pub repo: Option<String>,

    /// Scope within the repository: 'root' (default) or a submodule name.
    #[schemars(description = "Scope: 'root' (default) or submodule name.")]
    pub scope: Option<String>,

    /// File path relative to project root for automatic scope detection.
    /// If the file belongs to a submodule, the query targets that submodule's
    /// knowledge graph.
    #[schemars(
        description = "File path relative to project root. Used for automatic scope detection — if the file belongs to a submodule, the query targets that submodule's knowledge graph."
    )]
    pub file_path: Option<String>,
}

/// Execute the `query_code_pattern` tool.
///
/// Thin layer: validate input -> call `seshat_graph::query_code_pattern` ->
/// optionally filter by kind -> wrap in envelope. Returns the serialised JSON
/// envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    scope_name: &str,
    req: QueryCodePatternRequest,
) -> String {
    let start = Instant::now();
    let tool = "query_code_pattern";

    // Validate: query must not be empty.
    let query = req.query.trim();
    if query.is_empty() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "The query parameter must not be empty",
            "Provide a search query like 'handleRequest', 'Error', or 'parse config'",
        );
        return serde_json::to_string(&err).unwrap_or_else(|_| {
            r#"{"status":"error","tool":"query_code_pattern","repo":"","error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error","suggestion":"Report this issue"}}"#.to_owned()
        });
    }

    // Validate kind filter before issuing the query.
    const VALID_KINDS: &[&str] = &["function", "type", "export", "all"];
    if let Some(ref kind_filter) = req.kind {
        let kind_lower = kind_filter.trim().to_lowercase();
        if !kind_lower.is_empty() && !VALID_KINDS.contains(&kind_lower.as_str()) {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::InvalidInput,
                format!(
                    "Invalid kind filter '{kind_filter}'. Allowed values: function, type, export, all"
                ),
                "Use one of: 'function', 'type', 'export', or 'all'",
            );
            return serde_json::to_string(&err).unwrap_or_else(|_| {
                r#"{"status":"error","tool":"query_code_pattern","repo":"","error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error","suggestion":"Report this issue"}}"#.to_owned()
            });
        }
    }

    let result = seshat_graph::query_code_pattern(conn, branch, query);

    match result {
        Ok(mut data) => {
            // Apply optional kind filter.
            if let Some(ref kind_filter) = req.kind {
                let kind_lower = kind_filter.trim().to_lowercase();
                if kind_lower != "all" && !kind_lower.is_empty() {
                    data.patterns.retain(|p| p.kind == kind_lower);
                    // Update metadata counts after filtering.
                    data.metadata.pattern_count = data.patterns.len();
                }
            }

            let pattern_count = data.metadata.pattern_count;
            let convention_count = data.metadata.convention_count;

            let mut next_steps = Vec::new();
            if pattern_count > 0 {
                next_steps.push(
                    "Call query_dependencies on matching files to understand blast radius"
                        .to_owned(),
                );
                next_steps
                    .push("Call validate_approach to check for convention violations".to_owned());
            } else {
                next_steps.push(
                    "Try broader search terms or check if the codebase has been scanned".to_owned(),
                );
            }
            if convention_count > 0 {
                next_steps
                    .push("Review related conventions before implementing new code".to_owned());
            }

            let metadata = ResponseMetadata::new(next_steps)
                .with_extra("query", query)
                .with_extra("pattern_count", pattern_count)
                .with_extra("convention_count", convention_count)
                .with_extra("search_type", data.metadata.search_type.as_str());

            let envelope = ResponseEnvelope::success(
                tool, repo_name, branch, scope_name, data, metadata, start,
            );

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
        Export, Function, Language, LanguageIR, ProjectFile, RustIR, TypeDef, TypeDefKind,
    };

    use crate::test_helpers::{insert_ir, test_conn};

    /// Helper: create a sample ProjectFile with functions, types, and exports.
    fn sample_project_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: Vec::new(),
            exports: vec![Export {
                name: "handle_request".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            functions: vec![
                Function {
                    name: "handle_request".to_owned(),
                    is_public: true,
                    is_async: true,
                    line: 10,
                    end_line: 50,
                    parameters: vec!["req".to_owned()],
                },
                Function {
                    name: "parse_config".to_owned(),
                    is_public: true,
                    is_async: false,
                    line: 52,
                    end_line: 80,
                    parameters: vec!["path".to_owned()],
                },
            ],
            types: vec![TypeDef {
                name: "RequestHandler".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 5,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        }
    }

    #[test]
    fn handle_returns_success_with_patterns() {
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_code_pattern");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert!(parsed["data"]["patterns"].is_array());
        assert!(!parsed["data"]["patterns"].as_array().unwrap().is_empty());
        assert_eq!(parsed["metadata"]["query"], "handle_request");
        assert!(parsed["metadata"]["pattern_count"].as_u64().unwrap() > 0);
        assert_eq!(parsed["metadata"]["search_type"], "keyword");
    }

    #[test]
    fn handle_empty_query_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryCodePatternRequest {
                query: "".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn handle_whitespace_query_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryCodePatternRequest {
                query: "   ".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn handle_no_results_returns_success_with_empty_arrays() {
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryCodePatternRequest {
                query: "nonexistent_xyz_999".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["patterns"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["metadata"]["pattern_count"], 0);
    }

    #[test]
    fn handle_kind_filter_narrows_results() {
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        // "handle" matches function handle_request AND export handle_request.
        // Filter to only functions.
        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: Some("function".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        let patterns = parsed["data"]["patterns"].as_array().unwrap();
        assert!(!patterns.is_empty());
        for p in patterns {
            assert_eq!(p["kind"], "function");
        }
    }

    #[test]
    fn handle_kind_filter_all_returns_everything() {
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: Some("all".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        // Should include both function and export results.
        let patterns = parsed["data"]["patterns"].as_array().unwrap();
        let kinds: Vec<&str> = patterns
            .iter()
            .map(|p| p["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"function"));
        assert!(kinds.contains(&"export"));
    }

    #[test]
    fn handle_response_envelope_structure() {
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryCodePatternRequest {
                query: "parse".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_code_pattern");

        // data has patterns + related_conventions + metadata
        assert!(parsed["data"]["patterns"].is_array());
        assert!(parsed["data"]["related_conventions"].is_array());
        assert!(parsed["data"]["metadata"].is_object());

        // top-level metadata (envelope) has next_steps and extras
        assert!(parsed["metadata"]["next_steps"].is_array());
        assert!(parsed["metadata"]["query"].is_string());
        assert!(parsed["metadata"]["pattern_count"].is_number());
        assert!(parsed["metadata"]["convention_count"].is_number());
        assert!(parsed["metadata"]["search_type"].is_string());
    }
}
