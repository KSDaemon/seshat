//! Thin handler for the `validate_approach` MCP tool.
//!
//! Parses MCP input, validates the description parameter, calls
//! `seshat_graph::validate_approach`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

/// Request parameters for `validate_approach`.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct ValidateApproachRequest {
    /// Description of the proposed approach to validate against rules,
    /// conventions, and existing code patterns.
    #[schemars(
        description = "Description of the proposed approach to validate (e.g., 'add a new error handler using anyhow')"
    )]
    pub description: String,

    /// Optional file context for enriching results. When provided,
    /// duplicate pattern entries will include `used_by` counts from the
    /// dependency index.
    #[schemars(
        description = "Optional file context for enriching duplicate results with dependency counts"
    )]
    pub file_context: Option<String>,

    /// Optional approach type for categorisation (e.g., "refactor",
    /// "new_feature", "bug_fix").
    #[schemars(
        description = "Optional approach type (e.g., 'refactor', 'new_feature', 'bug_fix')"
    )]
    pub approach_type: Option<String>,

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

/// Execute the `validate_approach` tool.
///
/// Thin layer: validate input -> call `seshat_graph::validate_approach` ->
/// wrap in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    scope_name: &str,
    req: ValidateApproachRequest,
) -> String {
    let start = Instant::now();
    let tool = "validate_approach";

    // Validate: description must not be empty.
    let description = req.description.trim();
    if description.is_empty() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::EmptyTopic,
            "The description parameter must not be empty",
            "Provide a description of the approach you want to validate",
        );
        return serde_json::to_string(&err).unwrap_or_default();
    }

    let params = seshat_graph::ValidateApproachParams {
        description: description.to_owned(),
        file_context: req.file_context,
        approach_type: req.approach_type,
    };

    let result = seshat_graph::validate_approach(conn, branch, params);

    match result {
        Ok(data) => {
            let verdict = data.verdict.clone();
            let rule_count = data.rules.len();
            let duplicate_count = data.duplicates.len();
            let convention_count = data.conventions.len();
            let ready = data.ready;

            let mut next_steps = Vec::new();
            match verdict.as_str() {
                "rules_violated" => {
                    next_steps.push("Fix rule violations before proceeding".to_owned());
                    next_steps.push(
                        "Review each rule in the 'rules' section for specific requirements"
                            .to_owned(),
                    );
                }
                "warnings_found" => {
                    next_steps.push(
                        "Review contradictions and strong conventions before proceeding".to_owned(),
                    );
                    next_steps.push(
                        "Consider adjusting your approach to align with conventions".to_owned(),
                    );
                }
                "info_only" => {
                    next_steps.push(
                        "Review the conventions for context, then proceed with implementation"
                            .to_owned(),
                    );
                }
                _ => {
                    next_steps.push("Approach looks good — proceed with implementation".to_owned());
                }
            }

            if duplicate_count > 0 {
                next_steps.push(
                    "Consider reusing existing code patterns listed in 'duplicates'".to_owned(),
                );
            }

            let metadata = ResponseMetadata::new(next_steps)
                .with_extra("verdict", verdict.as_str())
                .with_extra("rule_count", rule_count)
                .with_extra("duplicate_count", duplicate_count)
                .with_extra("convention_count", convention_count)
                .with_extra("ready", ready);

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
        Export, Function, Language, LanguageIR, ProjectFile, RustIR, TypeDef, TypeDefKind,
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

    /// Helper: create a sample ProjectFile.
    fn sample_project_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: Vec::new(),
            exports: vec![Export {
                name: "handle_error".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            functions: vec![Function {
                name: "handle_error".to_owned(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 50,
                parameters: vec!["err".to_owned()],
            }],
            types: vec![TypeDef {
                name: "ErrorHandler".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 5,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        }
    }

    /// Helper: insert a convention node.
    fn insert_convention(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        description: &str,
        weight: &str,
        confidence: f64,
        nature: &str,
    ) {
        let c = conn.lock().unwrap();
        let ext = serde_json::json!({
            "source": if nature == "decision" { "user" } else { "auto_detected" },
            "detector_name": "test",
            "trend": "stable",
            "evidence": [{
                "file": "src/main.rs",
                "line": 10,
                "end_line": 15,
                "snippet": "example snippet"
            }]
        });
        c.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             VALUES (?1, ?2, ?3, ?4, 9, 10, ?5, ?6)",
            params![branch_id, nature, weight, confidence, description, ext.to_string()],
        )
        .unwrap();
    }

    #[test]
    fn handle_returns_success_for_clean_approach() {
        let conn = test_conn();
        let file = sample_project_file("src/utils.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            ValidateApproachRequest {
                description: "add new widget component zzz_unique".to_owned(),
                file_context: None,
                approach_type: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "validate_approach");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert_eq!(parsed["data"]["verdict"], "approved");
        assert_eq!(parsed["data"]["ready"], true);
        assert_eq!(parsed["metadata"]["verdict"], "approved");
        assert_eq!(parsed["metadata"]["ready"], true);
    }

    #[test]
    fn handle_empty_description_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            ValidateApproachRequest {
                description: "".to_owned(),
                file_context: None,
                approach_type: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "EMPTY_TOPIC");
    }

    #[test]
    fn handle_whitespace_description_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            ValidateApproachRequest {
                description: "   ".to_owned(),
                file_context: None,
                approach_type: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "EMPTY_TOPIC");
    }

    #[test]
    fn handle_with_rule_violation_returns_rules_violated() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Always use thiserror for error types",
            "rule",
            1.0,
            "convention",
        );
        seshat_graph::rebuild_fts_index(&conn).unwrap();

        let file = sample_project_file("src/errors.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            ValidateApproachRequest {
                description: "thiserror error types".to_owned(),
                file_context: None,
                approach_type: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["verdict"], "rules_violated");
        assert_eq!(parsed["data"]["ready"], false);
        assert_eq!(parsed["metadata"]["verdict"], "rules_violated");
        assert_eq!(parsed["metadata"]["ready"], false);
        assert!(parsed["metadata"]["rule_count"].as_u64().unwrap() > 0);
    }

    #[test]
    fn handle_with_duplicates_populates_duplicates() {
        let conn = test_conn();
        let file = sample_project_file("src/errors.rs");
        insert_ir(&conn, "main", &file);

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            ValidateApproachRequest {
                description: "handle_error".to_owned(),
                file_context: Some("src/errors.rs".to_owned()),
                approach_type: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert!(parsed["metadata"]["duplicate_count"].as_u64().unwrap() > 0);
        assert!(parsed["data"]["duplicates"].is_array());
        assert!(!parsed["data"]["duplicates"].as_array().unwrap().is_empty());
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
            ValidateApproachRequest {
                description: "add new feature xyz unique test".to_owned(),
                file_context: None,
                approach_type: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "validate_approach");

        // data has expected fields
        assert!(parsed["data"]["rules"].is_array());
        assert!(parsed["data"]["contradictions"].is_array());
        assert!(parsed["data"]["duplicates"].is_array());
        assert!(parsed["data"]["conventions"].is_array());
        assert!(parsed["data"]["decisions"].is_array());
        assert!(parsed["data"]["observations"].is_array());
        assert!(parsed["data"]["verdict"].is_string());
        assert!(parsed["data"]["ready"].is_boolean());
        assert!(parsed["data"]["what_would_help"].is_array());
        assert!(parsed["data"]["summary"].is_string());

        // top-level metadata (envelope) has next_steps and extras
        assert!(parsed["metadata"]["next_steps"].is_array());
        assert!(parsed["metadata"]["verdict"].is_string());
        assert!(parsed["metadata"]["rule_count"].is_number());
        assert!(parsed["metadata"]["duplicate_count"].is_number());
        assert!(parsed["metadata"]["convention_count"].is_number());
        assert!(parsed["metadata"]["ready"].is_boolean());
    }
}
