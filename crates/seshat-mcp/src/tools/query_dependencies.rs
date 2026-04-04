//! Thin handler for the `query_dependencies` MCP tool.
//!
//! Parses MCP input, validates the path parameter, calls
//! `seshat_graph::query_dependencies`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{ResponseEnvelope, ResponseMetadata, map_graph_error, serialize_response};

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

/// Execute the `query_dependencies` tool.
///
/// Thin layer: validate input -> call `seshat_graph::query_dependencies` ->
/// wrap in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    scope_name: &str,
    req: QueryDependenciesRequest,
) -> String {
    let start = Instant::now();
    let tool = "query_dependencies";

    let result = seshat_graph::query_dependencies(conn, branch, &req.path);

    match result {
        Ok(data) => {
            let dependent_count = data.dependents.len();
            let dependency_count = data.dependencies.len();
            let blast_radius = data.blast_radius.clone();

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

            let metadata = ResponseMetadata::new(next_steps)
                .with_extra("target", req.path.as_str())
                .with_extra("dependent_count", dependent_count)
                .with_extra("dependency_count", dependency_count)
                .with_extra("blast_radius", blast_radius.as_str());

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

    use rusqlite::params;
    use seshat_core::{
        DependencyUsage, Export, Function, Import, Language, LanguageIR, ProjectFile, RustIR,
    };
    use seshat_storage::serialize_ir;

    use crate::test_helpers::test_conn;

    /// Helper: insert an IR file into the database for a branch.
    fn insert_ir(conn: &Arc<Mutex<Connection>>, branch_id: &str, file: &ProjectFile) {
        let c = conn.lock().unwrap();
        let ir_data = serialize_ir(file).expect("serialize IR");
        let file_path = file.path.to_string_lossy();
        c.execute(
            "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                branch_id,
                file_path.as_ref(),
                file.language.as_str(),
                file.content_hash,
                ir_data,
            ],
        )
        .expect("insert IR");
    }

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
            }],
            functions: vec![Function {
                name: "handle_request".to_owned(),
                is_public: true,
                is_async: true,
                line: 10,
                end_line: 50,
                parameters: vec!["req".to_owned()],
            }],
            types: Vec::new(),
            dependencies_used: vec![DependencyUsage {
                package: "serde".to_owned(),
                import_path: "serde::Serialize".to_owned(),
                line: 1,
            }],
            language_ir: LanguageIR::Rust(RustIR::default()),
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
            }],
            functions: vec![Function {
                name: "format_response".to_owned(),
                is_public: true,
                is_async: false,
                line: 5,
                end_line: 20,
                parameters: vec!["data".to_owned()],
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
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
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
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
            "root",
            QueryDependenciesRequest {
                path: "src/handler.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_dependencies");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert!(parsed["data"]["dependencies"].is_array());
        assert!(parsed["data"]["dependents"].is_array());
        assert!(parsed["data"]["blast_radius"].is_string());
        assert!(parsed["data"]["blast_radius_count"].is_number());
        assert!(parsed["metadata"]["dependent_count"].is_number());
        assert!(parsed["metadata"]["dependency_count"].is_number());
        assert!(parsed["metadata"]["blast_radius"].is_string());
        assert!(parsed["metadata"]["target"].is_string());
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
            "root",
            QueryDependenciesRequest {
                path: "src/nonexistent.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "NODE_NOT_FOUND");
    }

    #[test]
    fn handle_empty_path_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryDependenciesRequest {
                path: "".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
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
            "root",
            QueryDependenciesRequest {
                path: "src/handler.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
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
            "root",
            QueryDependenciesRequest {
                path: "src/handler.rs".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
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
        assert!(parsed["data"]["blast_radius_count"].is_number());

        // top-level metadata (envelope) has next_steps and extras
        assert!(parsed["metadata"]["next_steps"].is_array());
        assert!(parsed["metadata"]["target"].is_string());
        assert!(parsed["metadata"]["dependent_count"].is_number());
        assert!(parsed["metadata"]["dependency_count"].is_number());
        assert!(parsed["metadata"]["blast_radius"].is_string());
    }
}
