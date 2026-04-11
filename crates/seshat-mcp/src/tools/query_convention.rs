//! Thin handler for the `query_convention` MCP tool.
//!
//! Parses MCP input, validates the topic parameter, calls
//! `seshat_graph::query_convention`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

/// Request parameters for `query_convention`.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct QueryConventionRequest {
    /// Topic to search for in convention descriptions (e.g., "error handling",
    /// "logging", "naming"). Searched via FTS5 full-text search.
    #[schemars(
        description = "Topic to search conventions for (e.g., 'error handling', 'logging')"
    )]
    pub topic: String,

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

/// Execute the `query_convention` tool.
///
/// Thin layer: validate input → call `seshat_graph::query_convention` → wrap
/// in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    scope_name: &str,
    req: QueryConventionRequest,
) -> String {
    let start = Instant::now();
    let tool = "query_convention";

    // Validate: topic must not be empty.
    let topic = req.topic.trim();
    if topic.is_empty() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::EmptyTopic,
            "The topic parameter must not be empty",
            "Provide a topic like 'error handling', 'logging', or 'naming conventions'",
        );
        return serde_json::to_string(&err).unwrap_or_default();
    }

    let result = seshat_graph::query_convention(conn, branch, topic);

    match result {
        Ok(data) => {
            let results_count = data.conventions.len();

            let mut next_steps = Vec::new();
            if results_count > 0 {
                next_steps.push(
                    "Use record_decision to capture team conventions not auto-detected".to_owned(),
                );
                next_steps
                    .push("Use query_project_context for a broader project overview".to_owned());
                next_steps.push(
                    "Use validate_approach to check your proposed changes against these conventions".to_owned(),
                );
            } else {
                next_steps.push(format!(
                    "No conventions found for '{}'. Try a broader term or use query_project_context to see all detected conventions",
                    topic
                ));
            }

            let metadata = ResponseMetadata::new(next_steps)
                .with_extra("query", topic)
                .with_extra("results_count", results_count)
                .with_extra("search_type", "fts5");

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

    use crate::test_helpers::{insert_convention, test_conn};

    #[test]
    fn handle_returns_success_with_conventions() {
        let conn = test_conn();
        insert_convention(
            &conn,
            "Uses thiserror for error handling (Rust)",
            "error_handling",
            0.9,
        );

        // Rebuild FTS5 index.
        seshat_graph::rebuild_fts_index(&conn).unwrap();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryConventionRequest {
                topic: "error".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_convention");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert!(parsed["data"]["conventions"].is_array());
        assert!(!parsed["data"]["conventions"].as_array().unwrap().is_empty());
        assert_eq!(parsed["metadata"]["search_type"], "fts5");
        assert_eq!(parsed["metadata"]["query"], "error");
        assert!(parsed["metadata"]["results_count"].as_u64().unwrap() > 0);
    }

    #[test]
    fn handle_empty_topic_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryConventionRequest {
                topic: "".to_owned(),
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
    fn handle_whitespace_topic_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryConventionRequest {
                topic: "   ".to_owned(),
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
    fn handle_no_matches_returns_success_with_empty_array() {
        let conn = test_conn();
        seshat_graph::rebuild_fts_index(&conn).unwrap();

        let result = handle(
            &conn,
            "test-project",
            "main",
            "root",
            QueryConventionRequest {
                topic: "nonexistent_xyz".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["conventions"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["metadata"]["results_count"], 0);
    }
}
