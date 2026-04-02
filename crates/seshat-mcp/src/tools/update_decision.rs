//! Thin handler for the `update_decision` MCP tool.
//!
//! Parses MCP input, validates parameters, calls
//! `seshat_graph::update_decision`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata};

/// Request parameters for `update_decision`.
#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct UpdateDecisionRequest {
    /// ID of the decision node to update (required).
    #[schemars(description = "ID of the decision node to update")]
    pub id: i64,

    /// Updated description (optional — only set if provided).
    #[schemars(description = "Updated description text")]
    pub description: Option<String>,

    /// Updated nature: 'decision', 'convention', or 'preference' (optional).
    #[schemars(description = "Updated nature: 'decision', 'convention', or 'preference'")]
    pub nature: Option<String>,

    /// Updated weight: 'rule' or 'strong' (optional).
    #[schemars(description = "Updated weight: 'rule' or 'strong'")]
    pub weight: Option<String>,

    /// Updated category (optional).
    #[schemars(description = "Updated category for grouping")]
    pub category: Option<String>,

    /// Updated evidence examples (optional — replaces all existing examples).
    #[schemars(description = "Updated evidence examples: [{file, line, end_line, snippet}]")]
    pub examples: Option<Vec<ExampleInput>>,

    /// Updated reason (optional).
    #[schemars(description = "Updated reasoning or rationale")]
    pub reason: Option<String>,

    /// Repository name or path. Auto-detected in single-repo mode (Epic 5).
    /// Required in multi-repo daemon mode (Epic 6).
    #[schemars(
        description = "Repository name. Auto-detected in project mode, required in daemon mode."
    )]
    pub repo: Option<String>,

    /// Scope within the repository: 'root' (default) or a submodule name.
    /// Reserved for submodule-aware queries (Epic 6).
    #[schemars(description = "Scope: 'root' (default) or submodule name.")]
    pub scope: Option<String>,
}

/// An evidence example from the codebase.
#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct ExampleInput {
    /// File path.
    pub file: String,
    /// Start line number.
    pub line: Option<u32>,
    /// End line number.
    pub end_line: Option<u32>,
    /// Code snippet.
    pub snippet: Option<String>,
}

/// Execute the `update_decision` tool.
///
/// Thin layer: validate input -> call `seshat_graph::update_decision` -> wrap
/// in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    _branch: &str,
    req: UpdateDecisionRequest,
) -> String {
    let start = Instant::now();
    let tool = "update_decision";

    // Map MCP examples to graph examples.
    let examples = req.examples.map(|exs| {
        exs.into_iter()
            .map(|ex| seshat_graph::decisions::ExampleInput {
                file: ex.file,
                line: ex.line.unwrap_or(0),
                end_line: ex.end_line.unwrap_or(ex.line.unwrap_or(0)),
                snippet: ex.snippet.unwrap_or_default(),
            })
            .collect()
    });

    // Trim description if provided.
    let description = req.description.and_then(|d| {
        let trimmed = d.trim().to_owned();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    let params = seshat_graph::UpdateDecisionParams {
        id: req.id,
        description,
        nature: req.nature,
        weight: req.weight,
        category: req.category,
        examples,
        reason: req.reason,
    };

    match seshat_graph::update_decision(conn, params) {
        Ok(data) => {
            let metadata = ResponseMetadata::new(vec![
                "Use query_convention to verify the updated decision".to_owned(),
            ])
            .with_extra("node_id", serde_json::Value::from(data.id));

            let envelope =
                ResponseEnvelope::success(tool, repo_name, _branch, data, metadata, start);

            serde_json::to_string(&envelope).unwrap_or_else(|e| {
                let err = ErrorEnvelope::new(
                    tool,
                    repo_name,
                    ErrorCode::InternalError,
                    format!("Failed to serialize response: {e}"),
                    "Please report this issue",
                );
                serde_json::to_string(&err).unwrap_or_default()
            })
        }
        Err(seshat_graph::GraphError::NodeNotFound(msg)) => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::NodeNotFound,
                msg,
                "Check the node ID and try again",
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
        Err(seshat_graph::GraphError::NotUserDecision(msg)) => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::NotUserDecision,
                msg,
                "Only user-recorded decisions can be updated. Auto-detected conventions are managed by re-scanning.",
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
        Err(seshat_graph::GraphError::InvalidInput(msg)) => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::InvalidInput,
                msg,
                "Check the nature and weight parameter values",
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
        Err(e) => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::InternalError,
                format!("{e}"),
                "Check database and retry",
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_storage::Database;

    fn test_conn() -> Arc<Mutex<Connection>> {
        let db = Database::open(":memory:").expect("in-memory DB");
        db.connection().clone()
    }

    /// Helper: record a decision and return its node ID.
    fn record_test_decision(conn: &Arc<Mutex<Connection>>) -> i64 {
        let result = seshat_graph::record_decision(
            conn,
            "main",
            seshat_graph::RecordDecisionParams {
                description: "Original test decision".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: Some("testing".to_owned()),
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();
        result.id
    }

    #[test]
    fn handle_updates_decision_successfully() {
        let conn = test_conn();
        let node_id = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            UpdateDecisionRequest {
                id: node_id,
                description: Some("Updated description".to_owned()),
                nature: Some("convention".to_owned()),
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "update_decision");
        assert_eq!(parsed["data"]["id"], node_id);
        assert_eq!(parsed["data"]["description"], "Updated description");
        assert_eq!(parsed["data"]["nature"], "convention");
        assert_eq!(parsed["data"]["weight"], "strong"); // unchanged
        assert_eq!(parsed["metadata"]["node_id"], node_id);
    }

    #[test]
    fn handle_node_not_found_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            UpdateDecisionRequest {
                id: 99999,
                description: Some("Should fail".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "NODE_NOT_FOUND");
    }

    #[test]
    fn handle_auto_detected_returns_not_user_decision() {
        let conn = test_conn();

        // Insert an auto-detected node.
        let node_id = {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.9, 10, 12, 'Auto convention', ?1)",
                rusqlite::params![serde_json::json!({"source": "auto_detected"}).to_string()],
            )
            .unwrap();
            c.last_insert_rowid()
        };

        let result = handle(
            &conn,
            "test-project",
            "main",
            UpdateDecisionRequest {
                id: node_id,
                description: Some("Should fail".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "NOT_USER_DECISION");
    }

    #[test]
    fn handle_whitespace_description_treated_as_no_change() {
        let conn = test_conn();
        let node_id = record_test_decision(&conn);

        // Whitespace-only description is treated as None (no change).
        let result = handle(
            &conn,
            "test-project",
            "main",
            UpdateDecisionRequest {
                id: node_id,
                description: Some("   ".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["description"], "Original test decision");
    }
}
