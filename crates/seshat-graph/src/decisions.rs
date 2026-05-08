//! Record, update, and remove user-recorded decisions in the V12 `decisions`
//! table.
//!
//! User-recorded decisions are project-wide knowledge keyed by
//! `description_hash`. Each row in `decisions` represents settled state:
//!   * `state='recorded'` — explicit knowledge captured via MCP `record_decision`
//!   * `state='approved' / 'rejected' / 'partial'` — TUI review of an
//!     auto-detected convention.
//!
//! Removal is a hard delete from the `decisions` table — there is no
//! soft-delete in the new schema. Re-record the decision if you change your
//! mind.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use seshat_core::BranchId;
use seshat_storage::{
    Decision, DecisionNature, DecisionRepository, DecisionState, DecisionWeight, ExampleEvidence,
    SqliteDecisionRepository,
};
use sha2::{Digest, Sha256};

use crate::error::GraphError;

/// Normalise a description for hashing: lowercase, trim, collapse
/// internal whitespace to single spaces, strip leading/trailing punctuation.
fn normalize_description(desc: &str) -> String {
    let mut s = desc.to_lowercase();
    s = s.trim().to_string();
    // Collapse internal whitespace (spaces, tabs, newlines) to single space
    let collapsed: String = s
        .chars()
        .fold((String::new(), false), |(acc, prev_space), c| {
            if c.is_whitespace() {
                (format!("{acc} "), prev_space)
            } else {
                (format!("{acc}{c}"), false)
            }
        })
        .0;
    s = collapsed;
    // Strip leading/trailing punctuation
    s = s.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    s
}

/// Compute a SHA-256 hash of the normalised description, returning
/// the first 16 hex characters.
pub fn compute_description_hash(desc: &str) -> String {
    use std::fmt::Write;
    let normalised = normalize_description(desc);
    let hash = Sha256::digest(normalised.as_bytes());
    hash.iter().take(8).fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

// ── Request types ────────────────────────────────────────────

/// Parameters for recording a new decision.
pub struct RecordDecisionParams {
    /// Human-readable description of the decision/convention (required).
    pub description: String,
    /// Nature: Decision, Convention, or Preference. Defaults to Decision.
    pub nature: String,
    /// Weight: Rule or Strong. Defaults to Strong.
    pub weight: String,
    /// Optional category for grouping (e.g., "error-handling", "naming").
    pub category: Option<String>,
    /// Optional evidence examples.
    pub examples: Vec<ExampleInput>,
    /// Optional reasoning/rationale for the decision.
    pub reason: Option<String>,
}

/// An evidence example provided by the user.
pub struct ExampleInput {
    /// File path where the example can be found.
    pub file: String,
    /// Start line number.
    pub line: u32,
    /// End line number.
    pub end_line: u32,
    /// Code snippet.
    pub snippet: String,
}

impl From<&ExampleInput> for ExampleEvidence {
    fn from(ex: &ExampleInput) -> Self {
        ExampleEvidence {
            file: ex.file.clone(),
            line: ex.line,
            end_line: ex.end_line,
            snippet: ex.snippet.clone(),
        }
    }
}

/// Parameters for updating an existing decision.
pub struct UpdateDecisionParams {
    /// Description hash of the decision to update (required).
    pub description_hash: String,
    /// Updated description (optional — only set if provided).
    pub description: Option<String>,
    /// Updated nature (optional).
    pub nature: Option<String>,
    /// Updated weight (optional).
    pub weight: Option<String>,
    /// Updated category (optional).
    pub category: Option<String>,
    /// Updated examples (optional — replaces all examples).
    pub examples: Option<Vec<ExampleInput>>,
    /// Updated reason (optional).
    pub reason: Option<String>,
}

/// Parameters for removing a decision.
pub struct RemoveDecisionParams {
    /// Description hash of the decision to remove (required).
    pub description_hash: String,
    /// Reason for removal (recorded in the audit log; the row itself is
    /// hard-deleted).
    pub reason: String,
}

// ── Response types ───────────────────────────────────────────

/// Sentinel value for the legacy `id` envelope field (H3 backwards-compat
/// shim). Pre-V12 this was the auto-generated `nodes.id` rowid; V12 keys
/// decisions by `description_hash` so there is no integer identity to
/// expose. The field is retained for one release so external clients
/// that still parse `data.id` / `metadata.node_id` get a typed zero
/// instead of a missing-field error during the transition.
const LEGACY_ID_SENTINEL: i64 = 0;

/// Response data for `record_decision`.
#[derive(Debug, Clone, Serialize)]
pub struct RecordDecisionData {
    /// The description hash, which is the row's primary key in the V12
    /// `decisions` table. Pass this to `update_decision` / `remove_decision`.
    pub description_hash: String,
    /// The description that was recorded.
    pub description: String,
    /// The nature that was set.
    pub nature: String,
    /// The weight that was set.
    pub weight: String,
    /// Legacy `id` field — always [`LEGACY_ID_SENTINEL`] in V12. Kept for
    /// envelope-shape backwards compat; will be removed one release
    /// after V12 lands. Use `description_hash` instead.
    #[serde(rename = "id")]
    pub legacy_id: i64,
}

/// Response data for `update_decision`.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateDecisionData {
    /// The description hash of the updated row.
    pub description_hash: String,
    /// The current description after update.
    pub description: String,
    /// The current nature after update.
    pub nature: String,
    /// The current weight after update.
    pub weight: String,
    /// Legacy `id` field — always [`LEGACY_ID_SENTINEL`] in V12. Kept for
    /// envelope-shape backwards compat; will be removed one release
    /// after V12 lands. Use `description_hash` instead.
    #[serde(rename = "id")]
    pub legacy_id: i64,
}

/// Response data for `remove_decision`.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveDecisionData {
    /// The description hash of the removed row.
    pub description_hash: String,
    /// Confirmation message.
    pub message: String,
    /// Legacy `id` field — always [`LEGACY_ID_SENTINEL`] in V12. Kept for
    /// envelope-shape backwards compat; will be removed one release
    /// after V12 lands. Use `description_hash` instead.
    #[serde(rename = "id")]
    pub legacy_id: i64,
}

// ── Validation ───────────────────────────────────────────────

/// Valid nature values for user-recorded decisions.
const VALID_NATURES: &[&str] = &["decision", "convention", "preference"];

/// Valid weight values for user-recorded decisions.
const VALID_WEIGHTS: &[&str] = &["rule", "strong"];

/// Map a validated user-input nature string to a `DecisionNature` enum.
fn parse_nature(nature: &str) -> Result<DecisionNature, GraphError> {
    let n = nature.to_lowercase();
    if !VALID_NATURES.contains(&n.as_str()) {
        return Err(GraphError::InvalidInput(format!(
            "Invalid nature '{nature}'. Must be one of: decision, convention, preference"
        )));
    }
    Ok(match n.as_str() {
        "convention" => DecisionNature::Convention,
        "preference" => DecisionNature::Preference,
        _ => DecisionNature::Decision,
    })
}

/// Map a validated user-input weight string to a `DecisionWeight` enum.
fn parse_weight(weight: &str) -> Result<DecisionWeight, GraphError> {
    let w = weight.to_lowercase();
    if !VALID_WEIGHTS.contains(&w.as_str()) {
        return Err(GraphError::InvalidInput(format!(
            "Invalid weight '{weight}'. Must be one of: rule, strong"
        )));
    }
    Ok(match w.as_str() {
        "rule" => DecisionWeight::Rule,
        _ => DecisionWeight::Strong,
    })
}

/// Render a `DecisionNature` back to the user-facing string form used by the
/// MCP envelope (the SQL form happens to match in every case).
fn nature_to_string(n: DecisionNature) -> String {
    n.as_sql_str().to_owned()
}

/// Render a `DecisionWeight` back to the user-facing string form used by the
/// MCP envelope (the SQL form happens to match in every case).
fn weight_to_string(w: DecisionWeight) -> String {
    w.as_sql_str().to_owned()
}

/// Reject mutations targeting decisions whose state is not `recorded`.
///
/// Rows with `state ∈ {approved, rejected, partial}` are produced by the
/// TUI review flow (US-005) and represent settled user decisions about
/// auto-detected conventions. Letting MCP `update_decision` /
/// `remove_decision` mutate them would let an agent silently override
/// or delete those decisions, undermining G7 ("MCP tools and TUI flow
/// share one storage backend, no parallel mechanisms") in the wrong
/// direction — by giving MCP write authority over TUI-owned rows.
///
/// Pre-V12 (the deleted `*_returns_not_user_decision` tests) the same
/// invariant was enforced via the auto-detected vs user-recorded
/// distinction on `nodes`. The new schema collapses both into a single
/// table, but the authority distinction is preserved here via `state`.
fn ensure_state_is_recorded(decision: &Decision, op: &'static str) -> Result<(), GraphError> {
    if decision.state == DecisionState::Recorded {
        return Ok(());
    }
    Err(GraphError::NotUserDecision(format!(
        "Cannot {op} decision {}: row state is '{}' (TUI-owned, not MCP-mutable). \
         Re-run the TUI review flow to change it, or remove and re-record \
         it explicitly.",
        decision.description_hash,
        decision.state.as_sql_str()
    )))
}

// ── Record function ──────────────────────────────────────────

/// Record a new user decision in the V12 `decisions` table with
/// `state='recorded'`. No `nodes` row is created.
///
/// # Errors
///
/// Returns `GraphError::InvalidInput` for an invalid nature or weight, or
/// `GraphError::Storage` if the database operation fails.
pub fn record_decision(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    params: RecordDecisionParams,
) -> Result<RecordDecisionData, GraphError> {
    let nature = parse_nature(&params.nature)?;
    let weight = parse_weight(&params.weight)?;

    let description_hash = compute_description_hash(&params.description);
    let now = chrono::Utc::now().timestamp();

    let examples: Vec<ExampleEvidence> = params.examples.iter().map(Into::into).collect();

    let decision = Decision {
        description_hash: description_hash.clone(),
        description: params.description.clone(),
        state: DecisionState::Recorded,
        nature,
        weight,
        category: params.category,
        reason: params.reason,
        examples,
        decided_on_branch: BranchId::from(branch_id),
        decided_at: now,
        updated_at: now,
    };

    let repo = SqliteDecisionRepository::new(conn.clone());
    repo.upsert(&decision).map_err(GraphError::Storage)?;

    tracing::info!(
        description_hash = %description_hash,
        description = %params.description,
        nature = %nature.as_sql_str(),
        weight = %weight.as_sql_str(),
        "Recorded user decision"
    );

    Ok(RecordDecisionData {
        description_hash,
        description: params.description,
        nature: nature_to_string(nature),
        weight: weight_to_string(weight),
        legacy_id: LEGACY_ID_SENTINEL,
    })
}

// ── Update function ──────────────────────────────────────────

/// Update an existing user decision keyed by `description_hash`.
///
/// Loads the existing row, merges the provided fields, and re-upserts. Only
/// fields explicitly set on `params` are changed; other fields keep their
/// previous values. The `description_hash` PK is preserved even if the
/// description text is rewritten — agents that need to re-key under a new
/// hash should call `remove_decision` followed by `record_decision`.
///
/// # Errors
///
/// Returns `GraphError::InvalidInput` for an invalid nature or weight,
/// `GraphError::NodeNotFound` if no decisions row matches the hash, or
/// `GraphError::Storage` if the database operation fails.
pub fn update_decision(
    conn: &Arc<Mutex<Connection>>,
    params: UpdateDecisionParams,
) -> Result<UpdateDecisionData, GraphError> {
    let new_nature = match params.nature.as_deref() {
        Some(s) => Some(parse_nature(s)?),
        None => None,
    };
    let new_weight = match params.weight.as_deref() {
        Some(s) => Some(parse_weight(s)?),
        None => None,
    };

    let repo = SqliteDecisionRepository::new(conn.clone());
    let mut decision = repo
        .get_by_hash(&params.description_hash)
        .map_err(GraphError::Storage)?
        .ok_or_else(|| {
            GraphError::NodeNotFound(format!(
                "No decision found with description_hash {}",
                params.description_hash
            ))
        })?;

    // State guard (H2): only state='recorded' rows are mutable through this
    // path. Approved/rejected/partial rows are produced by the TUI review
    // flow; an MCP agent should not be able to silently clobber a user's
    // TUI-confirmed convention. Caller must remove + re-record to override.
    ensure_state_is_recorded(&decision, "update")?;

    let original_hash = decision.description_hash.clone();

    // If the description changed, recompute the content-derived PK and
    // migrate the row so the invariant `description_hash ==
    // compute_description_hash(description)` always holds. Storing a
    // stale PK silently breaks dedup-by-hash everywhere downstream
    // (persist_conventions skip-already-decided, MCP collision detection,
    // cross-branch propagation through G1).
    let mut rekey_target: Option<String> = None;
    if let Some(desc) = params.description {
        let new_hash = compute_description_hash(&desc);
        decision.description = desc;
        if new_hash != original_hash {
            // Refuse to overwrite a different existing row that already
            // lives at the new PK. Caller must remove + re-record to
            // merge intentionally.
            if repo
                .get_by_hash(&new_hash)
                .map_err(GraphError::Storage)?
                .is_some()
            {
                return Err(GraphError::InvalidInput(format!(
                    "Cannot update description: it hashes to {new_hash}, \
                     which collides with an existing decision. Use \
                     remove_decision on the colliding row first if you \
                     intend to merge them."
                )));
            }
            decision.description_hash = new_hash.clone();
            rekey_target = Some(new_hash);
        }
    }
    if let Some(n) = new_nature {
        decision.nature = n;
    }
    if let Some(w) = new_weight {
        decision.weight = w;
    }
    // P14: Some("") is a sentinel meaning "clear this field". Without it
    // there is no way to remove a category/reason once set: None means
    // "do not change", and a non-empty string overwrites in place. The
    // empty-string sentinel is borrowed from JSON-Patch and is what
    // most agent tooling produces when asked to "blank out" a field.
    if let Some(cat) = params.category {
        decision.category = if cat.is_empty() { None } else { Some(cat) };
    }
    if let Some(reason) = params.reason {
        decision.reason = if reason.is_empty() {
            None
        } else {
            Some(reason)
        };
    }
    if let Some(exs) = params.examples {
        decision.examples = exs.iter().map(Into::into).collect();
    }
    decision.updated_at = chrono::Utc::now().timestamp();

    if rekey_target.is_some() {
        repo.rekey(&original_hash, &decision)
            .map_err(GraphError::Storage)?;
    } else {
        repo.upsert(&decision).map_err(GraphError::Storage)?;
    }

    tracing::info!(
        description_hash = %decision.description_hash,
        previous_hash = %original_hash,
        description = %decision.description,
        nature = %decision.nature.as_sql_str(),
        weight = %decision.weight.as_sql_str(),
        "Updated user decision"
    );

    Ok(UpdateDecisionData {
        description_hash: decision.description_hash,
        description: decision.description,
        nature: nature_to_string(decision.nature),
        weight: weight_to_string(decision.weight),
        legacy_id: LEGACY_ID_SENTINEL,
    })
}

// ── Remove function ──────────────────────────────────────────

/// Hard-delete a user decision from the V12 `decisions` table.
///
/// Per US-004 AC the row is fully removed (no soft-delete column on the new
/// schema). The `reason` argument is logged for the audit trail but not
/// persisted — re-record the decision via `record_decision` if you change
/// your mind.
///
/// # Errors
///
/// Returns `GraphError::NodeNotFound` if no decisions row matches the hash,
/// or `GraphError::Storage` if the database operation fails.
pub fn remove_decision(
    conn: &Arc<Mutex<Connection>>,
    params: RemoveDecisionParams,
) -> Result<RemoveDecisionData, GraphError> {
    let repo = SqliteDecisionRepository::new(conn.clone());

    let existing = repo
        .get_by_hash(&params.description_hash)
        .map_err(GraphError::Storage)?
        .ok_or_else(|| {
            GraphError::NodeNotFound(format!(
                "No decision found with description_hash {}",
                params.description_hash
            ))
        })?;

    // State guard (H2): refuse to hard-delete TUI-confirmed/rejected/partial
    // rows through this path. The TUI review flow is the only legitimate
    // writer for those states; if an agent really wants to override a TUI
    // decision, they must use the TUI (or a future explicit
    // override-by-state flag).
    ensure_state_is_recorded(&existing, "remove")?;

    repo.delete(&params.description_hash)
        .map_err(GraphError::Storage)?;

    tracing::info!(
        description_hash = %params.description_hash,
        description = %existing.description,
        reason = %params.reason,
        "Removed user decision"
    );

    Ok(RemoveDecisionData {
        description_hash: params.description_hash,
        message: format!("Decision {} removed successfully", existing.description),
        legacy_id: LEGACY_ID_SENTINEL,
    })
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::test_conn;

    fn fetch_decision(conn: &Arc<Mutex<Connection>>, hash: &str) -> Option<Decision> {
        let repo = SqliteDecisionRepository::new(conn.clone());
        repo.get_by_hash(hash).unwrap()
    }

    /// Insert a decision row directly via the repo with the given state,
    /// simulating what the TUI confirm/reject/partial flow writes.
    /// Bypasses `record_decision` (which always writes state='recorded')
    /// so the H2 state guard can be exercised against approved /
    /// rejected / partial rows.
    fn insert_tui_decision(
        conn: &Arc<Mutex<Connection>>,
        description: &str,
        state: DecisionState,
    ) -> String {
        let repo = SqliteDecisionRepository::new(conn.clone());
        let hash = compute_description_hash(description);
        let now = chrono::Utc::now().timestamp();
        let decision = Decision {
            description_hash: hash.clone(),
            description: description.to_owned(),
            state,
            nature: DecisionNature::Convention,
            weight: DecisionWeight::Strong,
            category: None,
            reason: None,
            examples: vec![],
            decided_on_branch: BranchId("main".to_owned()),
            decided_at: now,
            updated_at: now,
        };
        repo.upsert(&decision).expect("seed TUI decision");
        hash
    }

    #[test]
    fn record_decision_writes_to_decisions_table_with_state_recorded() {
        let conn = test_conn();

        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Always use Result for fallible operations".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: Some("error-handling".to_owned()),
                examples: vec![],
                reason: Some("Explicit error handling is preferred".to_owned()),
            },
        )
        .unwrap();

        assert_eq!(result.nature, "decision");
        assert_eq!(result.weight, "strong");
        assert_eq!(
            result.description,
            "Always use Result for fallible operations"
        );
        assert!(!result.description_hash.is_empty());

        // Decisions table row was created with state='recorded'.
        let row = fetch_decision(&conn, &result.description_hash).expect("row exists");
        assert_eq!(row.state, DecisionState::Recorded);
        assert_eq!(row.nature, DecisionNature::Decision);
        assert_eq!(row.weight, DecisionWeight::Strong);
        assert_eq!(row.category.as_deref(), Some("error-handling"));
        assert_eq!(
            row.reason.as_deref(),
            Some("Explicit error handling is preferred")
        );
        assert_eq!(row.decided_on_branch, BranchId::from("main"));

        // No `nodes` row was created — the legacy user-source pattern is gone.
        let c = conn.lock().unwrap();
        let user_node_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE json_extract(ext_data, '$.source') = 'user'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            user_node_count, 0,
            "record_decision must not create user-source nodes"
        );
    }

    #[test]
    fn record_decision_persists_examples_as_json() {
        let conn = test_conn();

        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Use snake_case for variables".to_owned(),
                nature: "convention".to_owned(),
                weight: "rule".to_owned(),
                category: Some("naming".to_owned()),
                examples: vec![ExampleInput {
                    file: "src/main.rs".to_owned(),
                    line: 10,
                    end_line: 10,
                    snippet: "let my_variable = 42;".to_owned(),
                }],
                reason: Some("Rust convention".to_owned()),
            },
        )
        .unwrap();

        let row = fetch_decision(&conn, &result.description_hash).expect("row exists");
        assert_eq!(row.examples.len(), 1);
        assert_eq!(row.examples[0].file, "src/main.rs");
        assert_eq!(row.examples[0].line, 10);
        assert_eq!(row.examples[0].snippet, "let my_variable = 42;");
    }

    #[test]
    fn record_decision_invalid_nature_returns_error() {
        let conn = test_conn();

        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Test".to_owned(),
                nature: "invalid_nature".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid nature"));
    }

    #[test]
    fn record_decision_invalid_weight_returns_error() {
        let conn = test_conn();

        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Test".to_owned(),
                nature: "decision".to_owned(),
                weight: "moderate".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid weight"));
    }

    #[test]
    fn record_decision_case_insensitive_nature_and_weight() {
        let conn = test_conn();

        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Test case insensitive".to_owned(),
                nature: "Decision".to_owned(),
                weight: "Strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        assert_eq!(result.nature, "decision");
        assert_eq!(result.weight, "strong");
    }

    #[test]
    fn record_decision_upserts_on_repeat_for_same_hash() {
        let conn = test_conn();

        let first = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Use thiserror for error types".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: Some("first".to_owned()),
            },
        )
        .unwrap();

        // Re-record with the same description but different reason; the row is upserted.
        let second = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Use thiserror for error types".to_owned(),
                nature: "convention".to_owned(),
                weight: "rule".to_owned(),
                category: None,
                examples: vec![],
                reason: Some("second".to_owned()),
            },
        )
        .unwrap();

        assert_eq!(first.description_hash, second.description_hash);
        // Only one row exists.
        let repo = SqliteDecisionRepository::new(conn.clone());
        assert_eq!(repo.list().unwrap().len(), 1);

        let row = fetch_decision(&conn, &second.description_hash).unwrap();
        assert_eq!(row.nature, DecisionNature::Convention);
        assert_eq!(row.weight, DecisionWeight::Rule);
        assert_eq!(row.reason.as_deref(), Some("second"));
    }

    // ── update_decision tests ────────────────────────────────

    #[test]
    fn update_decision_modifies_description() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Original description".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        let updated = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: recorded.description_hash.clone(),
                description: Some("Updated description".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        )
        .unwrap();

        // PK migrates with the content change.
        assert_eq!(
            updated.description_hash,
            compute_description_hash("Updated description")
        );
        assert_ne!(updated.description_hash, recorded.description_hash);
        assert_eq!(updated.description, "Updated description");
        assert_eq!(updated.nature, "decision");
        assert_eq!(updated.weight, "strong");

        // Old PK is gone; fetch via new PK.
        assert!(fetch_decision(&conn, &recorded.description_hash).is_none());
        let row = fetch_decision(&conn, &updated.description_hash).unwrap();
        assert_eq!(row.description, "Updated description");
    }

    #[test]
    fn update_decision_modifies_nature_and_weight() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Test decision".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        let updated = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: recorded.description_hash.clone(),
                description: None,
                nature: Some("Convention".to_owned()),
                weight: Some("Rule".to_owned()),
                category: None,
                examples: None,
                reason: None,
            },
        )
        .unwrap();

        assert_eq!(updated.nature, "convention");
        assert_eq!(updated.weight, "rule");
        assert_eq!(updated.description, "Test decision"); // unchanged
    }

    #[test]
    fn update_decision_replaces_examples_and_reason_and_category() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Test".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: Some("old-category".to_owned()),
                examples: vec![],
                reason: Some("old reason".to_owned()),
            },
        )
        .unwrap();

        update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: recorded.description_hash.clone(),
                description: None,
                nature: None,
                weight: None,
                category: Some("new-category".to_owned()),
                examples: Some(vec![ExampleInput {
                    file: "src/lib.rs".to_owned(),
                    line: 5,
                    end_line: 10,
                    snippet: "fn example() {}".to_owned(),
                }]),
                reason: Some("new reason".to_owned()),
            },
        )
        .unwrap();

        let row = fetch_decision(&conn, &recorded.description_hash).unwrap();
        assert_eq!(row.category.as_deref(), Some("new-category"));
        assert_eq!(row.reason.as_deref(), Some("new reason"));
        assert_eq!(row.examples.len(), 1);
        assert_eq!(row.examples[0].file, "src/lib.rs");
        assert_eq!(row.examples[0].line, 5);
        assert_eq!(row.examples[0].end_line, 10);
    }

    #[test]
    fn update_decision_clears_category_and_reason_when_passed_empty_string() {
        // P14: Some("") sentinel = "clear this field" (None means
        // "no change"). Without this, callers can never blank a field
        // once set — only overwrite it.
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "P14 fixture".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: Some("category-to-clear".to_owned()),
                examples: vec![],
                reason: Some("reason-to-clear".to_owned()),
            },
        )
        .unwrap();
        // Sanity: both fields are set.
        let pre = fetch_decision(&conn, &recorded.description_hash).unwrap();
        assert!(pre.category.is_some());
        assert!(pre.reason.is_some());

        update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: recorded.description_hash.clone(),
                description: None,
                nature: None,
                weight: None,
                category: Some(String::new()),
                examples: None,
                reason: Some(String::new()),
            },
        )
        .unwrap();

        let post = fetch_decision(&conn, &recorded.description_hash).unwrap();
        assert!(
            post.category.is_none(),
            "Some(\"\") must clear category to None; got {:?}",
            post.category
        );
        assert!(
            post.reason.is_none(),
            "Some(\"\") must clear reason to None; got {:?}",
            post.reason
        );
    }

    #[test]
    fn update_decision_rekeys_pk_when_description_changes() {
        // Invariant: description_hash == compute_description_hash(description).
        // When the description changes, the PK must migrate to the new hash,
        // otherwise dedup-by-hash silently breaks (auto-detected scans
        // re-emit the convention as if no decision existed, MCP agents
        // looking up by recomputed hash get NotFound, etc).
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "First description".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();
        let original_hash = recorded.description_hash.clone();
        let new_desc = "Completely different description text";
        let expected_new_hash = compute_description_hash(new_desc);
        assert_ne!(original_hash, expected_new_hash);

        let updated = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: original_hash.clone(),
                description: Some(new_desc.to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        )
        .unwrap();

        // Returned hash matches the recomputed hash of the new description.
        assert_eq!(updated.description_hash, expected_new_hash);
        // Old PK is gone; new PK holds the row.
        assert!(fetch_decision(&conn, &original_hash).is_none());
        let migrated =
            fetch_decision(&conn, &expected_new_hash).expect("row exists at the recomputed hash");
        assert_eq!(migrated.description_hash, expected_new_hash);
        assert_eq!(migrated.description, new_desc);
        // The stored PK matches the hash of the stored description.
        assert_eq!(
            migrated.description_hash,
            compute_description_hash(&migrated.description),
            "PK ↔ description invariant must hold after update"
        );
    }

    #[test]
    fn update_decision_keeps_pk_when_description_unchanged() {
        // Updating only non-description fields must NOT rewrite the PK.
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Stable description".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();
        let original_hash = recorded.description_hash.clone();

        let updated = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: original_hash.clone(),
                description: None,
                nature: None,
                weight: None,
                category: Some("logging".to_owned()),
                examples: None,
                reason: Some("for traceability".to_owned()),
            },
        )
        .unwrap();

        assert_eq!(updated.description_hash, original_hash);
        let row = fetch_decision(&conn, &original_hash).expect("row at original PK");
        assert_eq!(row.category.as_deref(), Some("logging"));
        assert_eq!(row.reason.as_deref(), Some("for traceability"));
    }

    #[test]
    fn update_decision_rejects_description_change_that_collides_with_existing_row() {
        // If the new description hashes to a PK that already exists,
        // refuse to clobber it. Caller must explicitly remove the
        // colliding row first.
        let conn = test_conn();

        let first = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Use Result for fallible ops".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();
        let second = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Use anyhow for application errors".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // Reverse-engineer a description that hashes to `second`'s PK by
        // simply reusing its own text — the hash is deterministic.
        let colliding_desc = "Use anyhow for application errors";
        assert_eq!(
            compute_description_hash(colliding_desc),
            second.description_hash
        );

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: first.description_hash.clone(),
                description: Some(colliding_desc.to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        match result {
            Err(GraphError::InvalidInput(msg)) => {
                assert!(
                    msg.contains(&second.description_hash),
                    "error must mention the colliding hash, got: {msg}"
                );
            }
            other => panic!("expected InvalidInput, got: {other:?}"),
        }

        // Both original rows are intact: nothing was clobbered.
        let row_first = fetch_decision(&conn, &first.description_hash)
            .expect("first row must remain after rejected rekey");
        assert_eq!(row_first.description, "Use Result for fallible ops");
        let row_second = fetch_decision(&conn, &second.description_hash)
            .expect("second row must remain untouched");
        assert_eq!(row_second.description, "Use anyhow for application errors");
    }

    #[test]
    fn update_decision_hash_not_found() {
        let conn = test_conn();

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: "deadbeefcafebabe".to_owned(),
                description: Some("Updated".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            GraphError::NodeNotFound(msg) => assert!(msg.contains("deadbeefcafebabe")),
            other => panic!("expected NodeNotFound, got: {other}"),
        }
    }

    #[test]
    fn update_decision_invalid_nature_returns_error() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Test".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: recorded.description_hash,
                description: None,
                nature: Some("invalid".to_owned()),
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid nature"));
    }

    // ── remove_decision tests ────────────────────────────────

    #[test]
    fn remove_decision_hard_deletes_row() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Decision to remove".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        let result = remove_decision(
            &conn,
            RemoveDecisionParams {
                description_hash: recorded.description_hash.clone(),
                reason: "No longer relevant".to_owned(),
            },
        )
        .unwrap();

        assert_eq!(result.description_hash, recorded.description_hash);
        assert!(result.message.contains("removed successfully"));

        // Row is gone — hard delete.
        assert!(fetch_decision(&conn, &recorded.description_hash).is_none());
    }

    #[test]
    fn remove_decision_hash_not_found_returns_error() {
        let conn = test_conn();

        let result = remove_decision(
            &conn,
            RemoveDecisionParams {
                description_hash: "deadbeefcafebabe".to_owned(),
                reason: "Removing nonexistent".to_owned(),
            },
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            GraphError::NodeNotFound(msg) => assert!(msg.contains("deadbeefcafebabe")),
            other => panic!("expected NodeNotFound, got: {other}"),
        }
    }

    // ── End-to-end behaviour through query_convention ────────

    #[test]
    fn recorded_decision_visible_in_query_convention() {
        let conn = test_conn();

        record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Always wrap database errors with context".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                category: Some("error-handling".to_owned()),
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        let query_result = crate::conventions::query_convention(&conn, "main", "database").unwrap();
        assert!(
            !query_result.conventions.is_empty(),
            "recorded decision should appear in query_convention results"
        );
        assert_eq!(query_result.conventions[0].source, "user");
        assert!(query_result.conventions[0].user_confirmed);
    }

    #[test]
    fn query_convention_surfaces_state_and_reason_for_decisions() {
        // P9: state and reason from the V12 `decisions` table must reach
        // the MCP envelope so agents calling query_convention can see
        // (a) whether a row is `recorded`/`approved`/`partial` and
        // (b) the user-supplied rationale.
        let conn = test_conn();
        record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Never panic in async tasks".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: Some("incident-2025-04: panic crashed the runtime".to_owned()),
            },
        )
        .unwrap();

        let q = crate::conventions::query_convention(&conn, "main", "panic").unwrap();
        let row = q
            .conventions
            .iter()
            .find(|c| c.description == "Never panic in async tasks")
            .expect("recorded decision visible");
        assert_eq!(row.state.as_deref(), Some("recorded"));
        assert_eq!(
            row.reason.as_deref(),
            Some("incident-2025-04: panic crashed the runtime")
        );
    }

    #[test]
    fn search_decisions_uses_or_semantics_across_keywords() {
        // P10: a multi-word topic must match decisions whose description
        // contains ANY of the keywords (FTS5-default OR semantics on the
        // node side; pre-fix the decisions side used AND, producing
        // asymmetric recall between equivalent decisions and nodes).
        let conn = test_conn();
        record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Use thiserror for library crates".to_owned(),
                nature: "decision".to_owned(),
                weight: "rule".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // "logging thiserror" should still find the row even though
        // "logging" is absent — pre-fix it returned empty.
        let q = crate::conventions::query_convention(&conn, "main", "logging thiserror").unwrap();
        assert!(
            q.conventions
                .iter()
                .any(|c| c.description.contains("thiserror")),
            "OR-of-keywords must surface decisions matching any token; got: {:?}",
            q.conventions
        );
    }

    #[test]
    fn recorded_decision_survives_rescan_dedup() {
        // Decisions are project-wide and untouched by re-scans of the nodes
        // table — that property is the whole point of the V12 schema.
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Never use unwrap in production code".to_owned(),
                nature: "decision".to_owned(),
                weight: "rule".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // Simulate re-scan: delete auto-detected nodes (the persist_conventions
        // dedup path post-US-008).
        {
            let c = conn.lock().unwrap();
            c.execute(
                "DELETE FROM nodes WHERE branch_id = 'main'
                 AND json_extract(ext_data, '$.source') = 'auto_detected'",
                [],
            )
            .unwrap();
        }

        // The decision survives in the decisions table.
        assert!(fetch_decision(&conn, &recorded.description_hash).is_some());
    }

    // ── H2: state guard tests ─────────────────────────────────

    #[test]
    fn update_decision_refuses_approved_state() {
        let conn = test_conn();
        let hash = insert_tui_decision(&conn, "tui-approved row", DecisionState::Approved);

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: hash.clone(),
                description: Some("would clobber TUI approval".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        match result {
            Err(GraphError::NotUserDecision(msg)) => {
                assert!(msg.contains(&hash), "error must mention hash: {msg}");
                assert!(
                    msg.contains("approved"),
                    "error must mention the offending state: {msg}"
                );
            }
            other => panic!("expected NotUserDecision, got: {other:?}"),
        }

        // Row is unchanged: still state=Approved with original description.
        let row = fetch_decision(&conn, &hash).expect("row preserved");
        assert_eq!(row.state, DecisionState::Approved);
        assert_eq!(row.description, "tui-approved row");
    }

    #[test]
    fn update_decision_refuses_rejected_state() {
        let conn = test_conn();
        let hash = insert_tui_decision(&conn, "tui-rejected row", DecisionState::Rejected);

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: hash.clone(),
                description: None,
                nature: None,
                weight: None,
                category: Some("logging".to_owned()),
                examples: None,
                reason: None,
            },
        );

        assert!(matches!(result, Err(GraphError::NotUserDecision(_))));
        let row = fetch_decision(&conn, &hash).expect("row preserved");
        assert_eq!(row.state, DecisionState::Rejected);
        assert!(row.category.is_none());
    }

    #[test]
    fn update_decision_refuses_partial_state() {
        let conn = test_conn();
        let hash = insert_tui_decision(&conn, "tui-partial row", DecisionState::Partial);

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: hash.clone(),
                description: None,
                nature: Some("convention".to_owned()),
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        assert!(matches!(result, Err(GraphError::NotUserDecision(_))));
        let row = fetch_decision(&conn, &hash).expect("row preserved");
        assert_eq!(row.state, DecisionState::Partial);
    }

    #[test]
    fn remove_decision_refuses_approved_state() {
        let conn = test_conn();
        let hash = insert_tui_decision(&conn, "tui-approved row", DecisionState::Approved);

        let result = remove_decision(
            &conn,
            RemoveDecisionParams {
                description_hash: hash.clone(),
                reason: "agent decided to clean up".to_owned(),
            },
        );

        match result {
            Err(GraphError::NotUserDecision(msg)) => {
                assert!(msg.contains(&hash));
                assert!(msg.contains("approved"));
            }
            other => panic!("expected NotUserDecision, got: {other:?}"),
        }

        // Row survived.
        assert!(fetch_decision(&conn, &hash).is_some());
    }

    #[test]
    fn remove_decision_refuses_rejected_state() {
        let conn = test_conn();
        let hash = insert_tui_decision(&conn, "tui-rejected row", DecisionState::Rejected);

        let result = remove_decision(
            &conn,
            RemoveDecisionParams {
                description_hash: hash.clone(),
                reason: "agent cleanup".to_owned(),
            },
        );

        assert!(matches!(result, Err(GraphError::NotUserDecision(_))));
        assert!(fetch_decision(&conn, &hash).is_some());
    }

    #[test]
    fn remove_decision_refuses_partial_state() {
        let conn = test_conn();
        let hash = insert_tui_decision(&conn, "tui-partial row", DecisionState::Partial);

        let result = remove_decision(
            &conn,
            RemoveDecisionParams {
                description_hash: hash.clone(),
                reason: "agent cleanup".to_owned(),
            },
        );

        assert!(matches!(result, Err(GraphError::NotUserDecision(_))));
        assert!(fetch_decision(&conn, &hash).is_some());
    }

    #[test]
    fn update_decision_allows_recorded_state() {
        // Sanity / regression: state='recorded' rows (the MCP-owned subset)
        // remain mutable through update_decision. Without this we'd have
        // accidentally locked out the legitimate caller.
        let conn = test_conn();
        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "agent-recorded".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        let updated = update_decision(
            &conn,
            UpdateDecisionParams {
                description_hash: recorded.description_hash,
                description: None,
                nature: None,
                weight: None,
                category: Some("naming".to_owned()),
                examples: None,
                reason: None,
            },
        );
        assert!(
            updated.is_ok(),
            "recorded rows must remain mutable: {updated:?}"
        );
    }

    #[test]
    fn remove_decision_allows_recorded_state() {
        let conn = test_conn();
        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "agent-recorded".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        let removed = remove_decision(
            &conn,
            RemoveDecisionParams {
                description_hash: recorded.description_hash.clone(),
                reason: "no longer relevant".to_owned(),
            },
        );
        assert!(removed.is_ok(), "recorded rows must remain removable");
        assert!(fetch_decision(&conn, &recorded.description_hash).is_none());
    }
}
