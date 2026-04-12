//! Warm tier: periodic convention recalculation.
//!
//! Runs every `warm_tier_interval_seconds` (default 30 s).  When
//! `has_pending_changes` is `true` the warm tier:
//!
//! 1. Loads all `ProjectFile` IR from the database.
//! 2. Runs all convention detectors (CPU-bound, via rayon inside detectors).
//! 3. Aggregates findings into `AggregatedConvention` entries.
//! 4. Replaces auto-detected convention nodes in the DB.
//! 5. Updates per-file compliance counts.
//! 6. Rebuilds the FTS5 index.
//! 7. Resets `has_pending_changes` to `false`.
//!
//! If no changes are pending, the timer fires but does nothing (zero cost).
//!
//! Architecture reference: ADR-12, ADR-13.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, NodeId};
use seshat_detectors::{AggregatedConvention, aggregate_findings, run_all_detectors};
use seshat_graph::SOURCE_AUTO_DETECTED;
use seshat_storage::{FileIRRepository, SqliteFileIRRepository};
use tracing::{debug, info, warn};

use crate::WatcherError;

/// Start the warm-tier tokio task.
///
/// The task loops on a fixed interval.  On each tick it checks
/// `has_pending_changes` and, if set, runs the full detection pipeline.
pub async fn start_warm_tier(
    db_conn: Arc<Mutex<Connection>>,
    branch_id: BranchId,
    detection_config: DetectionConfig,
    interval_secs: u64,
    has_pending_changes: Arc<AtomicBool>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the immediate first tick so we don't run on startup.
        interval.tick().await;

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    debug!("Warm tier: shutdown signal received");
                    break;
                }
                _ = interval.tick() => {
                    if !has_pending_changes.load(Ordering::Relaxed) {
                        debug!("Warm tier: no pending changes, skipping");
                        continue;
                    }

                    let conn = db_conn.clone();
                    let branch = branch_id.clone();
                    let config = detection_config.clone();

                    let result = tokio::task::spawn_blocking(move || {
                        run_detection_cycle(&conn, &branch, &config)
                    })
                    .await;

                    match result {
                        Ok(Ok(counts)) => {
                            // Use compare_exchange to avoid overwriting a `true`
                            // that the hot tier may have stored while detection was
                            // running (race between cycle completion and new events).
                            let _ = has_pending_changes.compare_exchange(
                                true,
                                false,
                                Ordering::Release,
                                Ordering::Relaxed,
                            );
                            info!(
                                conventions = counts.convention_count,
                                files = counts.file_count,
                                "Warm tier: recalculation complete"
                            );
                        }
                        Ok(Err(e)) => {
                            warn!("Warm tier: detection cycle failed: {e}");
                            // Don't reset has_pending_changes — retry next cycle.
                        }
                        Err(join_err) => {
                            warn!("Warm tier: spawn_blocking panicked: {join_err}");
                        }
                    }
                }
            }
        }
        debug!("Warm tier: task exiting");
    })
}

// ---------------------------------------------------------------------------
// Detection cycle
// ---------------------------------------------------------------------------

pub struct CycleCounts {
    pub file_count: usize,
    pub convention_count: usize,
}

/// Run the full detection pipeline on the given database connection.
///
/// Called inside `spawn_blocking` — all operations are synchronous.
pub fn run_detection_cycle(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    detection_config: &DetectionConfig,
) -> Result<CycleCounts, WatcherError> {
    // 1. Load all files from the DB.
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let all_files =
        file_ir_repo
            .get_by_branch(branch_id)
            .map_err(|e| WatcherError::EventProcessingError {
                path: String::new(),
                reason: format!("load files_ir: {e}"),
            })?;
    let file_count = all_files.len();

    if all_files.is_empty() {
        return Ok(CycleCounts {
            file_count: 0,
            convention_count: 0,
        });
    }

    // 2. Run detectors (CPU-bound, uses rayon internally).
    let detector_results = run_all_detectors(&all_files, detection_config, None);
    let findings: Vec<seshat_core::ConventionFinding> = detector_results
        .into_iter()
        .flat_map(|r| r.findings)
        .collect();

    // 3. Build file-dates map (None for all files — dates are for trend
    //    computation and we don't re-collect git history in warm tier;
    //    stored dates in files_ir are used instead via get_file_dates_by_branch).
    let file_dates_map: HashMap<String, Option<i64>> = {
        let dates = file_ir_repo
            .get_file_dates_by_branch(branch_id)
            .unwrap_or_default();
        dates.into_iter().collect()
    };

    let now = chrono::Utc::now().timestamp();
    let aggregated = aggregate_findings(&findings, detection_config, &file_dates_map, now);
    let convention_count = aggregated.len();

    // 4–6. Replace convention nodes, update compliance counts, rebuild FTS.
    //
    // Wrapped in a single SQLite transaction so a partial failure (e.g., a
    // failed insert mid-way through) leaves the table fully intact rather than
    // partially deleted. The transaction is committed only after all writes
    // succeed; it rolls back automatically on any error.
    {
        let guard = conn
            .lock()
            .map_err(|e| WatcherError::EventProcessingError {
                path: String::new(),
                reason: format!("lock DB for transaction: {e}"),
            })?;

        guard
            .execute_batch("BEGIN")
            .map_err(|e| WatcherError::EventProcessingError {
                path: String::new(),
                reason: format!("BEGIN transaction: {e}"),
            })?;

        // We run the individual repository operations through the shared
        // Arc<Mutex<Connection>> — the mutex is already held by `guard`, so we
        // must not call repo methods that re-acquire it. Instead, run the SQL
        // statements directly on `guard`.
        let delete_result = guard.execute(
            "DELETE FROM nodes
             WHERE branch_id = ?1
               AND json_extract(ext_data, '$.source') = 'auto_detected'",
            rusqlite::params![branch_id.0],
        );

        if let Err(e) = delete_result {
            let _ = guard.execute_batch("ROLLBACK");
            return Err(WatcherError::EventProcessingError {
                path: String::new(),
                reason: format!("delete conventions: {e}"),
            });
        }

        for convention in &aggregated {
            let node = convention_to_node(convention, branch_id);
            let ext = node.ext_data.as_ref().map(|v| v.to_string());
            let insert_result = guard.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence,
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
            if let Err(e) = insert_result {
                let _ = guard.execute_batch("ROLLBACK");
                return Err(WatcherError::EventProcessingError {
                    path: String::new(),
                    reason: format!("insert convention: {e}"),
                });
            }
        }

        // Per-file compliance counts within the same transaction.
        let mut counts: HashMap<String, u32> = HashMap::new();
        for finding in &findings {
            if finding.follows_convention {
                let key = finding.file_path.to_string_lossy().to_string();
                *counts.entry(key).or_insert(0) += 1;
            }
        }

        if let Err(e) = guard.execute(
            "UPDATE files_ir SET convention_compliance_count = 0 WHERE branch_id = ?1",
            rusqlite::params![branch_id.0],
        ) {
            let _ = guard.execute_batch("ROLLBACK");
            return Err(WatcherError::EventProcessingError {
                path: String::new(),
                reason: format!("reset compliance counts: {e}"),
            });
        }

        for (file_path, count) in &counts {
            if let Err(e) = guard.execute(
                "UPDATE files_ir SET convention_compliance_count = ?1
                 WHERE branch_id = ?2 AND file_path = ?3",
                rusqlite::params![count, branch_id.0, file_path],
            ) {
                let _ = guard.execute_batch("ROLLBACK");
                return Err(WatcherError::EventProcessingError {
                    path: String::new(),
                    reason: format!("update compliance count for {file_path}: {e}"),
                });
            }
        }

        guard
            .execute_batch("COMMIT")
            .map_err(|e| WatcherError::EventProcessingError {
                path: String::new(),
                reason: format!("COMMIT transaction: {e}"),
            })?;
    }

    // 7. Rebuild FTS5 index (outside the transaction — FTS updates are
    //    idempotent and a failure here is non-critical; warm tier will retry).
    seshat_graph::rebuild_fts_index(conn).map_err(|e| WatcherError::EventProcessingError {
        path: String::new(),
        reason: format!("rebuild FTS: {e}"),
    })?;

    Ok(CycleCounts {
        file_count,
        convention_count,
    })
}

// ---------------------------------------------------------------------------
// Convention node construction (mirrors seshat-cli/src/scan.rs)
// ---------------------------------------------------------------------------

fn convention_to_node(convention: &AggregatedConvention, branch_id: &BranchId) -> KnowledgeNode {
    let evidence_json: Vec<serde_json::Value> = convention
        .evidence
        .iter()
        .map(|e| {
            // NOTE: CodeEvidence does not carry a file_path field (by design —
            // it is a snippet excerpt). The "file" field mirrors the pattern in
            // seshat-cli/src/scan.rs. Fixing CodeEvidence to carry file_path is
            // a separate cross-crate change (tracked as a deferred improvement).
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
        id: NodeId(0),
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

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_storage::Database;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn warm_tier_skips_when_no_pending_changes() {
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        let branch = BranchId::from("main");
        let config = DetectionConfig::default();
        let has_pending = Arc::new(AtomicBool::new(false));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = start_warm_tier(
            conn,
            branch,
            config,
            1, // 1-second interval for testing
            has_pending.clone(),
            shutdown_rx,
        )
        .await;

        // Wait slightly more than the interval.
        tokio::time::sleep(Duration::from_millis(1200)).await;

        // has_pending was false — it should still be false (no reset happens, no write).
        assert!(!has_pending.load(Ordering::Relaxed));

        let _ = shutdown_tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn warm_tier_runs_cycle_when_pending() {
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        let branch = BranchId::from("main");
        let config = DetectionConfig::default();
        let has_pending = Arc::new(AtomicBool::new(true));

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let handle =
            start_warm_tier(conn, branch, config, 1, has_pending.clone(), shutdown_rx).await;

        // Wait for the cycle to run (1s interval + some margin).
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // After a successful cycle, has_pending should be reset to false.
        assert!(!has_pending.load(Ordering::Relaxed));

        let _ = shutdown_tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[test]
    fn run_detection_cycle_on_empty_db_succeeds() {
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        let branch = BranchId::from("main");
        let config = DetectionConfig::default();

        let result = run_detection_cycle(&conn, &branch, &config);
        assert!(result.is_ok());
        let counts = result.unwrap();
        assert_eq!(counts.file_count, 0);
        assert_eq!(counts.convention_count, 0);
    }
}
