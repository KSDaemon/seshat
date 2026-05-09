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
//!

pub mod error;
pub mod events;
pub mod hot_tier;
pub mod warm_tier;

pub use error::WatcherError;
pub use hot_tier::{process_file_change, process_file_delete};

use std::path::{Path, PathBuf};
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
#[allow(clippy::too_many_arguments)]
pub async fn start_watcher(
    params: WatcherParams,
    project_root: PathBuf,
    db_path: PathBuf,
    db_conn: Arc<Mutex<Connection>>,
    branch_id: BranchId,
    scan_config: ScanConfig,
    detection_config: DetectionConfig,
    on_branch_switch: Arc<dyn Fn() + Send + Sync + 'static>,
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

    // --- Create debouncer (offloaded to blocking thread) ----------------
    // `new_debouncer` spawns an OS file-watching thread (FSEvents on macOS,
    // inotify on Linux) and `watch()` registers the path with the kernel.
    // On macOS this can take several seconds for large directories, which
    // would block the tokio executor thread and freeze the MCP server before
    // it even starts reading stdin.  Running both calls in `spawn_blocking`
    // keeps the async runtime responsive during watcher initialisation.
    let tx_cb = event_tx.clone();
    let debounce_ms = params.debounce_ms;
    let watch_root = project_root.clone();
    let debouncer = tokio::task::spawn_blocking(move || {
        let mut d = new_debouncer(Duration::from_millis(debounce_ms), None, move |res| {
            let _ = tx_cb.send(res);
        })
        .map_err(|e| WatcherError::InitError(e.to_string()))?;
        d.watch(&watch_root, RecursiveMode::Recursive)
            .map_err(|e| WatcherError::InitError(e.to_string()))?;
        Ok::<_, WatcherError>(d)
    })
    .await
    .map_err(|e| WatcherError::InitError(format!("spawn_blocking panicked: {e}")))??;

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

    let on_bulk_rescan: Arc<dyn Fn(PathBuf) + Send + Sync + 'static> = Arc::new(move |trigger| {
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
            execute_bulk_rescan(
                &root,
                &db_path,
                &conn,
                &branch,
                &scan_cfg,
                &detect_cfg,
                &pending,
                &in_progress,
                trigger,
            );
        });
    });

    // --- Spawn tasks ---------------------------------------------------
    let hot_task = hot_tier::start_hot_tier(
        event_rx,
        db_conn.clone(),
        branch_id.clone(),
        project_root.clone(),
        scan_config,
        ignore_set,
        has_pending.clone(),
        params.bulk_change_threshold,
        on_bulk_rescan,
        on_branch_switch,
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

/// Execute a bulk rescan: open DB → scan_project → detection cycle.
///
/// Extracted from the `on_bulk_rescan` closure so it can be unit-tested.
#[allow(clippy::too_many_arguments)]
fn execute_bulk_rescan(
    root: &Path,
    db_path: &Path,
    conn: &Arc<Mutex<rusqlite::Connection>>,
    branch: &BranchId,
    scan_cfg: &ScanConfig,
    detect_cfg: &DetectionConfig,
    pending: &AtomicBool,
    in_progress: &AtomicBool,
    trigger: PathBuf,
) {
    let _ = trigger; // used by caller for logging context
    let _ = (conn, detect_cfg); // detection now lives inside scan_project
    info!(root = %root.display(), "Bulk rescan starting");
    match Database::open(db_path) {
        Ok(fresh_db) => {
            // scan_project records the freshness sentinel internally
            // (P19) using a HEAD captured before discovery (P18) AND runs
            // the detection cycle with the populated source_map. The
            // pre-fix follow-up call to `run_detection_cycle_sync` here
            // wiped every snippet (it reran detection with an empty
            // source_map), so it has been removed.
            if let Err(e) = scan_project(root, scan_cfg, &fresh_db, branch.clone()) {
                warn!("Bulk rescan: scan_project failed: {e}");
                pending.store(true, Ordering::Relaxed);
            } else {
                pending.store(false, Ordering::Relaxed);
                info!("Bulk rescan complete");
            }
        }
        Err(e) => {
            warn!("Bulk rescan: failed to open DB: {e}");
            pending.store(true, Ordering::Relaxed);
        }
    }
    in_progress.store(false, Ordering::Release);
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
            Arc::new(|| {}),
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
            Arc::new(|| {}),
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
        process_file_change(
            &src,
            dir.path(),
            &conn,
            &BranchId::from("main"),
            &ScanConfig::default(),
        )
        .unwrap();

        let handle = start_watcher(
            make_params(50),
            dir.path().to_path_buf(),
            db_path,
            conn.clone(),
            BranchId::from("main"),
            ScanConfig::default(),
            DetectionConfig::default(),
            Arc::new(|| {}),
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

    #[test]
    fn watcher_params_default() {
        let params = WatcherParams::default();
        assert!(params.enabled);
        assert_eq!(params.debounce_ms, 500);
        assert_eq!(params.warm_tier_interval_seconds, 30);
        assert_eq!(params.bulk_change_threshold, 20);
        assert!(params.ignore_patterns.is_empty());
    }

    #[tokio::test]
    async fn start_watcher_disabled_returns_error() {
        let params = WatcherParams {
            enabled: false,
            ..Default::default()
        };
        let result = start_watcher(
            params,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/test.db"),
            Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            BranchId::from("main"),
            ScanConfig::default(),
            DetectionConfig::default(),
            Arc::new(|| {}),
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .err()
                .is_some_and(|e| matches!(e, WatcherError::Disabled))
        );
    }

    #[test]
    fn watcher_params_clone() {
        let params = WatcherParams {
            enabled: false,
            debounce_ms: 100,
            ignore_patterns: vec!["target/**".to_owned()],
            warm_tier_interval_seconds: 10,
            bulk_change_threshold: 50,
        };
        let cloned = params.clone();
        assert!(!cloned.enabled);
        assert_eq!(cloned.debounce_ms, 100);
        assert_eq!(cloned.ignore_patterns.len(), 1);
        assert_eq!(cloned.warm_tier_interval_seconds, 10);
        assert_eq!(cloned.bulk_change_threshold, 50);
    }

    #[test]
    fn watcher_error_display_disabled() {
        let err = WatcherError::Disabled;
        assert!(err.to_string().contains("disabled"));
    }

    #[test]
    fn watcher_error_display_init_error() {
        let err = WatcherError::InitError("permission denied".to_owned());
        assert!(err.to_string().contains("permission denied"));
    }

    #[test]
    fn watcher_error_display_event_processing_error() {
        let err = WatcherError::EventProcessingError {
            path: "src/main.rs".to_owned(),
            reason: "parse failed".to_owned(),
        };
        let s = err.to_string();
        assert!(s.contains("src/main.rs"));
        assert!(s.contains("parse failed"));
    }

    #[test]
    fn watcher_error_display_branch_detection_error() {
        let err = WatcherError::BranchDetectionError("detached head".to_owned());
        assert!(err.to_string().contains("detached head"));
    }

    #[test]
    fn watcher_error_from_io() {
        let io_err = std::io::Error::other("oh no");
        let watcher_err: WatcherError = io_err.into();
        assert!(watcher_err.to_string().contains("oh no"));
    }

    // ── execute_bulk_rescan ──────────────────────────────────────────

    #[test]
    fn execute_bulk_rescan_valid_project_clears_pending() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "pub fn hello() -> &'static str { \"hello\" }\n",
        )
        .unwrap();

        let db_path = root.join("scan.db");
        let db = Database::open(&db_path).unwrap();
        let conn = db.connection().clone();
        let branch = BranchId::from("main");
        let pending = Arc::new(AtomicBool::new(true));
        let in_progress = Arc::new(AtomicBool::new(true));

        execute_bulk_rescan(
            root,
            &db_path,
            &conn,
            &branch,
            &ScanConfig::default(),
            &DetectionConfig::default(),
            &pending,
            &in_progress,
            PathBuf::from("/trigger"),
        );

        assert!(!pending.load(Ordering::Relaxed));
    }

    #[test]
    fn execute_bulk_rescan_does_not_advance_sentinel_when_git_unavailable() {
        // P17/P18: with no .git in `root`, get_head_commit returns None,
        // so the sentinel must remain NULL. The previous startup-recorded
        // value (if any) must NOT be replaced with a placeholder. This
        // also indirectly verifies the "snapshot HEAD before scan" path
        // — when there's no HEAD to snapshot, we don't write garbage.
        use seshat_storage::{BranchRepository, SqliteBranchRepository};

        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn hi() {}\n").unwrap();
        // Note: no `git init` — root is a non-git directory.

        let db_path = root.join("scan.db");
        let db = Database::open(&db_path).unwrap();
        let conn = db.connection().clone();
        let branch = BranchId::from("main");
        let pending = Arc::new(AtomicBool::new(true));
        let in_progress = Arc::new(AtomicBool::new(true));

        execute_bulk_rescan(
            root,
            &db_path,
            &conn,
            &branch,
            &ScanConfig::default(),
            &DetectionConfig::default(),
            &pending,
            &in_progress,
            PathBuf::from("/trigger"),
        );

        // Sentinel remains None — git-unavailable path took the silent
        // skip rather than writing a placeholder.
        let branch_repo = SqliteBranchRepository::new(conn.clone());
        let sentinel = branch_repo.get_last_scanned_commit(&branch).unwrap();
        assert!(
            sentinel.is_none(),
            "git-unavailable rescan must not advance the sentinel; got: {sentinel:?}"
        );
    }
}
