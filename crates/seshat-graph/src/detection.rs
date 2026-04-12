//! Convention detection pipeline — shared between the scan command and the
//! warm-tier watcher.
//!
//! This module exists to eliminate the copy-paste that previously lived in
//! both `seshat-cli/src/scan.rs` and `seshat-watcher/src/warm_tier.rs`.
//! Both callers now call into this single implementation.
//!
//! # Pipeline
//!
//! ```text
//! load files_ir from DB
//!   → run_all_detectors (rayon, CPU-bound)
//!   → aggregate_findings (confidence, trend, adoption)
//!   → persist_conventions (delete auto-detected → insert fresh nodes)
//!   → update_convention_compliance_counts
//!   → rebuild_fts_index
//! ```
//!
//! The entire persist step runs inside a single SQLite transaction so a
//! partial failure leaves the nodes table intact.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, NodeId};
use seshat_detectors::{AggregatedConvention, aggregate_findings, run_all_detectors};
use seshat_storage::{FileIRRepository, SqliteFileIRRepository};
use tracing::info;

use crate::error::GraphError;
use crate::{SOURCE_AUTO_DETECTED, rebuild_fts_index};

// ── Public API ────────────────────────────────────────────────────────────────

/// Result of a successful detection cycle.
#[derive(Debug, Clone, Copy)]
pub struct DetectionReport {
    /// Number of source files that were analysed.
    pub file_count: usize,
    /// Number of distinct convention nodes persisted.
    pub convention_count: usize,
}

/// Run the full convention-detection pipeline on the given connection.
///
/// # Arguments
///
/// * `conn` — shared database connection (holds all IR and nodes).
/// * `branch_id` — branch to operate on (currently always `"main"`).
/// * `detection_config` — thresholds, weights, and detector settings.
/// * `file_dates` — optional map of `file_path → last_commit_unix_ts`
///   used for trend computation.  Pass an empty map when git dates are
///   unavailable (e.g. warm-tier incremental runs).
///
/// # Errors
///
/// Returns `GraphError` on any database or serialisation failure.
/// The persist step is transactional: a failure rolls back the entire
/// node replacement, leaving the previous state intact.
pub fn run_detection_cycle(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    detection_config: &DetectionConfig,
    file_dates: &HashMap<String, Option<i64>>,
) -> Result<DetectionReport, GraphError> {
    // 1. Load all parsed files from the DB (current IR schema version only).
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let all_files = file_ir_repo
        .get_by_branch(branch_id)
        .map_err(GraphError::Storage)?;

    let file_count = all_files.len();

    if all_files.is_empty() {
        return Ok(DetectionReport {
            file_count: 0,
            convention_count: 0,
        });
    }

    // 2. Run all detectors in parallel (rayon).
    let detector_results = run_all_detectors(&all_files, detection_config, None);
    let findings: Vec<seshat_core::ConventionFinding> = detector_results
        .into_iter()
        .flat_map(|r| r.findings)
        .collect();

    // 3. Aggregate findings into convention nodes.
    let now = chrono::Utc::now().timestamp();
    let aggregated = aggregate_findings(&findings, detection_config, file_dates, now);
    let convention_count = aggregated.len();

    // 4. Persist: delete old auto-detected nodes + insert fresh ones, all in
    //    a single transaction so a partial failure leaves the table intact.
    persist_conventions(conn, branch_id, &aggregated)?;

    // 5. Update per-file compliance counts (outside the main transaction —
    //    idempotent and non-critical; warm tier will retry on next cycle).
    update_compliance_counts(conn, branch_id, &findings)?;

    // 6. Rebuild FTS5 index.
    rebuild_fts_index(conn).map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "rebuild FTS: {e}"
        )))
    })?;

    info!(
        files = file_count,
        conventions = convention_count,
        "Detection cycle complete"
    );

    Ok(DetectionReport {
        file_count,
        convention_count,
    })
}

/// Convert an [`AggregatedConvention`] to a [`KnowledgeNode`] for storage.
///
/// The `ext_data` JSON contains:
/// - `source`: `"auto_detected"` (distinguishes from user decisions)
/// - `detector_name`: which detector produced this
/// - `trend`: rising / stable / declining / unknown
/// - `adoption_rate`: confidence as a float
/// - `evidence`: `[{file, line, end_line, snippet}]`
pub fn convention_to_node(
    convention: &AggregatedConvention,
    branch_id: &BranchId,
) -> KnowledgeNode {
    let evidence_json: Vec<serde_json::Value> = convention
        .evidence
        .iter()
        .map(|e| {
            // NOTE: `CodeEvidence` does not carry a `file_path` field — it is
            // a raw snippet excerpt.  The `"file"` field here mirrors the
            // existing pattern in the former scan.rs / warm_tier.rs copies.
            // Fixing `CodeEvidence` to carry `file_path` is tracked separately.
            serde_json::json!({
                "file": e.snippet.lines().next().unwrap_or(""),
                "line": e.line,
                "end_line": e.end_line,
                "snippet": e.snippet,
            })
        })
        .collect();

    let mut ext_data = convention
        .ext_data(None)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    ext_data.insert(
        "source".to_owned(),
        serde_json::Value::String(SOURCE_AUTO_DETECTED.to_owned()),
    );
    ext_data.insert(
        "detector_name".to_owned(),
        serde_json::Value::String(convention.detector_name.clone()),
    );
    ext_data.insert(
        "evidence".to_owned(),
        serde_json::Value::Array(evidence_json),
    );

    KnowledgeNode {
        id: NodeId(0), // auto-assigned by DB
        branch_id: branch_id.clone(),
        nature: convention.nature,
        weight: convention.weight,
        confidence: convention.confidence,
        adoption_count: convention.adoption_count,
        total_count: convention.total_count,
        description: convention.description.clone(),
        ext_data: Some(serde_json::Value::Object(ext_data)),
    }
}

/// Persist aggregated conventions and rebuild search indices without re-running
/// detection.
///
/// Use this when the caller has already run detection (e.g., the scan command
/// runs detection with a progress spinner) and only needs to persist the
/// results.  For a full end-to-end cycle use [`run_detection_cycle`] instead.
pub fn persist_and_index(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    aggregated: &[AggregatedConvention],
    findings: &[seshat_core::ConventionFinding],
) -> Result<(), GraphError> {
    persist_conventions(conn, branch_id, aggregated)?;
    update_compliance_counts(conn, branch_id, findings)?;
    rebuild_fts_index(conn).map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "rebuild FTS: {e}"
        )))
    })?;
    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Atomically replace all auto-detected convention nodes for a branch.
///
/// Runs DELETE + INSERT inside a single `BEGIN … COMMIT` transaction.
/// On any error the transaction is rolled back and the previous node set
/// remains intact.
fn persist_conventions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    aggregated: &[AggregatedConvention],
) -> Result<(), GraphError> {
    let guard = crate::lock_conn(conn)?;

    guard.execute_batch("BEGIN").map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "BEGIN: {e}"
        )))
    })?;

    // Delete all auto-detected nodes for this branch.
    let del = guard.execute(
        "DELETE FROM nodes
         WHERE branch_id = ?1
           AND json_extract(ext_data, '$.source') = 'auto_detected'",
        rusqlite::params![branch_id.0],
    );
    if let Err(e) = del {
        let _ = guard.execute_batch("ROLLBACK");
        return Err(GraphError::Storage(
            seshat_storage::StorageError::QueryError(format!("delete conventions: {e}")),
        ));
    }

    // Insert fresh nodes.
    for convention in aggregated {
        let node = convention_to_node(convention, branch_id);
        let ext = node.ext_data.as_ref().map(|v| v.to_string());
        let ins = guard.execute(
            "INSERT INTO nodes
             (branch_id, nature, weight, confidence,
              adoption_count, total_count, description, ext_data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                node.branch_id.0,
                node.nature.as_str(),
                node.weight.as_str(),
                node.confidence,
                node.adoption_count,
                node.total_count,
                node.description,
                ext,
            ],
        );
        if let Err(e) = ins {
            let _ = guard.execute_batch("ROLLBACK");
            return Err(GraphError::Storage(
                seshat_storage::StorageError::QueryError(format!("insert convention: {e}")),
            ));
        }
    }

    guard.execute_batch("COMMIT").map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "COMMIT: {e}"
        )))
    })?;

    info!(count = aggregated.len(), "Persisted convention nodes");
    Ok(())
}

/// Compute and write per-file convention-compliance counts.
///
/// Counts `ConventionFinding`s where `follows_convention == true` per file
/// path and writes those counts into `files_ir.convention_compliance_count`.
fn update_compliance_counts(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    findings: &[seshat_core::ConventionFinding],
) -> Result<(), GraphError> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for finding in findings {
        if finding.follows_convention {
            let key = finding.file_path.to_string_lossy().to_string();
            *counts.entry(key).or_insert(0) += 1;
        }
    }

    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    file_ir_repo
        .update_convention_compliance_counts(branch_id, &counts)
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "update compliance counts: {e}"
            )))
        })?;

    info!(
        files_with_conventions = counts.len(),
        "Updated per-file convention compliance counts"
    );
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::Language;
    use seshat_core::test_helpers::make_project_file;
    use seshat_storage::Database;

    fn open_db() -> (Database, Arc<Mutex<Connection>>) {
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        (db, conn)
    }

    #[test]
    fn run_detection_cycle_empty_db_returns_zero() {
        let (_db, conn) = open_db();
        let branch = BranchId::from("main");
        let config = DetectionConfig::default();
        let result = run_detection_cycle(&conn, &branch, &config, &HashMap::new());
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.file_count, 0);
        assert_eq!(r.convention_count, 0);
    }

    #[test]
    fn run_detection_cycle_with_files_runs_without_error() {
        let (db, conn) = open_db();
        let branch = BranchId::from("main");

        // Seed a file via the proper upsert path.
        let file = make_project_file(Language::Rust);
        SqliteFileIRRepository::new(conn.clone())
            .upsert(&branch, &file, None)
            .expect("upsert");

        let config = DetectionConfig::default();
        let result = run_detection_cycle(&conn, &branch, &config, &HashMap::new());
        assert!(
            result.is_ok(),
            "detection cycle should not fail: {result:?}"
        );
        let r = result.unwrap();
        assert_eq!(r.file_count, 1);
        drop(db); // keep db alive until here
    }

    #[test]
    fn convention_to_node_sets_source_auto_detected() {
        use seshat_core::{KnowledgeNature, KnowledgeWeight, Trend};
        use seshat_detectors::AggregatedConvention;

        let convention = AggregatedConvention {
            description: "test convention".to_string(),
            detector_name: "test_detector".to_string(),
            nature: KnowledgeNature::Convention,
            weight: KnowledgeWeight::Strong,
            confidence: 0.85,
            adoption_count: 8,
            total_count: 10,
            trend: Trend::Stable,
            evidence: vec![],
        };

        let branch = BranchId::from("main");
        let node = convention_to_node(&convention, &branch);

        let ext = node.ext_data.as_ref().unwrap();
        assert_eq!(ext["source"].as_str().unwrap(), SOURCE_AUTO_DETECTED);
        assert_eq!(ext["detector_name"].as_str().unwrap(), "test_detector");
        assert_eq!(node.confidence, 0.85);
        assert_eq!(node.description, "test convention");
    }
}
