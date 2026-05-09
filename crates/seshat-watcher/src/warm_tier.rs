//! Warm tier: periodic convention recalculation.
//!
//! Runs every `warm_tier_interval_seconds` (default 30 s).  When
//! `has_pending_changes` is `true` the warm tier runs the full detection
//! pipeline via [`seshat_graph::run_detection_cycle`] and then resets the
//! flag.  If no changes are pending, the timer fires but does nothing.
//!
//! The detection pipeline itself lives in `seshat-graph` and is shared with
//! the scan command — there is no duplicated logic here.
//!
//! Architecture reference: ADR-12, ADR-13.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use seshat_core::{BranchId, DetectionConfig};
use seshat_graph::run_detection_cycle;
use seshat_storage::FileIRRepository;
use seshat_storage::SqliteFileIRRepository;
use tracing::{debug, info, warn};

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
                        // Load file dates from the DB for trend computation.
                        // We use stored dates rather than re-running `git log`
                        // on every warm-tier tick.
                        let file_dates = load_file_dates(&conn, &branch);
                        // Warm-tier runs without source in memory: pass an empty
                        // source_map so detectors fall back to IR-only detection.
                        // Snippets were already populated during the preceding
                        // full scan or hot-tier re-parse.
                        run_detection_cycle(&conn, &branch, &config, &file_dates, &HashMap::new())
                    })
                    .await;

                    match result {
                        Ok(Ok(report)) => {
                            // compare_exchange avoids overwriting a `true` that
                            // the hot tier may have set while detection was running.
                            let _ = has_pending_changes.compare_exchange(
                                true,
                                false,
                                Ordering::Release,
                                Ordering::Relaxed,
                            );
                            info!(
                                conventions = report.convention_count,
                                files = report.file_count,
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

/// Load per-file git commit dates from the database.
///
/// Used for trend computation (rising/stable/declining). Returns an empty map
/// on error — the detection pipeline degrades gracefully with no dates.
fn load_file_dates(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
) -> HashMap<String, Option<i64>> {
    SqliteFileIRRepository::new(conn.clone())
        .get_file_dates_by_branch(branch_id)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_storage::Database;

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

        tokio::time::sleep(Duration::from_millis(1200)).await;
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

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!has_pending.load(Ordering::Relaxed));

        let _ = shutdown_tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[test]
    fn run_detection_cycle_on_empty_db_succeeds() {
        // Direct invocation of the canonical entry point (no wrapper), with
        // an explicit empty source_map mirroring the warm-tier semantics
        // ("nothing changed in memory, just re-aggregate IR").
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        let branch = BranchId::from("main");
        let config = DetectionConfig::default();

        let file_dates = load_file_dates(&conn, &branch);
        let report = run_detection_cycle(&conn, &branch, &config, &file_dates, &HashMap::new())
            .expect("empty-db detection cycle");
        assert_eq!(report.file_count, 0);
        assert_eq!(report.convention_count, 0);
    }
}
