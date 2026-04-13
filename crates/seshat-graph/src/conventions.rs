//! Convention lookup by topic for the `query_convention` MCP tool.
//!
//! Searches conventions via FTS5 full-text search on descriptions, then
//! enriches each matching node with adoption metrics, trend, evidence,
//! and source information.
//!
//! All queries run against the SQLite database — no filesystem access needed.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;
use seshat_core::{CodeSnippet, truncate_snippet};

use crate::error::GraphError;
use crate::fts;
use crate::{SOURCE_USER, SQL_NOT_REMOVED};

// ── Response data types ──────────────────────────────────────

/// Full response data for the `query_convention` tool.
#[derive(Debug, Clone, Serialize)]
pub struct QueryConventionData {
    /// Convention results matching the topic query.
    pub conventions: Vec<ConventionResult>,
}

/// A single convention result with full enrichment.
#[derive(Debug, Clone, Serialize)]
pub struct ConventionResult {
    /// Nature of the knowledge (convention, observation, decision, etc.).
    pub nature: String,
    /// Weight/authoritativeness (rule, strong, moderate, weak, info).
    pub weight: String,
    /// Confidence score as integer percentage (0–100).
    pub confidence_pct: u32,
    /// Adoption metrics.
    pub adoption: AdoptionInfo,
    /// Trend indicator (rising, stable, declining, unknown).
    pub trend: String,
    /// Human-readable description of the convention.
    pub description: String,
    /// Source of the convention (auto_detected or user).
    pub source: String,
    /// Whether the convention was confirmed by a user.
    pub user_confirmed: bool,
    /// Evidence examples from the codebase.
    pub examples: Vec<EvidenceExample>,
}

/// Adoption metrics for a convention.
#[derive(Debug, Clone, Serialize)]
pub struct AdoptionInfo {
    /// Number of files/instances following this convention.
    pub count: u32,
    /// Total number of files/instances evaluated.
    pub total: u32,
    /// Adoption rate as integer percentage (0–100).
    pub rate_pct: u32,
}

/// A code evidence example from the codebase.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceExample {
    /// File path where the evidence was found.
    pub file: String,
    /// Start line number.
    pub line: u32,
    /// End line number.
    pub end_line: u32,
    /// Code snippet (may be truncated).
    pub snippet: CodeSnippet,
}

/// Query conventions matching a topic via FTS5 full-text search.
///
/// Searches the `conventions_fts` table, then loads full node data from `nodes`
/// and enriches each result with adoption, trend, evidence, and source info.
///
/// Returns `Ok` with an empty `conventions` vec when nothing matches (not an error).
pub fn query_convention(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    topic: &str,
) -> Result<QueryConventionData, GraphError> {
    // Search FTS5 index for matching node IDs.
    let node_ids = fts::search_conventions(conn, topic)?;

    if node_ids.is_empty() {
        return Ok(QueryConventionData {
            conventions: Vec::new(),
        });
    }

    // Load full node data for each matching ID, filtering by branch and
    // excluding removed decisions.
    let mut conventions = Vec::new();

    let conn_guard = crate::lock_conn(conn)?;

    let sql = format!(
        "SELECT id, nature, weight, confidence, adoption_count, total_count, description, ext_data
         FROM nodes
         WHERE id = ?1
           AND branch_id = ?2
           AND {SQL_NOT_REMOVED}"
    );

    for node_id in &node_ids {
        let row = conn_guard.query_row(&sql, params![node_id.0, branch_id], |row| {
            Ok(RawConventionRow {
                id: row.get(0)?,
                nature: row.get(1)?,
                weight: row.get(2)?,
                confidence: row.get(3)?,
                adoption_count: row.get(4)?,
                total_count: row.get(5)?,
                description: row.get(6)?,
                ext_data: row.get(7)?,
            })
        });

        match row {
            Ok(raw) => {
                if let Some(result) = enrich_convention(raw) {
                    conventions.push(result);
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Node doesn't belong to this branch or was removed — skip.
                continue;
            }
            Err(e) => {
                tracing::warn!(node_id = node_id.0, "Skipping node due to query error: {e}");
            }
        }
    }

    Ok(QueryConventionData { conventions })
}

// ── Internal types ───────────────────────────────────────────

/// Raw row data from the nodes table.
struct RawConventionRow {
    #[allow(dead_code)]
    id: i64,
    nature: String,
    weight: String,
    confidence: f64,
    adoption_count: u32,
    total_count: u32,
    description: String,
    ext_data: Option<String>,
}

/// Enrich a raw convention row into a full `ConventionResult`.
fn enrich_convention(raw: RawConventionRow) -> Option<ConventionResult> {
    let ext: serde_json::Value = raw
        .ext_data
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);

    let source = ext
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();

    let user_confirmed = ext
        .get("user_confirmed")
        .and_then(|v| v.as_bool())
        .unwrap_or(source == SOURCE_USER);

    let trend = ext
        .get("trend")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();

    let adoption_rate = if raw.total_count > 0 {
        raw.adoption_count as f64 / raw.total_count as f64
    } else {
        ext.get("adoption_rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    };

    let examples = extract_evidence(&ext);

    Some(ConventionResult {
        nature: raw.nature,
        weight: raw.weight,
        confidence_pct: (raw.confidence.clamp(0.0, 1.0) * 100.0).round() as u32,
        adoption: AdoptionInfo {
            count: raw.adoption_count,
            total: raw.total_count,
            rate_pct: (adoption_rate.clamp(0.0, 1.0) * 100.0).round() as u32,
        },
        trend,
        description: raw.description,
        source,
        user_confirmed,
        examples,
    })
}

/// Extract evidence examples from ext_data.
fn extract_evidence(ext: &serde_json::Value) -> Vec<EvidenceExample> {
    let evidence = match ext.get("evidence").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    evidence
        .iter()
        .filter_map(|e| {
            let file = e.get("file").and_then(|v| v.as_str())?.to_owned();
            let line = e.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let end_line = e
                .get("end_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(line as u64) as u32;
            // "snippet" is stored as {"content": "...", "truncated": false} by
            // convention_to_node.  Extract the "content" string from the object.
            // Fall back to treating it as a raw string for backwards compatibility.
            let snippet_raw = e
                .get("snippet")
                .and_then(|s| {
                    // New format: {"content": "...", "truncated": false}
                    s.get("content")
                        .and_then(|c| c.as_str())
                        // Legacy format: plain string
                        .or_else(|| s.as_str())
                })
                .unwrap_or("");
            let snippet = truncate_snippet(snippet_raw);

            Some(EvidenceExample {
                file,
                line,
                end_line,
                snippet,
            })
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::test_conn;

    /// Helper: insert a convention node with ext_data and return its assigned ID.
    #[allow(clippy::too_many_arguments)]
    fn insert_convention(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        description: &str,
        source: &str,
        detector_name: &str,
        confidence: f64,
        adoption_count: u32,
        total_count: u32,
    ) -> i64 {
        let c = conn.lock().unwrap();

        let mut ext = serde_json::Map::new();
        ext.insert("source".into(), source.into());
        ext.insert("detector_name".into(), detector_name.into());
        ext.insert("trend".into(), "stable".into());
        ext.insert("adoption_rate".into(), serde_json::json!(confidence));
        ext.insert(
            "evidence".into(),
            serde_json::json!([
                {
                    "file": "src/main.rs",
                    "line": 10,
                    "end_line": 15,
                    "snippet": { "content": "use thiserror::Error;", "truncated": false }
                }
            ]),
        );
        if source == "user" {
            ext.insert("user_confirmed".into(), true.into());
        }

        c.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             VALUES (?1, 'convention', 'strong', ?2, ?3, ?4, ?5, ?6)",
            params![
                branch_id,
                confidence,
                adoption_count,
                total_count,
                description,
                serde_json::Value::Object(ext).to_string(),
            ],
        )
        .unwrap();

        c.last_insert_rowid()
    }

    /// Helper: insert a removed convention node.
    fn insert_removed_convention(
        conn: &Arc<Mutex<Connection>>,
        branch_id: &str,
        description: &str,
    ) -> i64 {
        let c = conn.lock().unwrap();

        let ext = serde_json::json!({
            "source": "user",
            "removed": true,
            "removed_reason": "outdated",
            "removed_at": "2026-01-01T00:00:00Z",
        });

        c.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             VALUES (?1, 'decision', 'strong', 1.0, 1, 1, ?2, ?3)",
            params![branch_id, description, ext.to_string()],
        )
        .unwrap();

        c.last_insert_rowid()
    }

    #[test]
    fn query_convention_returns_matching_conventions() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Uses thiserror for error handling (Rust)",
            "auto_detected",
            "error_handling",
            0.9,
            9,
            10,
        );
        insert_convention(
            &conn,
            "main",
            "snake_case naming convention (Rust)",
            "auto_detected",
            "naming",
            0.95,
            19,
            20,
        );

        // Rebuild FTS5 index.
        fts::rebuild_fts_index(&conn).unwrap();

        let result = query_convention(&conn, "main", "error").unwrap();
        assert!(!result.conventions.is_empty());
        assert!(
            result
                .conventions
                .iter()
                .any(|c| c.description.contains("error"))
        );
    }

    #[test]
    fn query_convention_returns_both_auto_and_user() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Uses thiserror for error handling (Rust)",
            "auto_detected",
            "error_handling",
            0.9,
            9,
            10,
        );
        insert_convention(
            &conn,
            "main",
            "Always wrap errors with context",
            "user",
            "",
            1.0,
            1,
            1,
        );

        fts::rebuild_fts_index(&conn).unwrap();

        // Search for "error" — both should match.
        let result = query_convention(&conn, "main", "error").unwrap();
        assert!(!result.conventions.is_empty());

        // Check that user convention has user_confirmed.
        let user_conv = result.conventions.iter().find(|c| c.source == "user");
        if let Some(uc) = user_conv {
            assert!(uc.user_confirmed);
        }
    }

    #[test]
    fn query_convention_filters_out_removed() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Uses logging framework",
            "auto_detected",
            "logging",
            0.8,
            8,
            10,
        );
        insert_removed_convention(&conn, "main", "Old logging convention removed");

        fts::rebuild_fts_index(&conn).unwrap();

        let result = query_convention(&conn, "main", "logging").unwrap();
        // Only the non-removed convention should appear.
        for c in &result.conventions {
            assert_ne!(c.description, "Old logging convention removed");
        }
    }

    #[test]
    fn query_convention_empty_result_is_not_error() {
        let conn = test_conn();
        fts::rebuild_fts_index(&conn).unwrap();

        let result = query_convention(&conn, "main", "nonexistent_xyz").unwrap();
        assert!(result.conventions.is_empty());
    }

    #[test]
    fn query_convention_filters_by_branch() {
        let conn = test_conn();

        // Insert on "main" branch.
        insert_convention(
            &conn,
            "main",
            "Uses reqwest for HTTP (Rust)",
            "auto_detected",
            "dependency_usage",
            0.9,
            9,
            10,
        );
        // Insert on "feature" branch.
        insert_convention(
            &conn,
            "feature",
            "Uses axios for HTTP (TypeScript)",
            "auto_detected",
            "dependency_usage",
            0.8,
            8,
            10,
        );

        fts::rebuild_fts_index(&conn).unwrap();

        // Query for "main" branch — should only get reqwest.
        let result = query_convention(&conn, "main", "HTTP").unwrap();
        assert_eq!(result.conventions.len(), 1);
        assert!(result.conventions[0].description.contains("reqwest"));
    }

    #[test]
    fn convention_result_has_evidence_examples() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "Uses thiserror for error types",
            "auto_detected",
            "error_handling",
            0.9,
            9,
            10,
        );

        fts::rebuild_fts_index(&conn).unwrap();

        let result = query_convention(&conn, "main", "thiserror").unwrap();
        assert!(!result.conventions.is_empty());
        let conv = &result.conventions[0];
        assert!(!conv.examples.is_empty());
        assert_eq!(conv.examples[0].file, "src/main.rs");
        assert_eq!(conv.examples[0].line, 10);
        assert_eq!(conv.examples[0].end_line, 15);
        assert!(!conv.examples[0].snippet.truncated);
        // snippet.content must be extracted from the {"content": ..., "truncated": false} object
        assert!(
            conv.examples[0].snippet.content.contains("thiserror"),
            "snippet.content must contain 'thiserror', got: {:?}",
            conv.examples[0].snippet.content
        );
    }

    #[test]
    fn convention_result_has_adoption_info() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "main",
            "snake_case naming (Rust)",
            "auto_detected",
            "naming",
            0.95,
            19,
            20,
        );

        fts::rebuild_fts_index(&conn).unwrap();

        let result = query_convention(&conn, "main", "naming").unwrap();
        assert_eq!(result.conventions.len(), 1);
        let conv = &result.conventions[0];
        assert_eq!(conv.adoption.count, 19);
        assert_eq!(conv.adoption.total, 20);
        assert_eq!(conv.adoption.rate_pct, 95);
        assert_eq!(conv.trend, "stable");
    }

    #[test]
    fn enrich_convention_handles_missing_ext_data() {
        let raw = RawConventionRow {
            id: 1,
            nature: "convention".into(),
            weight: "strong".into(),
            confidence: 0.9,
            adoption_count: 9,
            total_count: 10,
            description: "Test convention".into(),
            ext_data: None,
        };

        let result = enrich_convention(raw).unwrap();
        assert_eq!(result.source, "unknown");
        assert!(!result.user_confirmed);
        assert_eq!(result.trend, "unknown");
        assert!(result.examples.is_empty());
    }
}
