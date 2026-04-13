//! Thin handler for the `query_project_context` MCP tool.
//!
//! Parses MCP input, calls `seshat_graph::query_project_context`, and wraps
//! the result in a `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{ResponseEnvelope, ResponseMetadata, map_graph_error, serialize_response};

/// Request parameters for `query_project_context`.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct ProjectContextRequest {
    /// Optional focus area to filter results (e.g., "logging", "testing").
    /// Filters conventions by case-insensitive substring match on description.
    #[schemars(description = "Optional domain to focus on (e.g., 'logging', 'testing')")]
    pub focus_area: Option<String>,

    /// Repository name or path. Auto-detected in single-repo mode (Epic 5).
    /// Required in multi-repo daemon mode (Epic 6).
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

/// Execute the `query_project_context` tool.
///
/// Thin layer: parse input → call `seshat_graph::query_project_context` → wrap
/// in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    req: ProjectContextRequest,
) -> String {
    let tool = "query_project_context";

    let result = seshat_graph::query_project_context(conn, branch, req.focus_area.as_deref());

    match result {
        Ok(data) => {
            let mut next_steps =
                vec!["Use query_convention to explore specific conventions".to_owned()];

            next_steps.push(
                "Use validate_approach to check your proposed changes against project rules and conventions".to_owned(),
            );

            // Suggest focus areas based on what we found.
            if !data.dependencies.by_domain.is_empty() {
                if let Some(top) = data.dependencies.by_domain.first() {
                    next_steps.push(format!(
                        "Query conventions for '{}' domain: query_convention(topic: '{}')",
                        top.domain, top.domain
                    ));
                }
            }

            let metadata = if let Some(ref focus) = req.focus_area {
                ResponseMetadata::new(next_steps)
                    .with_extra("focus_area", serde_json::Value::from(focus.as_str()))
            } else {
                ResponseMetadata::new(next_steps)
            };

            let envelope = ResponseEnvelope::success(tool, repo_name, data, metadata);

            serialize_response(tool, repo_name, &envelope)
        }
        Err(e) => map_graph_error(tool, repo_name, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::test_helpers::make_project_file;
    use seshat_core::{BranchId, Language};
    use seshat_storage::{FileIRRepository, SqliteFileIRRepository};

    use crate::test_helpers::{insert_convention, test_conn};

    fn insert_file(conn: &Arc<Mutex<Connection>>, path: &str, lang: Language) {
        let repo = SqliteFileIRRepository::new(conn.clone());
        let branch = BranchId::from("main");
        let mut file = make_project_file(lang);
        file.path = path.into();
        file.content_hash = format!("hash_{path}");
        repo.upsert(&branch, &file, None).unwrap();
    }

    #[test]
    fn handle_returns_success_envelope() {
        let conn = test_conn();
        insert_file(&conn, "src/main.rs", Language::Rust);
        insert_convention(
            &conn,
            "Uses reqwest for HTTP client (Rust)",
            "dependency_usage",
            0.9,
        );

        let result = handle(
            &conn,
            "test-project",
            "main",
            ProjectContextRequest {
                focus_area: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_project_context");
        assert_eq!(parsed["repo"], "test-project");
        // Verify noisy fields are absent
        assert!(parsed["branch"].is_null());
        assert!(parsed["scope"].is_null());
        assert!(parsed["duration_ms"].is_null());
        assert!(parsed["data"]["languages"].is_array());
        assert!(parsed["data"]["golden_files"].is_array());
        assert!(parsed["data"]["submodules"].is_array());
        assert_eq!(parsed["data"]["conventions_count"], 1);
        // focus_area must be absent from metadata when not requested
        assert!(
            parsed["metadata"]["focus_area"].is_null(),
            "focus_area should not appear in metadata when not provided"
        );
    }

    #[test]
    fn handle_with_focus_area() {
        let conn = test_conn();
        insert_convention(
            &conn,
            "Uses reqwest for HTTP client (Rust)",
            "dependency_usage",
            0.9,
        );
        insert_convention(&conn, "snake_case naming (Rust)", "naming", 0.95);

        let result = handle(
            &conn,
            "test-project",
            "main",
            ProjectContextRequest {
                focus_area: Some("HTTP".to_owned()),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["conventions_count"], 1);
        assert_eq!(parsed["metadata"]["focus_area"], "HTTP");
    }

    #[test]
    fn handle_empty_db() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            ProjectContextRequest {
                focus_area: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["conventions_count"], 0);
        assert_eq!(parsed["data"]["languages"].as_array().unwrap().len(), 0);
    }
}
