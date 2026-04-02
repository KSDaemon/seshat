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

use crate::error::GraphError;
use crate::fts;

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
    ext.insert("source".into(), "user".into());
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

    // Insert the node.
    let conn_guard = conn.lock().map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "Failed to acquire connection lock: {e}"
        )))
    })?;

    conn_guard
        .execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             VALUES (?1, ?2, ?3, 1.0, 1, 1, ?4, ?5)",
            params![branch_id, nature, weight, params.description, ext_data_str],
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

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_storage::Database;

    /// Open in-memory DB and return its connection.
    fn test_conn() -> Arc<Mutex<Connection>> {
        let db = Database::open(":memory:").expect("in-memory DB");
        db.connection().clone()
    }

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
}
