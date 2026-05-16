//! Thin handler for the `query_code_pattern` MCP tool.
//!
//! Parses MCP input, validates the query parameter, calls
//! `seshat_graph::query_code_pattern`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

/// Request parameters for `query_code_pattern`.
///
/// The response data is shaped as:
/// ```text
/// {
///   "patterns": [
///     {
///       "name": "...", "kind": "function|type|export",
///       "file_path": "...", "line": N, "end_line": N, "is_public": bool,
///       "snippet": { "content": "...", "truncated": bool },
///       "score": 0.4|0.7|1.0,
///       "dependent_files": ["path/to/importer.rs", ...],
///       "blast_radius": "low|medium|high|none",
///       "call_sites": [
///         { "file": "...", "site_count": N,
///           "lines": [u32, ...], "first_snippet": "..." }
///       ],
///       "total_call_sites": N
///     }
///   ],
///   "related_conventions": [ ... ]
/// }
/// ```
///
/// Per-pattern enrichment fields:
/// - `dependent_files`: distinct files that directly import this symbol via
///   `use ::Name` / `from m import Name` / `import { Name }` — re-exports are
///   not chased and the defining file is excluded.
/// - `blast_radius`: shared low/medium/high classification (`< 5 / 5..=20 / > 20`
///   dependent files), identical thresholds to `query_dependencies`. Emits
///   `none` if dependent-file enrichment failed catastrophically — treat as
///   "unknown", not "safe".
/// - `call_sites`: one aggregate per calling file (`site_count`, ascending
///   `lines`, `first_snippet` of the lowest-line occurrence), sorted by
///   `site_count` descending and capped at a small top-N preview. The total
///   call-expression count is reported separately via `total_call_sites`
///   (uncapped).
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
    req: QueryCodePatternRequest,
    embedding_provider: Option<&dyn seshat_embedding::EmbeddingProvider>,
) -> String {
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

    // Push the kind filter down to the graph layer so it can be applied as a
    // SQL `WHERE` clause against `symbol_definitions`. An empty or "all"
    // value is normalised to `None` by the graph function itself, but we
    // strip whitespace here for friendlier error reporting above.
    let kind_for_graph = req.kind.as_deref();

    let result = seshat_graph::query_code_pattern_with_embeddings(
        conn,
        branch,
        query,
        kind_for_graph,
        embedding_provider,
    );

    match result {
        Ok(data) => {
            let pattern_count = data.patterns.len();
            let convention_count = data.related_conventions.len();
            let has_high_blast = data
                .patterns
                .iter()
                .any(|p| p.blast_radius == seshat_graph::BlastRadius::High);

            let mut next_steps = Vec::new();
            if pattern_count > 0 {
                if has_high_blast {
                    next_steps.push(
                        "blast_radius=high on at least one match — review dependent_files before any change"
                            .to_owned(),
                    );
                }
                next_steps.push(
                    "Call query_dependencies on matching files for transitive blast and external dependencies"
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
                end_line: 1,
            }],
            functions: vec![
                Function {
                    name: "handle_request".to_owned(),
                    is_public: true,
                    is_async: true,
                    line: 10,
                    end_line: 50,
                    parameters: vec!["req".to_owned()],
                    doc_comment: None,
                },
                Function {
                    name: "parse_config".to_owned(),
                    is_public: true,
                    is_async: false,
                    line: 52,
                    end_line: 80,
                    parameters: vec!["path".to_owned()],
                    doc_comment: None,
                },
            ],
            types: vec![TypeDef {
                name: "RequestHandler".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 5,
                end_line: 5,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
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
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_code_pattern");
        assert_eq!(parsed["repo"], "test-project");
        assert!(parsed["data"]["patterns"].is_array());
        assert!(!parsed["data"]["patterns"].as_array().unwrap().is_empty());
        // Verify noisy fields are absent
        assert!(parsed["metadata"]["pattern_count"].is_null());
        assert!(parsed["metadata"]["search_type"].is_null());
        assert!(parsed["branch"].is_null());
        assert!(parsed["duration_ms"].is_null());
    }

    #[test]
    fn handle_empty_query_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
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
            QueryCodePatternRequest {
                query: "   ".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
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
            QueryCodePatternRequest {
                query: "nonexistent_xyz_999".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["patterns"].as_array().unwrap().len(), 0);
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
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: Some("function".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
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
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: Some("all".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
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
            QueryCodePatternRequest {
                query: "parse".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_code_pattern");

        // data has patterns + related_conventions only (no nested metadata)
        assert!(parsed["data"]["patterns"].is_array());
        assert!(parsed["data"]["related_conventions"].is_array());
        assert!(parsed["data"]["metadata"].is_null());

        // top-level metadata has only next_steps (no noisy extras)
        assert!(parsed["metadata"]["next_steps"].is_array());
        assert!(parsed["metadata"]["pattern_count"].is_null());
        assert!(parsed["metadata"]["convention_count"].is_null());
        assert!(parsed["metadata"]["search_type"].is_null());
    }

    #[test]
    fn handle_response_includes_dependent_files_field() {
        // Every pattern entry must carry a `dependent_files` array.  For a
        // sample file with no importers seeded, the array is empty — but the
        // field MUST still be present so MCP clients can rely on it.
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let patterns = parsed["data"]["patterns"].as_array().unwrap();
        assert!(!patterns.is_empty(), "expected at least one pattern result");
        for p in patterns {
            let deps = &p["dependent_files"];
            assert!(
                deps.is_array(),
                "dependent_files must be a JSON array on every pattern, got {p}"
            );
            assert_eq!(
                deps.as_array().unwrap().len(),
                0,
                "no importer fixtures seeded, expected empty dependent_files",
            );
        }
    }

    #[test]
    fn handle_response_dependent_files_populated_from_imports() {
        // End-to-end: seed a defining file plus two importers, query through
        // the MCP handler, assert the resulting JSON carries both importer
        // paths under `dependent_files`.
        use seshat_core::{Import, LanguageIR, RustIR, TypeDef, TypeDefKind};

        let conn = test_conn();

        // Defining file: `BranchId` type in core.
        let definer = ProjectFile {
            path: PathBuf::from("crates/seshat-core/src/ids.rs"),
            language: Language::Rust,
            content_hash: "h_def".to_owned(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: vec![TypeDef {
                name: "BranchId".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 14,
                end_line: 14,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };
        insert_ir(&conn, "main", &definer);

        // Two importers.
        for path in [
            "crates/seshat-cli/src/decisions.rs",
            "crates/seshat-graph/src/decisions.rs",
        ] {
            let importer = ProjectFile {
                path: PathBuf::from(path),
                language: Language::Rust,
                content_hash: format!("h_{path}"),
                imports: vec![Import {
                    module: "seshat_core::ids".to_owned(),
                    names: vec!["BranchId".to_owned()],
                    is_type_only: false,
                    line: 1,
                }],
                exports: Vec::new(),
                functions: Vec::new(),
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: LanguageIR::Rust(RustIR::default()),
                file_doc: None,
            };
            insert_ir(&conn, "main", &importer);
        }

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "BranchId".to_owned(),
                kind: Some("type".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let patterns = parsed["data"]["patterns"].as_array().unwrap();
        let branch_id_match = patterns
            .iter()
            .find(|p| p["name"] == "BranchId" && p["kind"] == "type")
            .expect("BranchId match in response");

        let deps = branch_id_match["dependent_files"].as_array().unwrap();
        let dep_strs: Vec<&str> = deps.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            dep_strs.len(),
            2,
            "expected 2 importer files, got {dep_strs:?}"
        );
        assert!(dep_strs.contains(&"crates/seshat-cli/src/decisions.rs"));
        assert!(dep_strs.contains(&"crates/seshat-graph/src/decisions.rs"));
    }

    #[test]
    fn handle_response_includes_blast_radius_field() {
        // Every pattern entry must carry a `blast_radius` string set to one
        // of `low`, `medium`, `high`. No importers seeded ⇒ all entries land
        // on `low`, but the field MUST be present so MCP
        // clients can rely on it.
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let patterns = parsed["data"]["patterns"].as_array().unwrap();
        assert!(!patterns.is_empty(), "expected at least one pattern result");
        for p in patterns {
            let radius = p["blast_radius"].as_str().unwrap_or_else(|| {
                panic!("blast_radius must be a string on every pattern, got {p}")
            });
            assert!(
                matches!(radius, "low" | "medium" | "high"),
                "blast_radius must be one of low|medium|high, got {radius:?}",
            );
            assert_eq!(
                radius, "low",
                "no importer fixtures seeded, expected blast_radius=low",
            );
        }
    }

    #[test]
    fn handle_response_blast_radius_reflects_importer_count() {
        // 5 distinct importers ⇒ Medium under shared
        // `< 5 / 5..=20 / > 20` thresholds. Goes through the MCP handler so
        // we exercise the same JSON shape clients consume.
        use seshat_core::{Import, LanguageIR, RustIR, TypeDef, TypeDefKind};

        let conn = test_conn();

        let definer = ProjectFile {
            path: PathBuf::from("crates/seshat-core/src/ids.rs"),
            language: Language::Rust,
            content_hash: "h_def".to_owned(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: vec![TypeDef {
                name: "BranchId".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 14,
                end_line: 14,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };
        insert_ir(&conn, "main", &definer);

        for i in 0..5 {
            let importer = ProjectFile {
                path: PathBuf::from(format!("crates/importer_{i:03}.rs")),
                language: Language::Rust,
                content_hash: format!("h_imp_{i}"),
                imports: vec![Import {
                    module: "seshat_core::ids".to_owned(),
                    names: vec!["BranchId".to_owned()],
                    is_type_only: false,
                    line: 1,
                }],
                exports: Vec::new(),
                functions: Vec::new(),
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: LanguageIR::Rust(RustIR::default()),
                file_doc: None,
            };
            insert_ir(&conn, "main", &importer);
        }

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "BranchId".to_owned(),
                kind: Some("type".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let patterns = parsed["data"]["patterns"].as_array().unwrap();
        let branch_id_match = patterns
            .iter()
            .find(|p| p["name"] == "BranchId" && p["kind"] == "type")
            .expect("BranchId match in response");

        assert_eq!(
            branch_id_match["dependent_files"].as_array().unwrap().len(),
            5
        );
        assert_eq!(branch_id_match["blast_radius"], serde_json::json!("medium"));
    }

    #[test]
    fn handle_response_call_sites_aggregated_by_file() {
        // End-to-end: a symbol called 4× in one file and 1× in another
        // surfaces in the MCP response as two `call_sites` entries (one per
        // file), sorted by site_count descending, with `total_call_sites` set
        // to the unbounded sum.  Exercises the same JSON shape clients
        // consume — `line` / `end_line` from the old flat shape MUST be
        // absent, and the new fields MUST be present on every entry.
        use seshat_core::{FunctionCall, RustIR};

        let conn = test_conn();

        // Defining file: pub fn target() in core.
        let definer = ProjectFile {
            path: PathBuf::from("src/target.rs"),
            language: Language::Rust,
            content_hash: "h_def".to_owned(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: vec![Function {
                name: "target".to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                end_line: 1,
                parameters: Vec::new(),
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };
        insert_ir(&conn, "main", &definer);

        // Heavy caller: 4 calls at lines 50, 10, 30, 70 (out of source order).
        let heavy_calls = [50_usize, 10, 30, 70]
            .iter()
            .map(|&line| FunctionCall {
                callee: "target".to_owned(),
                line,
                end_line: line,
                snippet: format!("    target(); // line {line}"),
            })
            .collect::<Vec<_>>();
        let heavy = ProjectFile {
            path: PathBuf::from("src/heavy.rs"),
            language: Language::Rust,
            content_hash: "h_heavy".to_owned(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR {
                function_calls: heavy_calls,
                ..RustIR::default()
            }),
            file_doc: None,
        };
        insert_ir(&conn, "main", &heavy);

        // Light caller: 1 call.
        let light = ProjectFile {
            path: PathBuf::from("src/light.rs"),
            language: Language::Rust,
            content_hash: "h_light".to_owned(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR {
                function_calls: vec![FunctionCall {
                    callee: "target".to_owned(),
                    line: 5,
                    end_line: 5,
                    snippet: "    target();".to_owned(),
                }],
                ..RustIR::default()
            }),
            file_doc: None,
        };
        insert_ir(&conn, "main", &light);

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "target".to_owned(),
                kind: Some("function".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let patterns = parsed["data"]["patterns"].as_array().unwrap();
        let target = patterns
            .iter()
            .find(|p| p["name"] == "target" && p["kind"] == "function")
            .expect("target match in response");

        assert_eq!(target["total_call_sites"], serde_json::json!(5));
        assert!(
            target.get("call_site_count").is_none(),
            "legacy call_site_count field must be removed; got {target}"
        );

        let call_sites = target["call_sites"].as_array().unwrap();
        assert_eq!(call_sites.len(), 2, "expected one entry per calling file");

        // Sorted by site_count desc — heavy.rs (4) before light.rs (1).
        assert_eq!(call_sites[0]["file"], "src/heavy.rs");
        assert_eq!(call_sites[0]["site_count"], serde_json::json!(4));
        assert_eq!(
            call_sites[0]["lines"],
            serde_json::json!([10, 30, 50, 70]),
            "lines must be ascending"
        );
        assert!(
            call_sites[0]["first_snippet"]
                .as_str()
                .unwrap()
                .contains("line 10"),
            "first_snippet must reflect the lowest-line occurrence; got {}",
            call_sites[0]["first_snippet"]
        );
        assert!(call_sites[0].get("line").is_none());
        assert!(call_sites[0].get("end_line").is_none());
        assert!(call_sites[0].get("snippet").is_none());

        assert_eq!(call_sites[1]["file"], "src/light.rs");
        assert_eq!(call_sites[1]["site_count"], serde_json::json!(1));
        assert_eq!(call_sites[1]["lines"], serde_json::json!([5]));
    }

    #[test]
    fn handle_next_steps_suggests_dependent_files_when_blast_is_high() {
        // When any pattern match has blast_radius=high, the next_steps
        // metadata must call out reviewing dependent_files before any
        // change.  Drives 21 distinct importers of `BranchId` through the
        // MCP handler — 21 > 20 ⇒ High under the shared
        // `< 5 / 5..=20 / > 20` thresholds.
        use seshat_core::{Import, LanguageIR, RustIR, TypeDef, TypeDefKind};

        let conn = test_conn();

        let definer = ProjectFile {
            path: PathBuf::from("crates/seshat-core/src/ids.rs"),
            language: Language::Rust,
            content_hash: "h_def".to_owned(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: vec![TypeDef {
                name: "BranchId".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 14,
                end_line: 14,
                doc_comment: None,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };
        insert_ir(&conn, "main", &definer);

        for i in 0..21 {
            let importer = ProjectFile {
                path: PathBuf::from(format!("crates/importer_{i:03}.rs")),
                language: Language::Rust,
                content_hash: format!("h_imp_{i}"),
                imports: vec![Import {
                    module: "seshat_core::ids".to_owned(),
                    names: vec!["BranchId".to_owned()],
                    is_type_only: false,
                    line: 1,
                }],
                exports: Vec::new(),
                functions: Vec::new(),
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: LanguageIR::Rust(RustIR::default()),
                file_doc: None,
            };
            insert_ir(&conn, "main", &importer);
        }

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "BranchId".to_owned(),
                kind: Some("type".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let branch_id_match = parsed["data"]["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "BranchId" && p["kind"] == "type")
            .expect("BranchId match in response");
        assert_eq!(branch_id_match["blast_radius"], serde_json::json!("high"));

        let next_steps = parsed["metadata"]["next_steps"].as_array().unwrap();
        let high_suggestion = next_steps.iter().find(|step| {
            step.as_str()
                .map(|s| s.contains("blast_radius=high") && s.contains("dependent_files"))
                .unwrap_or(false)
        });
        assert!(
            high_suggestion.is_some(),
            "expected a next_steps entry calling out high blast_radius and dependent_files; got {next_steps:?}",
        );
    }

    #[test]
    fn handle_next_steps_omits_high_suggestion_when_no_match_is_high() {
        // No importers seeded ⇒ every pattern is blast_radius=low; the
        // high-blast next_steps suggestion must NOT appear.
        let conn = test_conn();
        let file = sample_project_file("src/handler.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            QueryCodePatternRequest {
                query: "handle_request".to_owned(),
                kind: None,
                repo: None,
                scope: None,
                file_path: None,
            },
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let next_steps = parsed["metadata"]["next_steps"].as_array().unwrap();
        for step in next_steps {
            let s = step.as_str().unwrap_or("");
            assert!(
                !s.contains("blast_radius=high"),
                "did not expect high-blast next_steps when all matches are low; got {next_steps:?}",
            );
        }
    }
}
