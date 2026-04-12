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
use seshat_storage::{
    Database, FileIRRepository, NodeRepository, SqliteFileIRRepository, SqliteNodeRepository,
};
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
        let mut interval =
            tokio::time::interval(Duration::from_secs(interval_secs));
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
                            has_pending_changes.store(false, Ordering::Relaxed);
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
    let all_files = file_ir_repo
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

    // 4. Replace auto-detected convention nodes.
    let node_repo = SqliteNodeRepository::new(conn.clone());
    node_repo
        .delete_auto_detected_by_branch(branch_id)
        .map_err(|e| WatcherError::EventProcessingError {
            path: String::new(),
            reason: format!("delete conventions: {e}"),
        })?;

    for convention in &aggregated {
        let node = convention_to_node(convention, branch_id);
        node_repo.insert(&node).map_err(|e| WatcherError::EventProcessingError {
            path: String::new(),
            reason: format!("insert convention: {e}"),
        })?;
    }

    // 5. Update per-file compliance counts.
    let mut counts: HashMap<String, u32> = HashMap::new();
    for finding in &findings {
        if finding.follows_convention {
            let key = finding.file_path.to_string_lossy().to_string();
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    file_ir_repo
        .update_convention_compliance_counts(branch_id, &counts)
        .map_err(|e| WatcherError::EventProcessingError {
            path: String::new(),
            reason: format!("update compliance counts: {e}"),
        })?;

    // 6. Rebuild FTS5 index.
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

        let handle = start_warm_tier(
            conn,
            branch,
            config,
            1,
            has_pending.clone(),
            shutdown_rx,
        )
        .await;

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
