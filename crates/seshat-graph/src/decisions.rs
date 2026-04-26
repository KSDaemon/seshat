//! Record, update, and remove user-recorded decisions in the knowledge graph.
//!
//! User-recorded decisions are stored as `KnowledgeNode` rows in the `nodes`
//! table with `ext_data.source = "user"`. They are NEVER touched by automated
//! re-scanning — only explicit MCP tool calls can create, modify, or remove
//! them.
//!
//! Removal is a soft-delete: `ext_data.removed = true` with a reason and
//! timestamp. Removed decisions are filtered out by `query_convention` and
//! `query_project_context`.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::SOURCE_USER;
use crate::error::GraphError;
use crate::fts;

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

/// Parameters for updating an existing decision.
pub struct UpdateDecisionParams {
    /// Node ID to update (required).
    pub id: i64,
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

/// Parameters for removing (soft-deleting) a decision.
pub struct RemoveDecisionParams {
    /// Node ID to remove (required).
    pub id: i64,
    /// Reason for removal (required).
    pub reason: String,
}

// ── Response types ───────────────────────────────────────────

/// Response data for `record_decision`.
#[derive(Debug, Clone, Serialize)]
pub struct RecordDecisionData {
    /// The assigned node ID.
    pub id: i64,
    /// The description that was recorded.
    pub description: String,
    /// The nature that was set.
    pub nature: String,
    /// The weight that was set.
    pub weight: String,
}

/// Response data for `update_decision`.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateDecisionData {
    /// The updated node ID.
    pub id: i64,
    /// The current description after update.
    pub description: String,
    /// The current nature after update.
    pub nature: String,
    /// The current weight after update.
    pub weight: String,
}

/// Response data for `remove_decision`.
#[derive(Debug, Clone, Serialize)]
pub struct RemoveDecisionData {
    /// The removed node ID.
    pub id: i64,
    /// Confirmation message.
    pub message: String,
}

// ── Record function ──────────────────────────────────────────

/// Valid nature values for user-recorded decisions.
const VALID_NATURES: &[&str] = &["decision", "convention", "preference"];

/// Valid weight values for user-recorded decisions.
const VALID_WEIGHTS: &[&str] = &["rule", "strong"];

/// Record a new user decision in the knowledge graph.
///
/// Creates a `KnowledgeNode` with `ext_data.source = "user"` and immediately
/// indexes it in FTS5 so it appears in `query_convention` results.
///
/// # Errors
///
/// Returns `GraphError` if the database operation fails.
pub fn record_decision(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    params: RecordDecisionParams,
) -> Result<RecordDecisionData, GraphError> {
    // Validate nature.
    let nature = params.nature.to_lowercase();
    if !VALID_NATURES.contains(&nature.as_str()) {
        return Err(GraphError::InvalidInput(format!(
            "Invalid nature '{}'. Must be one of: decision, convention, preference",
            params.nature
        )));
    }

    // Validate weight.
    let weight = params.weight.to_lowercase();
    if !VALID_WEIGHTS.contains(&weight.as_str()) {
        return Err(GraphError::InvalidInput(format!(
            "Invalid weight '{}'. Must be one of: rule, strong",
            params.weight
        )));
    }

    // Build ext_data JSON.
    let mut ext = serde_json::Map::new();
    ext.insert("source".into(), SOURCE_USER.into());
    ext.insert("user_confirmed".into(), true.into());

    if let Some(ref category) = params.category {
        ext.insert("category".into(), category.clone().into());
    }

    if let Some(ref reason) = params.reason {
        ext.insert("reason".into(), reason.clone().into());
    }

    // Add examples as evidence array.
    if !params.examples.is_empty() {
        let evidence: Vec<serde_json::Value> = params
            .examples
            .iter()
            .map(|ex| {
                serde_json::json!({
                    "file": ex.file,
                    "line": ex.line,
                    "end_line": ex.end_line,
                    "snippet": ex.snippet,
                })
            })
            .collect();
        ext.insert("evidence".into(), serde_json::Value::Array(evidence));
    }

    let ext_data_str = serde_json::Value::Object(ext).to_string();

    // Compute description hash for deduplication.
    let description_hash = compute_description_hash(&params.description);

    // Insert the node.
    let conn_guard = crate::lock_conn(conn)?;

    conn_guard
         .execute(
             "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data, description_hash)
              VALUES (?1, ?2, ?3, 1.0, 1, 1, ?4, ?5, ?6)",
            params![branch_id, nature, weight, params.description, ext_data_str, description_hash],
          )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to insert decision node: {e}"
            )))
        })?;

    let node_id = conn_guard.last_insert_rowid();

    // Release the lock before calling FTS (which also acquires it).
    drop(conn_guard);

    // Index in FTS5 for searchability.
    let detector_name = params.category.as_deref().unwrap_or("");
    fts::insert_fts_entry(
        conn,
        seshat_core::NodeId(node_id),
        &params.description,
        detector_name,
    )?;

    tracing::info!(
        node_id,
        description = %params.description,
        nature = %nature,
        weight = %weight,
        "Recorded user decision"
    );

    Ok(RecordDecisionData {
        id: node_id,
        description: params.description,
        nature,
        weight,
    })
}

// ── Update function ──────────────────────────────────────────

/// Update an existing user decision in the knowledge graph.
///
/// Only nodes with `ext_data.source = "user"` can be updated. Auto-detected
/// conventions return `GraphError::NotUserDecision`. Modified fields are
/// merged into the existing node; unspecified fields remain unchanged.
///
/// After updating, the FTS5 index is refreshed: the old entry is deleted
/// and a new one is inserted with the (potentially) updated description.
///
/// # Errors
///
/// Returns `GraphError` if the node does not exist, is not a user decision,
/// or the database operation fails.
pub fn update_decision(
    conn: &Arc<Mutex<Connection>>,
    params: UpdateDecisionParams,
) -> Result<UpdateDecisionData, GraphError> {
    // Validate nature if provided.
    if let Some(ref nature) = params.nature {
        let n = nature.to_lowercase();
        if !VALID_NATURES.contains(&n.as_str()) {
            return Err(GraphError::InvalidInput(format!(
                "Invalid nature '{}'. Must be one of: decision, convention, preference",
                nature
            )));
        }
    }

    // Validate weight if provided.
    if let Some(ref weight) = params.weight {
        let w = weight.to_lowercase();
        if !VALID_WEIGHTS.contains(&w.as_str()) {
            return Err(GraphError::InvalidInput(format!(
                "Invalid weight '{}'. Must be one of: rule, strong",
                weight
            )));
        }
    }

    let conn_guard = crate::lock_conn(conn)?;

    // Load the existing node.
    let row = conn_guard
        .query_row(
            "SELECT nature, weight, description, ext_data FROM nodes WHERE id = ?1",
            params![params.id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                GraphError::NodeNotFound(format!("Node with id {} not found", params.id))
            }
            other => GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to load node {}: {other}",
                params.id
            ))),
        })?;

    let (current_nature, current_weight, current_description, ext_data_str) = row;

    // Parse ext_data to check source.
    let mut ext: serde_json::Map<String, serde_json::Value> = ext_data_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let source = ext.get("source").and_then(|v| v.as_str()).unwrap_or("");

    if source != SOURCE_USER {
        return Err(GraphError::NotUserDecision(format!(
            "Node {} has source '{}' — only user-recorded decisions can be updated",
            params.id, source
        )));
    }

    // Merge updated fields.
    let new_nature = params
        .nature
        .as_ref()
        .map(|n| n.to_lowercase())
        .unwrap_or(current_nature);
    let new_weight = params
        .weight
        .as_ref()
        .map(|w| w.to_lowercase())
        .unwrap_or(current_weight);
    let new_description = params.description.unwrap_or(current_description);

    // Update ext_data fields.
    if let Some(ref category) = params.category {
        ext.insert("category".into(), category.clone().into());
    }
    if let Some(ref reason) = params.reason {
        ext.insert("reason".into(), reason.clone().into());
    }
    if let Some(ref examples) = params.examples {
        let evidence: Vec<serde_json::Value> = examples
            .iter()
            .map(|ex| {
                serde_json::json!({
                    "file": ex.file,
                    "line": ex.line,
                    "end_line": ex.end_line,
                    "snippet": ex.snippet,
                })
            })
            .collect();
        ext.insert("evidence".into(), serde_json::Value::Array(evidence));
    }

    let ext_data_str = serde_json::Value::Object(ext).to_string();

    // Update the node.
    conn_guard
        .execute(
            "UPDATE nodes SET nature = ?1, weight = ?2, description = ?3, ext_data = ?4
             WHERE id = ?5",
            params![
                new_nature,
                new_weight,
                new_description,
                ext_data_str,
                params.id
            ],
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to update decision node {}: {e}",
                params.id
            )))
        })?;

    // Release the lock before FTS operations.
    drop(conn_guard);

    // Re-index in FTS5: delete old entry + insert new one.
    let node_id = seshat_core::NodeId(params.id);
    fts::delete_fts_entry(conn, node_id)?;

    let detector_name = params.category.as_deref().unwrap_or("");
    fts::insert_fts_entry(conn, node_id, &new_description, detector_name)?;

    tracing::info!(
        node_id = params.id,
        description = %new_description,
        nature = %new_nature,
        weight = %new_weight,
        "Updated user decision"
    );

    Ok(UpdateDecisionData {
        id: params.id,
        description: new_description,
        nature: new_nature,
        weight: new_weight,
    })
}

// ── Remove function ──────────────────────────────────────────

/// Soft-delete a user decision from the knowledge graph.
///
/// Sets `ext_data.removed = true` with a reason and ISO-8601 timestamp.
/// The node remains in the database for audit trail purposes but is
/// excluded from `query_convention` and `query_project_context` results.
///
/// Also removes the node from the FTS5 index so it no longer appears in
/// full-text searches.
///
/// # Errors
///
/// Returns `GraphError` if the node does not exist, is not a user decision,
/// or the database operation fails.
pub fn remove_decision(
    conn: &Arc<Mutex<Connection>>,
    params: RemoveDecisionParams,
) -> Result<RemoveDecisionData, GraphError> {
    let conn_guard = crate::lock_conn(conn)?;

    // Load the existing node.
    let ext_data_str: Option<String> = conn_guard
        .query_row(
            "SELECT ext_data FROM nodes WHERE id = ?1",
            params![params.id],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                GraphError::NodeNotFound(format!("Node with id {} not found", params.id))
            }
            other => GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to load node {}: {other}",
                params.id
            ))),
        })?;

    // Parse ext_data to check source.
    let mut ext: serde_json::Map<String, serde_json::Value> = ext_data_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let source = ext.get("source").and_then(|v| v.as_str()).unwrap_or("");

    if source != SOURCE_USER {
        return Err(GraphError::NotUserDecision(format!(
            "Node {} has source '{}' — only user-recorded decisions can be removed",
            params.id, source
        )));
    }

    // Set removed fields.
    ext.insert("removed".into(), serde_json::Value::Bool(true));
    ext.insert("removed_reason".into(), params.reason.clone().into());

    // Unix epoch seconds (integer, not ISO-8601 string).
    let now = chrono::Utc::now().timestamp();
    ext.insert("removed_at".into(), serde_json::Value::Number(now.into()));

    let ext_data_str = serde_json::Value::Object(ext).to_string();

    // Update the node's ext_data with removed flag.
    conn_guard
        .execute(
            "UPDATE nodes SET ext_data = ?1 WHERE id = ?2",
            params![ext_data_str, params.id],
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to remove decision node {}: {e}",
                params.id
            )))
        })?;

    // Release the lock before FTS operations.
    drop(conn_guard);

    // Remove from FTS5 index.
    fts::delete_fts_entry(conn, seshat_core::NodeId(params.id))?;

    tracing::info!(
        node_id = params.id,
        reason = %params.reason,
        "Soft-deleted user decision"
    );

    Ok(RemoveDecisionData {
        id: params.id,
        message: format!("Decision {} removed successfully", params.id),
    })
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::test_conn;

    #[test]
    fn record_decision_creates_node() {
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

        assert!(result.id > 0);
        assert_eq!(
            result.description,
            "Always use Result for fallible operations"
        );
        assert_eq!(result.nature, "decision");
        assert_eq!(result.weight, "strong");

        // Verify the node exists in the DB.
        let c = conn.lock().unwrap();
        let description: String = c
            .query_row(
                "SELECT description FROM nodes WHERE id = ?1",
                params![result.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(description, "Always use Result for fallible operations");
    }

    #[test]
    fn record_decision_sets_ext_data_correctly() {
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

        let c = conn.lock().unwrap();
        let ext_data_str: String = c
            .query_row(
                "SELECT ext_data FROM nodes WHERE id = ?1",
                params![result.id],
                |row| row.get(0),
            )
            .unwrap();

        let ext: serde_json::Value = serde_json::from_str(&ext_data_str).unwrap();
        assert_eq!(ext["source"], "user");
        assert_eq!(ext["user_confirmed"], true);
        assert_eq!(ext["category"], "naming");
        assert_eq!(ext["reason"], "Rust convention");
        assert!(ext["evidence"].is_array());
        assert_eq!(ext["evidence"][0]["file"], "src/main.rs");
        assert_eq!(ext["evidence"][0]["line"], 10);
        assert_eq!(ext["evidence"][0]["snippet"], "let my_variable = 42;");
    }

    #[test]
    fn record_decision_indexed_in_fts5() {
        let conn = test_conn();

        record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Use structured logging with tracing".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // Search FTS5 for the decision.
        let results = fts::search_conventions(&conn, "logging").unwrap();
        assert_eq!(results.len(), 1, "decision should be searchable via FTS5");
    }

    #[test]
    fn record_decision_visible_in_query_convention() {
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

        // Query convention should find it.
        let query_result = crate::conventions::query_convention(&conn, "main", "database").unwrap();
        assert!(
            !query_result.conventions.is_empty(),
            "recorded decision should appear in query_convention results"
        );
        assert_eq!(query_result.conventions[0].source, "user");
        assert!(query_result.conventions[0].user_confirmed);
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
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Invalid nature"),
            "error should mention invalid nature: {err}"
        );
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
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Invalid weight"),
            "error should mention invalid weight: {err}"
        );
    }

    #[test]
    fn record_decision_case_insensitive_nature() {
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
    fn record_decision_not_deleted_by_rescan() {
        let conn = test_conn();

        // Record a user decision.
        let result = record_decision(
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

        // Simulate re-scan: delete auto-detected nodes.
        {
            let c = conn.lock().unwrap();
            c.execute(
                "DELETE FROM nodes WHERE branch_id = 'main'
                 AND json_extract(ext_data, '$.source') = 'auto_detected'",
                [],
            )
            .unwrap();
        }

        // The user decision should still be there.
        let c = conn.lock().unwrap();
        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE id = ?1",
                params![result.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "user decision should survive re-scan deletion");
    }

    #[test]
    fn record_decision_defaults_work() {
        let conn = test_conn();

        // Nature defaults to decision, weight defaults to strong (verified at handler level).
        // At graph level, we pass explicit values.
        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Minimal decision".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // Verify ext_data doesn't have category or reason keys when not provided.
        let c = conn.lock().unwrap();
        let ext_data_str: String = c
            .query_row(
                "SELECT ext_data FROM nodes WHERE id = ?1",
                params![result.id],
                |row| row.get(0),
            )
            .unwrap();

        let ext: serde_json::Value = serde_json::from_str(&ext_data_str).unwrap();
        assert_eq!(ext["source"], "user");
        assert_eq!(ext["user_confirmed"], true);
        assert!(ext.get("category").is_none());
        assert!(ext.get("reason").is_none());
        assert!(ext.get("evidence").is_none());
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
                id: recorded.id,
                description: Some("Updated description".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        )
        .unwrap();

        assert_eq!(updated.id, recorded.id);
        assert_eq!(updated.description, "Updated description");
        assert_eq!(updated.nature, "decision");
        assert_eq!(updated.weight, "strong");

        // Verify in DB.
        let c = conn.lock().unwrap();
        let desc: String = c
            .query_row(
                "SELECT description FROM nodes WHERE id = ?1",
                params![recorded.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(desc, "Updated description");
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
                id: recorded.id,
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
    fn update_decision_updates_ext_data_fields() {
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
                id: recorded.id,
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

        let c = conn.lock().unwrap();
        let ext_data_str: String = c
            .query_row(
                "SELECT ext_data FROM nodes WHERE id = ?1",
                params![recorded.id],
                |row| row.get(0),
            )
            .unwrap();

        let ext: serde_json::Value = serde_json::from_str(&ext_data_str).unwrap();
        assert_eq!(ext["category"], "new-category");
        assert_eq!(ext["reason"], "new reason");
        assert!(ext["evidence"].is_array());
        assert_eq!(ext["evidence"][0]["file"], "src/lib.rs");
    }

    #[test]
    fn update_decision_re_indexes_fts5() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Original logging decision".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // Should find via original description.
        let results = fts::search_conventions(&conn, "logging").unwrap();
        assert!(!results.is_empty());

        // Update description.
        update_decision(
            &conn,
            UpdateDecisionParams {
                id: recorded.id,
                description: Some("Updated testing convention".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        )
        .unwrap();

        // Old description should not match.
        let results = fts::search_conventions(&conn, "logging").unwrap();
        assert!(
            results.is_empty(),
            "old description should no longer match FTS5"
        );

        // New description should match.
        let results = fts::search_conventions(&conn, "testing").unwrap();
        assert!(!results.is_empty(), "new description should match FTS5");
    }

    #[test]
    fn update_decision_node_not_found() {
        let conn = test_conn();

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                id: 99999,
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
            GraphError::NodeNotFound(msg) => assert!(msg.contains("99999")),
            other => panic!("expected NodeNotFound, got: {other}"),
        }
    }

    #[test]
    fn update_decision_auto_detected_returns_error() {
        let conn = test_conn();

        // Insert an auto-detected node directly.
        let node_id = {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.9, 10, 12, 'Auto convention', ?1)",
                params![serde_json::json!({"source": "auto_detected", "detector_name": "test"}).to_string()],
            )
            .unwrap();
            c.last_insert_rowid()
        };

        let result = update_decision(
            &conn,
            UpdateDecisionParams {
                id: node_id,
                description: Some("Should fail".to_owned()),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            GraphError::NotUserDecision(msg) => {
                assert!(msg.contains("auto_detected"));
            }
            other => panic!("expected NotUserDecision, got: {other}"),
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
                id: recorded.id,
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
    fn remove_decision_soft_deletes() {
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
                id: recorded.id,
                reason: "No longer relevant".to_owned(),
            },
        )
        .unwrap();

        assert_eq!(result.id, recorded.id);
        assert!(result.message.contains("removed successfully"));

        // Node should still exist in DB.
        let c = conn.lock().unwrap();
        let ext_data_str: String = c
            .query_row(
                "SELECT ext_data FROM nodes WHERE id = ?1",
                params![recorded.id],
                |row| row.get(0),
            )
            .unwrap();

        let ext: serde_json::Value = serde_json::from_str(&ext_data_str).unwrap();
        assert_eq!(ext["removed"], true);
        assert_eq!(ext["removed_reason"], "No longer relevant");
        assert!(ext["removed_at"].is_number());
    }

    #[test]
    fn remove_decision_hides_from_query_convention() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Decision about error handling".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // Should be visible before removal.
        let results = crate::conventions::query_convention(&conn, "main", "error").unwrap();
        assert!(!results.conventions.is_empty());

        // Remove it.
        remove_decision(
            &conn,
            RemoveDecisionParams {
                id: recorded.id,
                reason: "Deprecated".to_owned(),
            },
        )
        .unwrap();

        // Should no longer appear in query_convention results.
        let results = crate::conventions::query_convention(&conn, "main", "error").unwrap();
        assert!(
            results.conventions.is_empty(),
            "removed decision should not appear in query_convention"
        );
    }

    #[test]
    fn remove_decision_removes_from_fts5() {
        let conn = test_conn();

        let recorded = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Decision about database patterns".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();

        // Should be searchable.
        let results = fts::search_conventions(&conn, "database").unwrap();
        assert!(!results.is_empty());

        // Remove it.
        remove_decision(
            &conn,
            RemoveDecisionParams {
                id: recorded.id,
                reason: "Not needed".to_owned(),
            },
        )
        .unwrap();

        // Should no longer be searchable.
        let results = fts::search_conventions(&conn, "database").unwrap();
        assert!(results.is_empty(), "removed decision should not be in FTS5");
    }

    #[test]
    fn remove_decision_node_not_found() {
        let conn = test_conn();

        let result = remove_decision(
            &conn,
            RemoveDecisionParams {
                id: 99999,
                reason: "Removing nonexistent".to_owned(),
            },
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            GraphError::NodeNotFound(msg) => assert!(msg.contains("99999")),
            other => panic!("expected NodeNotFound, got: {other}"),
        }
    }

    #[test]
    fn remove_decision_auto_detected_returns_error() {
        let conn = test_conn();

        // Insert an auto-detected node directly.
        let node_id = {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.9, 10, 12, 'Auto convention', ?1)",
                params![serde_json::json!({"source": "auto_detected", "detector_name": "test"}).to_string()],
            )
            .unwrap();
            c.last_insert_rowid()
        };

        let result = remove_decision(
            &conn,
            RemoveDecisionParams {
                id: node_id,
                reason: "Should fail".to_owned(),
            },
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            GraphError::NotUserDecision(msg) => {
                assert!(msg.contains("auto_detected"));
            }
            other => panic!("expected NotUserDecision, got: {other}"),
        }
    }

    #[test]
    fn confirm_convention_creates_user_decision() {
        let conn = test_conn();

        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Import grouping: stdlib -> external -> internal".to_owned(),
                nature: "convention".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: Some("Confirmed via seshat review TUI".to_owned()),
            },
        )
        .unwrap();

        assert!(result.id > 0);
        assert_eq!(result.nature, "convention");
        assert_eq!(result.weight, "strong");

        let c = conn.lock().unwrap();
        let ext_data_str: String = c
            .query_row(
                "SELECT ext_data FROM nodes WHERE id = ?1",
                params![result.id],
                |row| row.get(0),
            )
            .unwrap();
        let ext: serde_json::Value = serde_json::from_str(&ext_data_str).unwrap();
        assert_eq!(ext["source"].as_str().unwrap(), "user");
        assert!(ext["user_confirmed"].as_bool().unwrap());
    }

    #[test]
    fn partial_convention_creates_preference() {
        let conn = test_conn();

        let result = record_decision(
            &conn,
            "main",
            RecordDecisionParams {
                description: "Partial: Use snake_case for variables".to_owned(),
                nature: "preference".to_owned(),
                weight: "strong".to_owned(),
                category: None,
                examples: vec![],
                reason: Some("Partially confirmed via seshat review TUI".to_owned()),
            },
        )
        .unwrap();

        assert!(result.id > 0);
        assert_eq!(result.nature, "preference");
        assert!(result.description.starts_with("Partial: "));
    }

    #[test]
    fn reject_auto_detected_marks_user_rejected() {
        let conn = test_conn();

        let node_id = {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.9, 10, 12, 'Auto convention', ?1)",
                params![serde_json::json!({"source": "auto_detected", "detector_name": "test"}).to_string()],
            )
            .unwrap();
            c.last_insert_rowid()
        };

        let now = chrono::Utc::now().timestamp();
        {
            let mut ext = serde_json::json!({"source": "auto_detected", "detector_name": "test"});
            ext["removed"] = serde_json::json!(1);
            ext["removed_reason"] = serde_json::json!("Rejected via seshat review TUI");
            ext["removed_at"] = serde_json::json!(now);
            ext["user_rejected"] = serde_json::json!(1);
            let c = conn.lock().unwrap();
            c.execute(
                "UPDATE nodes SET ext_data = ?1 WHERE id = ?2",
                params![ext.to_string(), node_id],
            )
            .unwrap();
        }

        let c = conn.lock().unwrap();
        let ext_data_str: String = c
            .query_row(
                "SELECT ext_data FROM nodes WHERE id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .unwrap();
        let ext: serde_json::Value = serde_json::from_str(&ext_data_str).unwrap();
        assert_eq!(ext["user_rejected"].as_i64().unwrap(), 1);
        assert_eq!(ext["removed"].as_i64().unwrap(), 1);
    }

    #[test]
    fn reject_auto_detected_removes_from_fts5() {
        let conn = test_conn();

        let node_id = {
            let c = conn.lock().unwrap();
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES ('main', 'convention', 'strong', 0.9, 10, 12, 'FTS reject test', ?1)",
                params![serde_json::json!({"source": "auto_detected", "detector_name": "test"}).to_string()],
            )
            .unwrap();
            c.last_insert_rowid()
        };

        fts::insert_fts_entry(
            &conn,
            seshat_core::NodeId(node_id),
            "FTS reject test",
            "test",
        )
        .unwrap();
        fts::delete_fts_entry(&conn, seshat_core::NodeId(node_id)).unwrap();

        let results = fts::search_conventions(&conn, "FTS reject test").unwrap();
        assert!(
            results.is_empty(),
            "rejected convention should not be in FTS5"
        );
    }
}
