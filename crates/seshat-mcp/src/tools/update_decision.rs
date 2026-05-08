//! Thin handler for the `update_decision` MCP tool.
//!
//! Parses MCP input, validates parameters, calls
//! `seshat_graph::update_decision`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{ResponseEnvelope, ResponseMetadata, map_graph_error, serialize_response};

/// Request parameters for `update_decision`.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct UpdateDecisionRequest {
    /// Description hash of the decision to update (required).
    /// Obtain this from `data.description_hash` returned by `record_decision`,
    /// or from the `description_hash` field of a `DecisionEntry` in
    /// `validate_approach` results.
    #[schemars(
        description = "Description hash of the decision to update. Obtain from `data.description_hash` returned by `record_decision`, or from the `description_hash` field of `DecisionEntry` in `validate_approach` results."
    )]
    pub description_hash: String,

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

    /// Scope within the repository: `'root'` (default) or the submodule mount
    /// path relative to the project root (e.g. `'vendor/libfoo'`). Short names
    /// (last path segment, e.g. `'libfoo'`) work when unambiguous. Omit to
    /// auto-detect from `file_path`, or default to root.
    #[schemars(
        description = "Scope: 'root' (default) or submodule mount path relative to project root (e.g. 'vendor/libfoo'). Short names work if unambiguous. Omit to auto-detect from file_path."
    )]
    pub scope: Option<String>,

    /// File path relative to project root for automatic scope detection.
    /// If the file belongs to a submodule, the update targets that submodule's
    /// knowledge graph.
    #[schemars(
        description = "File path relative to project root. Used for automatic scope detection — if the file belongs to a submodule, the query/write targets that submodule's knowledge graph."
    )]
    pub file_path: Option<String>,
}

use super::ExampleInput;

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
    let tool = "update_decision";

    // P34: validate description_hash at the boundary instead of letting
    // an empty / whitespace-only value reach the storage layer (where
    // it would surface as DECISION_NOT_FOUND).
    let hash = req.description_hash.trim();
    if hash.is_empty() {
        let err = crate::envelope::ErrorEnvelope::new(
            tool,
            repo_name,
            crate::envelope::ErrorCode::InvalidInput,
            "description_hash must not be empty",
            "Pass the value of `data.description_hash` from a prior \
             record_decision / query_convention response",
        );
        return serde_json::to_string(&err).unwrap_or_default();
    }

    // Map MCP examples to graph examples.
    let examples = req
        .examples
        .as_ref()
        .map(|exs| exs.iter().map(Into::into).collect());

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
        description_hash: req.description_hash,
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
    fn handle_updates_decision_successfully() {
        let conn = test_conn();
        let hash = record_test_decision(&conn);

        let result = handle(
            &conn,
            "test-project",
            "main",
            UpdateDecisionRequest {
                description_hash: hash.clone(),
                description: Some("Updated description".to_owned()),
                nature: Some("convention".to_owned()),
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "update_decision");
        // PK migrates with the description change (H1: content-derived
        // hash invariant). The returned hash must match the recomputed
        // hash of the new description, NOT the old hash the caller sent.
        let expected_new_hash = seshat_graph::compute_description_hash("Updated description");
        assert_eq!(parsed["data"]["description_hash"], expected_new_hash);
        assert_ne!(parsed["data"]["description_hash"], hash);
        assert_eq!(parsed["data"]["description"], "Updated description");
        assert_eq!(parsed["data"]["nature"], "convention");
        assert_eq!(parsed["data"]["weight"], "strong"); // unchanged
        assert_eq!(parsed["metadata"]["description_hash"], expected_new_hash);
    }

    #[test]
    fn handle_hash_not_found_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            UpdateDecisionRequest {
                description_hash: "deadbeefcafebabe".to_owned(),
                description: Some("Should fail".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "DECISION_NOT_FOUND");
    }

    #[test]
    fn handle_refuses_to_mutate_tui_approved_decision() {
        // H2: MCP must not be able to clobber a TUI-confirmed convention.
        // We seed a state='approved' row directly (bypassing the
        // record_decision path, which always writes 'recorded') and
        // verify the MCP layer surfaces NOT_USER_DECISION rather than
        // silently overwriting the description.
        use seshat_core::BranchId;
        use seshat_storage::{
            Decision, DecisionNature, DecisionRepository, DecisionState, DecisionWeight,
            SqliteDecisionRepository,
        };

        let conn = test_conn();
        let hash = seshat_graph::compute_description_hash("convention approved through the TUI");

        {
            let repo = SqliteDecisionRepository::new(conn.clone());
            let row = Decision {
                description_hash: hash.clone(),
                description: "convention approved through the TUI".to_owned(),
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
            UpdateDecisionRequest {
                description_hash: hash.clone(),
                description: Some("agent rewrites the convention".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error", "envelope: {parsed}");
        assert_eq!(parsed["error"]["code"], "NOT_USER_DECISION");

        // Row in the DB is untouched — the TUI's authority survives.
        let repo = SqliteDecisionRepository::new(conn.clone());
        let row = repo
            .get_by_hash(&hash)
            .unwrap()
            .expect("approved row still present");
        assert_eq!(row.state, DecisionState::Approved);
        assert_eq!(row.description, "convention approved through the TUI");
    }

    #[test]
    fn handle_whitespace_description_treated_as_no_change() {
        let conn = test_conn();
        let hash = record_test_decision(&conn);

        // Whitespace-only description is treated as None (no change).
        let result = handle(
            &conn,
            "test-project",
            "main",
            UpdateDecisionRequest {
                description_hash: hash,
                description: Some("   ".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
                repo: None,
                scope: None,
                file_path: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(
            parsed["data"]["description"],
            "Test decision for removal/update"
        );
    }
}
