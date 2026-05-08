//! Thin handler for the `remove_decision` MCP tool.
//!
//! Parses MCP input, validates parameters, calls
//! `seshat_graph::remove_decision`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{
    ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata, map_graph_error,
    serialize_response,
};

/// Request parameters for `remove_decision`.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct RemoveDecisionRequest {
    /// Description hash of the decision to remove (required).
    /// Obtain from `data.description_hash` returned by `record_decision`,
    /// or from the `description_hash` field of a `DecisionEntry` in
    /// `validate_approach` results.
    #[schemars(
        description = "Description hash of the decision to remove. Obtain from `data.description_hash` returned by `record_decision`, or from the `description_hash` field of `DecisionEntry` in `validate_approach` results."
    )]
    pub description_hash: String,

    /// Reason for removal (required).
    #[schemars(description = "Reason for removing this decision")]
    pub reason: String,

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
    /// If the file belongs to a submodule, the removal targets that submodule's
    /// knowledge graph.
    #[schemars(
        description = "File path relative to project root. Used for automatic scope detection — if the file belongs to a submodule, the query/write targets that submodule's knowledge graph."
    )]
    pub file_path: Option<String>,
}

/// Execute the `remove_decision` tool.
///
/// Thin layer: validate input -> call `seshat_graph::remove_decision` -> wrap
/// in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    _branch: &str,
    req: RemoveDecisionRequest,
) -> String {
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
        description_hash: req.description_hash,
        reason: reason.to_owned(),
    };

    match seshat_graph::remove_decision(conn, params) {
        Ok(data) => {
            let metadata = ResponseMetadata::new(vec![
                "The decision has been deleted and will no longer appear in query results"
                    .to_owned(),
                "Use record_decision to create a replacement if needed".to_owned(),
            ])
            .with_extra(
                "description_hash",
                serde_json::Value::from(data.description_hash.as_str()),
            );

            let envelope = ResponseEnvelope::success(tool, repo_name, data, metadata);

            serialize_response(tool, repo_name, &envelope)
        }
        Err(e) => map_graph_error(tool, repo_name, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::{record_test_decision, test_conn};

    #[test]
    fn handle_removes_decision_successfully() {
        let conn = test_conn();
        let hash = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                description_hash: hash.clone(),
                reason: "No longer needed".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "remove_decision");
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
    fn handle_empty_reason_returns_error() {
        let conn = test_conn();
        let hash = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                description_hash: hash,
                reason: "".to_owned(),
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
    fn handle_hash_not_found_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                description_hash: "deadbeefcafebabe".to_owned(),
                reason: "Should fail".to_owned(),
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
    fn handle_refuses_to_remove_tui_approved_decision() {
        // H2: MCP must not be able to hard-delete a TUI-confirmed
        // convention out from under the user. Seed an approved row,
        // then assert remove_decision surfaces NOT_USER_DECISION and
        // leaves the row in place.
        use seshat_core::BranchId;
        use seshat_storage::{
            Decision, DecisionNature, DecisionRepository, DecisionState, DecisionWeight,
            SqliteDecisionRepository,
        };

        let conn = test_conn();
        let hash = seshat_graph::compute_description_hash("convention approved by the user");

        {
            let repo = SqliteDecisionRepository::new(conn.clone());
            let row = Decision {
                description_hash: hash.clone(),
                description: "convention approved by the user".to_owned(),
                state: DecisionState::Approved,
                nature: DecisionNature::Convention,
                weight: DecisionWeight::Strong,
                category: None,
                reason: None,
                examples: vec![],
                decided_on_branch: BranchId("main".to_owned()),
                decided_at: 1_700_000_000,
                updated_at: 1_700_000_000,
            };
            repo.upsert(&row).expect("seed approved row");
        }

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                description_hash: hash.clone(),
                reason: "agent decided to clean up".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error", "envelope: {parsed}");
        assert_eq!(parsed["error"]["code"], "NOT_USER_DECISION");

        // Row is intact.
        let repo = SqliteDecisionRepository::new(conn.clone());
        assert!(
            repo.get_by_hash(&hash).unwrap().is_some(),
            "approved row must survive a refused remove"
        );
    }

    #[test]
    fn handle_whitespace_reason_returns_error() {
        let conn = test_conn();
        let hash = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            RemoveDecisionRequest {
                description_hash: hash,
                reason: "   ".to_owned(),
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }
}
