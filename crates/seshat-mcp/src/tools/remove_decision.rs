//! Thin handler for the `remove_decision` MCP tool.
//!
//! Parses MCP input, validates parameters, calls
//! `seshat_graph::remove_decision`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata};

/// Request parameters for `remove_decision`.
#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct RemoveDecisionRequest {
    /// ID of the decision node to remove (required).
    #[schemars(description = "ID of the decision node to remove")]
    pub id: i64,

    /// Reason for removal (required).
    #[schemars(description = "Reason for removing this decision")]
    pub reason: String,
}

/// Execute the `remove_decision` tool.
///
/// Thin layer: validate input -> call `seshat_graph::remove_decision` -> wrap
/// in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    req: RemoveDecisionRequest,
) -> String {
    let start = Instant::now();
    let tool = "remove_decision";

    // Validate: reason must not be empty.
    let reason = req.reason.trim();
    if reason.is_empty() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "The reason parameter must not be empty",
            "Provide a reason explaining why this decision is being removed",
        );
        return serde_json::to_string(&err).unwrap_or_default();
    }

    let params = seshat_graph::RemoveDecisionParams {
        id: req.id,
        reason: reason.to_owned(),
    };

    match seshat_graph::remove_decision(conn, params) {
        Ok(data) => {
            let metadata = ResponseMetadata::new(vec![
                "The decision has been soft-deleted and will no longer appear in query results"
                    .to_owned(),
                "Use record_decision to create a replacement if needed".to_owned(),
            ])
            .with_extra("node_id", serde_json::Value::from(data.id));

            let envelope =
                ResponseEnvelope::success(tool, repo_name, branch, data, metadata, start);

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
                "Only user-recorded decisions can be removed. Auto-detected conventions are managed by re-scanning.",
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
                description: "Decision to be removed".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();
        result.id
    }

    #[test]
    fn handle_removes_decision_successfully() {
        let conn = test_conn();
        let node_id = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                id: node_id,
                reason: "No longer needed".to_owned(),
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "remove_decision");
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
    fn handle_empty_reason_returns_error() {
        let conn = test_conn();
        let node_id = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                id: node_id,
                reason: "".to_owned(),
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn handle_node_not_found_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                id: 99999,
                reason: "Should fail".to_owned(),
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
            RemoveDecisionRequest {
                id: node_id,
                reason: "Should fail".to_owned(),
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "NOT_USER_DECISION");
    }

    #[test]
    fn handle_whitespace_reason_returns_error() {
        let conn = test_conn();
        let node_id = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                id: node_id,
                reason: "   ".to_owned(),
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }
}
