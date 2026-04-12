//! # Seshat Watcher
//!
//! File watching and incremental update pipeline. Orchestrates the hot tier
//! (immediate file change → re-parse → update IR) and warm tier (periodic
//! convention recalculation).
//!
//! Architecture (ADR-12): two independent tokio tasks:
//! - **Hot tier task**: `notify-debouncer-full` events → re-parse file →
//!   update IR in DB → update edges. Target: <1 s latency.
//! - **Warm tier task**: timer (30 s) → check `has_pending_changes` →
//!   recalculate convention aggregates.
//!
//! Also handles bulk-change detection and `.git/HEAD` watch (ADR-14 partial;
//! full branch snapshots are Epic 11).

pub mod error;
pub mod events;
pub mod hot_tier;
pub mod warm_tier;

pub use error::WatcherError;
pub use hot_tier::{process_file_change, process_file_delete};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use globset::{Glob, GlobSetBuilder};
use notify_debouncer_full::{new_debouncer, notify::RecursiveMode};
use rusqlite::Connection;
use seshat_core::{BranchId, DetectionConfig, ScanConfig};
use seshat_scanner::scan_project;
use seshat_storage::Database;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::warm_tier::run_detection_cycle;

/// A handle to the running watcher.
///
/// Call [`WatcherHandle::shutdown`] (or simply drop) to stop all tasks.
pub struct WatcherHandle {
    /// Held so the notify debouncer stays alive for the full watcher lifetime.
    _debouncer: Box<dyn std::any::Any + Send>,
    hot_shutdown: oneshot::Sender<()>,
    warm_shutdown: oneshot::Sender<()>,
    hot_task: tokio::task::JoinHandle<()>,
    warm_task: tokio::task::JoinHandle<()>,
}

impl WatcherHandle {
    /// Signal all tasks to stop and await their completion.
    ///
    /// Both tasks are shut down **concurrently** with a 5-second timeout each,
    /// so total shutdown latency is at most 5 s (not 10 s).
    pub async fn shutdown(self) {
        let _ = self.hot_shutdown.send(());
        let _ = self.warm_shutdown.send(());
        let timeout = Duration::from_secs(5);
        tokio::join!(
            async {
                let _ = tokio::time::timeout(timeout, self.hot_task).await;
            },
            async {
                let _ = tokio::time::timeout(timeout, self.warm_task).await;
            }
        );
    }
}

/// Parameters for [`start_watcher`].
///
/// Mirrors the relevant fields from `seshat-cli`'s `WatcherConfig` without
/// creating a crate dependency on `seshat-cli`.
#[derive(Debug, Clone)]
pub struct WatcherParams {
    /// When `false`, `start_watcher` returns `Err(WatcherError::Disabled)`
    /// immediately without initialising any OS watcher resources.
    pub enabled: bool,
    /// Debounce delay before an event batch is delivered.
    pub debounce_ms: u64,
    /// Glob patterns (relative or absolute) to ignore.
    pub ignore_patterns: Vec<String>,
    /// Seconds between warm-tier recalculation runs.
    pub warm_tier_interval_seconds: u64,
    /// Number of file events in a 2-second window that triggers bulk-rescan.
    /// Must be ≥ 1; values of 0 are treated as 1.
    pub bulk_change_threshold: usize,
}

impl Default for WatcherParams {
    fn default() -> Self {
        Self {
            enabled: true,
            debounce_ms: 500,
            ignore_patterns: Vec::new(),
            warm_tier_interval_seconds: 30,
            bulk_change_threshold: 20,
        }
    }
}

/// Start the file watcher and return a [`WatcherHandle`].
///
/// # Errors
///
/// - `Err(WatcherError::Disabled)` when `params` indicates the watcher is
///   disabled (`enabled = false` in config). Non-fatal — caller should show
///   a banner message and serve without incremental updates.
/// - `Err(WatcherError::InitError(_))` when `notify` fails to initialise
///   (e.g., `inotify` limit exceeded on Linux). Also non-fatal per ADR-21.
pub async fn start_watcher(
    params: WatcherParams,
    project_root: PathBuf,
    db_path: PathBuf,
    db_conn: Arc<Mutex<Connection>>,
    branch_id: BranchId,
    scan_config: ScanConfig,
    detection_config: DetectionConfig,
) -> Result<WatcherHandle, WatcherError> {
    if !params.enabled {
        return Err(WatcherError::Disabled);
    }
    // --- Build ignore globset -------------------------------------------
    let ignore_set = {
        let mut builder = GlobSetBuilder::new();
        for pattern in &params.ignore_patterns {
            match Glob::new(pattern) {
                Ok(g) => {
                    builder.add(g);
                }
                Err(_) => {
                    warn!("Watcher: invalid ignore pattern '{}', skipping", pattern);
                }
            }
        }
        builder.build().unwrap_or_default()
    };

    // --- Shared pending-changes flag ------------------------------------
    let has_pending = Arc::new(AtomicBool::new(false));

    // --- Event channel (debouncer callback → hot-tier task) ------------
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    // --- Create debouncer -----------------------------------------------
    let tx_cb = event_tx.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(params.debounce_ms),
        None,
        move |res| {
            let _ = tx_cb.send(res);
        },
    )
    .map_err(|e| WatcherError::InitError(e.to_string()))?;

    debouncer
        .watch(&project_root, RecursiveMode::Recursive)
        .map_err(|e| WatcherError::InitError(e.to_string()))?;

    info!(
        root = %project_root.display(),
        debounce_ms = params.debounce_ms,
        "File watcher started"
    );

    // --- Shutdown channels ---------------------------------------------
    let (hot_tx, hot_rx) = oneshot::channel::<()>();
    let (warm_tx, warm_rx) = oneshot::channel::<()>();

    // --- Bulk-rescan callback ------------------------------------------
    // Triggered when the hot tier detects >threshold events in 2 s or
    // a .git/HEAD change.  Opens a fresh Database for scan_project.
    // Uses the `project_root` passed to `start_watcher` directly — no
    // fragile path-derivation heuristics.
    let bulk_root = project_root.clone();
    let bulk_db_path = db_path.clone();
    let bulk_conn = db_conn.clone();
    let bulk_branch = branch_id.clone();
    let bulk_scan_cfg = scan_config.clone();
    let bulk_detect_cfg = detection_config.clone();
    let bulk_pending = has_pending.clone();

    // Guard against concurrent bulk rescan threads (e.g., multiple paths in
    // one batch all exceeding the threshold before `reset()` took effect).
    let bulk_in_progress = Arc::new(AtomicBool::new(false));

    let on_bulk_rescan: Arc<dyn Fn(PathBuf) + Send + Sync + 'static> = Arc::new(move |_trigger| {
        // Skip if a rescan thread is already running.
        if bulk_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        let root = bulk_root.clone();
        let db_path = bulk_db_path.clone();
        let conn = bulk_conn.clone();
        let branch = bulk_branch.clone();
        let scan_cfg = bulk_scan_cfg.clone();
        let detect_cfg = bulk_detect_cfg.clone();
        let pending = bulk_pending.clone();
        let in_progress = bulk_in_progress.clone();

        std::thread::spawn(move || {
            info!(root = %root.display(), "Bulk rescan starting");
            match Database::open(&db_path) {
                Ok(fresh_db) => {
                    if let Err(e) = scan_project(&root, &scan_cfg, &fresh_db) {
                        warn!("Bulk rescan: scan_project failed: {e}");
                        pending.store(true, Ordering::Relaxed);
                    } else {
                        match run_detection_cycle(&conn, &branch, &detect_cfg) {
                            Ok(_) => {
                                pending.store(false, Ordering::Relaxed);
                                info!("Bulk rescan complete");
                            }
                            Err(e) => {
                                warn!("Bulk rescan: detection failed: {e}");
                                pending.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Bulk rescan: failed to open DB: {e}");
                    pending.store(true, Ordering::Relaxed);
                }
            }
            in_progress.store(false, Ordering::Release);
        });
    });

    // --- Spawn tasks ---------------------------------------------------
    let hot_task = hot_tier::start_hot_tier(
        event_rx,
        db_conn.clone(),
        branch_id.clone(),
        scan_config,
        ignore_set,
        has_pending.clone(),
        params.bulk_change_threshold,
        on_bulk_rescan,
        hot_rx,
    )
    .await;

    let warm_task = warm_tier::start_warm_tier(
        db_conn,
        branch_id,
        detection_config,
        params.warm_tier_interval_seconds,
        has_pending,
        warm_rx,
    )
    .await;

    Ok(WatcherHandle {
        _debouncer: Box::new(debouncer),
        hot_shutdown: hot_tx,
        warm_shutdown: warm_tx,
        hot_task,
        warm_task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_storage::{Database, FileIRRepository, SqliteFileIRRepository};
    use std::fs;
    use tempfile::tempdir;

    fn make_params(debounce_ms: u64) -> WatcherParams {
        WatcherParams {
            debounce_ms,
            warm_tier_interval_seconds: 60, // avoid warm-tier noise in hot-tier tests
            bulk_change_threshold: 20,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn start_watcher_and_shutdown_cleanly() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open DB");
        let conn = db.connection().clone();

        let handle = start_watcher(
            make_params(50),
            dir.path().to_path_buf(),
            db_path,
            conn,
            BranchId::from("main"),
            ScanConfig::default(),
            DetectionConfig::default(),
        )
        .await
        .expect("watcher should start");

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn hot_tier_detects_file_creation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open DB");
        let conn = db.connection().clone();

        let handle = start_watcher(
            make_params(50),
            dir.path().to_path_buf(),
            db_path,
            conn.clone(),
            BranchId::from("main"),
            ScanConfig::default(),
            DetectionConfig::default(),
        )
        .await
        .expect("watcher should start");

        let src_dir = dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "pub fn answer() -> u32 { 42 }").unwrap();

        // Wait for debounce + processing.
        tokio::time::sleep(Duration::from_millis(600)).await;

        let repo = SqliteFileIRRepository::new(conn.clone());
        let files = repo.get_by_branch(&BranchId::from("main")).unwrap();
        assert!(
            !files.is_empty(),
            "expected file in files_ir after creation event"
        );

        handle.shutdown().await;
    }

    // Ignored on CI: notify Remove events for pre-existing files are not
    // reliably delivered by kqueue/FSEvents on macOS. The unit test
    // `hot_tier::tests::process_file_delete_removes_ir` covers the logic.
    #[tokio::test]
    #[ignore]
    async fn hot_tier_detects_file_deletion() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("main.rs");
        fs::write(&src, "fn main() {}").unwrap();

        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open DB");
        let conn = db.connection().clone();

        // Pre-seed the DB with the file's IR.
        process_file_change(&src, &conn, &BranchId::from("main"), &ScanConfig::default()).unwrap();

        let handle = start_watcher(
            make_params(50),
            dir.path().to_path_buf(),
            db_path,
            conn.clone(),
            BranchId::from("main"),
            ScanConfig::default(),
            DetectionConfig::default(),
        )
        .await
        .expect("watcher should start");

        fs::remove_file(&src).unwrap();

        // Wait up to 3 s for the notify event to propagate (fs events
        // can be slow on CI macOS/kqueue).
        let repo = SqliteFileIRRepository::new(conn.clone());
        let mut files = repo.get_by_branch(&BranchId::from("main")).unwrap();
        for _ in 0..10 {
            if files.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            files = repo.get_by_branch(&BranchId::from("main")).unwrap();
        }
        assert!(files.is_empty(), "files_ir should be empty after deletion");

        handle.shutdown().await;
    }
}
