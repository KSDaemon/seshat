//! Convention lookup by topic for the `query_convention` MCP tool.
//!
//! Searches conventions via FTS5 full-text search on descriptions, then
//! enriches each matching node with adoption metrics, trend, evidence,
//! and source information.
//!
//! All queries run against the SQLite database — no filesystem access needed.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;
use seshat_core::{CodeSnippet, truncate_snippet};
use seshat_storage::{Decision, ExampleEvidence};

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
    /// Node ID in the knowledge graph. `0` for rows sourced from the
    /// project-wide `decisions` table (which has no rowid).
    pub id: i64,
    /// Description hash — the canonical identifier for `decisions` rows and
    /// the column populated on auto-detected `nodes` since US-008. Use this
    /// to call `update_decision` / `remove_decision`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description_hash: String,
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
    /// Category for grouping (e.g., "naming", "error-handling").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
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
    /// Line number where the snippet text starts (may be less than `line` when
    /// leading context lines are included).  0 means use `line` as the start.
    pub snippet_start_line: u32,
    /// Code snippet (may be truncated).
    pub snippet: CodeSnippet,
}

/// Query conventions matching a topic via FTS5 full-text search and the V12
/// `decisions` table.
///
/// Two sources are merged:
///   * Auto-detected nodes — searched via the FTS5 `conventions_fts` index,
///     loaded from `nodes`, filtered by branch and `SQL_NOT_REMOVED`.
///   * User-recorded decisions — searched via SQL `LIKE` on `decisions`
///     filtered to states `'recorded'`, `'approved'`, and `'partial'`
///     (rejected decisions are decisions, but they don't represent project
///     knowledge and so don't surface here).
///
/// Results are deduplicated by `description_hash`: when both sources have a
/// row for the same hash, the decision row wins because it carries the
/// authoritative user context.
///
/// Returns `Ok` with an empty `conventions` vec when nothing matches (not an
/// error).
pub fn query_convention(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    topic: &str,
) -> Result<QueryConventionData, GraphError> {
    // 1. Auto-detected node matches via FTS5.
    let node_ids = fts::search_conventions(conn, topic)?;
    let mut node_results = Vec::new();
    if !node_ids.is_empty() {
        let conn_guard = crate::lock_conn(conn)?;

        let sql = format!(
            "SELECT id, description_hash, nature, weight, confidence, adoption_count, total_count, description, ext_data
             FROM nodes
             WHERE id = ?1
               AND branch_id = ?2
               AND {SQL_NOT_REMOVED}"
        );

        for node_id in &node_ids {
            let row = conn_guard.query_row(&sql, params![node_id.0, branch_id], |row| {
                Ok(RawConventionRow {
                    id: row.get(0)?,
                    description_hash: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    nature: row.get(2)?,
                    weight: row.get(3)?,
                    confidence: row.get(4)?,
                    adoption_count: row.get(5)?,
                    total_count: row.get(6)?,
                    description: row.get(7)?,
                    ext_data: row.get(8)?,
                })
            });

            match row {
                Ok(raw) => {
                    if let Some(result) = enrich_convention(raw) {
                        node_results.push(result);
                    }
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                Err(e) => {
                    tracing::warn!(node_id = node_id.0, "Skipping node due to query error: {e}");
                }
            }
        }
        // Lock is released at end of scope.
    }

    // 2. User-recorded decisions — searched via SQL LIKE on description.
    let decision_results = search_decisions_by_topic(conn, topic)?;

    // 3. Merge: decisions take precedence over nodes for same description_hash.
    let decision_hashes: HashSet<String> = decision_results
        .iter()
        .map(|c| c.description_hash.clone())
        .collect();

    let mut conventions = decision_results;
    for n in node_results {
        if n.description_hash.is_empty() || !decision_hashes.contains(&n.description_hash) {
            conventions.push(n);
        }
    }

    Ok(QueryConventionData { conventions })
}

/// Search the V12 `decisions` table for rows whose description contains every
/// significant keyword in `topic`. Filters to states that represent settled
/// project knowledge (`approved`, `partial`, `recorded`).
fn search_decisions_by_topic(
    conn: &Arc<Mutex<Connection>>,
    topic: &str,
) -> Result<Vec<ConventionResult>, GraphError> {
    let keywords: Vec<String> = topic
        .split_whitespace()
        .filter(|t| t.len() > 1)
        .map(|t| t.to_lowercase())
        .collect();
    if keywords.is_empty() {
        return Ok(Vec::new());
    }

    let conn_guard = crate::lock_conn(conn)?;

    let mut where_clauses = vec!["state IN ('recorded', 'approved', 'partial')".to_owned()];
    let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    for (i, kw) in keywords.iter().enumerate() {
        where_clauses.push(format!("LOWER(description) LIKE ?{}", i + 1));
        bind_values.push(Box::new(format!("%{kw}%")));
    }

    let sql = format!(
        "SELECT description_hash, description, state, nature, weight, category,
                reason, examples, decided_on_branch, decided_at
         FROM decisions
         WHERE {}
         ORDER BY decided_at DESC",
        where_clauses.join(" AND ")
    );

    let mut stmt = conn_guard
        .prepare(&sql)
        .map_err(|e| GraphError::query(format!("Failed to prepare decisions search: {e}")))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        bind_values.iter().map(|b| b.as_ref()).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let description_hash: String = row.get(0)?;
            let description: String = row.get(1)?;
            let state_s: String = row.get(2)?;
            let nature_s: String = row.get(3)?;
            let weight_s: String = row.get(4)?;
            let category: Option<String> = row.get(5)?;
            let reason: Option<String> = row.get(6)?;
            let examples_s: Option<String> = row.get(7)?;
            let _decided_on_branch: String = row.get(8)?;
            let _decided_at: i64 = row.get(9)?;
            Ok((
                description_hash,
                description,
                state_s,
                nature_s,
                weight_s,
                category,
                reason,
                examples_s,
            ))
        })
        .map_err(|e| GraphError::query(format!("Failed to query decisions: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        match row {
            Ok((hash, desc, _state, nature, weight, category, _reason, examples_json)) => {
                let examples: Vec<ExampleEvidence> = match examples_json {
                    Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
                    _ => Vec::new(),
                };
                let snippet_examples: Vec<EvidenceExample> = examples
                    .into_iter()
                    .map(|ex| EvidenceExample {
                        file: ex.file,
                        line: ex.line,
                        end_line: ex.end_line,
                        snippet_start_line: 0,
                        snippet: truncate_snippet(&ex.snippet),
                    })
                    .collect();

                out.push(ConventionResult {
                    id: 0, // decisions table has no rowid — sentinel
                    description_hash: hash,
                    nature,
                    weight,
                    confidence_pct: 100, // user-recorded knowledge is fully confident
                    adoption: AdoptionInfo {
                        count: 1,
                        total: 1,
                        rate_pct: 100,
                    },
                    trend: "stable".to_owned(),
                    description: desc,
                    source: SOURCE_USER.to_owned(),
                    user_confirmed: true,
                    category,
                    examples: snippet_examples,
                });
            }
            Err(e) => tracing::warn!("Skipping decisions search row: {e}"),
        }
    }

    Ok(out)
}

/// Build a `Decision` → `ConventionResult` translation reused by callers
/// outside this module that have already loaded the row (e.g. tests).
#[doc(hidden)]
pub fn decision_to_convention_result(d: Decision) -> ConventionResult {
    let snippet_examples: Vec<EvidenceExample> = d
        .examples
        .into_iter()
        .map(|ex| EvidenceExample {
            file: ex.file,
            line: ex.line,
            end_line: ex.end_line,
            snippet_start_line: 0,
            snippet: truncate_snippet(&ex.snippet),
        })
        .collect();

    ConventionResult {
        id: 0,
        description_hash: d.description_hash,
        nature: d.nature.as_sql_str().to_owned(),
        weight: d.weight.as_sql_str().to_owned(),
        confidence_pct: 100,
        adoption: AdoptionInfo {
            count: 1,
            total: 1,
            rate_pct: 100,
        },
        trend: "stable".to_owned(),
        description: d.description,
        source: SOURCE_USER.to_owned(),
        user_confirmed: true,
        category: d.category,
        examples: snippet_examples,
    }
}

// ── Internal types ───────────────────────────────────────────

/// Raw row data from the nodes table.
struct RawConventionRow {
    id: i64,
    description_hash: String,
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

    let category = ext
        .get("category")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let examples = extract_evidence(&ext);

    Some(ConventionResult {
        id: raw.id,
        description_hash: raw.description_hash,
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
        category,
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

            let snippet_start_line = e
                .get("snippet_start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;

            Some(EvidenceExample {
                file,
                line,
                end_line,
                snippet_start_line,
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
            description_hash: String::new(),
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
