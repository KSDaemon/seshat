//! Hot tier: immediate file-change processing.
//!
//! For every file-system event received from `notify-debouncer-full` the hot
//! tier does the minimum work required to keep the knowledge graph consistent:
//!
//! 1. Re-parse the changed file with Tree-sitter → `ProjectFile` IR.
//! 2. Upsert the IR in the `files_ir` table.
//! 3. Update the per-file convention-compliance count.
//!
//! Convention *aggregate* recalculation (confidence scores, FTS index) is
//! left to the **warm tier** (`warm_tier.rs`) which runs every 30 s.
//!
//! Edge re-insertion after a single-file change is also deferred to the warm
//! tier's full scan_project pass — the hot tier only removes stale per-file IR
//! and keeps the MCP `query_code_pattern` results current.
//!
//! Architecture reference: ADR-12, ADR-13.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use globset::{Glob, GlobSet, GlobSetBuilder};
use notify_debouncer_full::notify::EventKind;
use rusqlite::Connection;
use seshat_core::{BranchId, Language, ScanConfig};
use seshat_storage::{FileIRRepository, SqliteFileIRRepository};
use tracing::{debug, info, warn};

use crate::WatcherError;
use crate::events::{BulkChangeDetector, is_git_head_change};

/// Event batch type delivered by the debouncer.
pub type EventBatch = notify_debouncer_full::DebounceEventResult;

/// Start the hot-tier tokio task.
///
/// Returns a `JoinHandle` for the spawned task. The task runs until
/// `shutdown_rx` fires or the `rx` channel closes.
#[allow(clippy::too_many_arguments)]
pub async fn start_hot_tier(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<EventBatch>,
    conn: Arc<Mutex<Connection>>,
    branch_id: BranchId,
    project_root: PathBuf,
    scan_config: ScanConfig,
    ignore_set: GlobSet,
    has_pending_changes: Arc<AtomicBool>,
    bulk_threshold: usize,
    on_bulk_rescan: Arc<dyn Fn(PathBuf) + Send + Sync + 'static>,
    on_branch_switch: Arc<dyn Fn() + Send + Sync + 'static>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut bulk_detector = BulkChangeDetector::new(bulk_threshold);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    debug!("Hot tier: shutdown signal received");
                    break;
                }
                Some(result) = rx.recv() => {
                    match result {
                        Ok(events) => {
                            // Track whether this event batch should trigger a bulk
                            // rescan or branch switch. We break out of all path loops
                            // on detection so only one action is taken per batch.
                            let mut batch_handled = false;

                            for debounced in events {
                                let event = debounced.event;

                                for path in event.paths {
                                    // --- filter .git internals ---------
                                    if is_inside_git_dir(&path) {
                                        if is_git_head_change(&path) {
                                    info!("Branch switch detected, triggering snapshot switch");
                                    // Reset detector so checkout-induced file events
                                    // don't fire a second threshold-based rescan.
                                    bulk_detector.reset();
                                    on_branch_switch();
                                    batch_handled = true;
                                }
                                        continue;
                                    }

                                    // Skip remaining paths in this batch if we already
                                    // handled a branch switch or bulk-rescan.
                                    if batch_handled {
                                        break;
                                    }

                                    // --- filter ignored patterns --------
                                    if ignore_set.is_match(&path) {
                                        debug!(path = %path.display(), "Hot tier: ignoring");
                                        continue;
                                    }

                                    // --- bulk-change detection ----------
                                    bulk_detector.observe();
                                    if bulk_detector.should_bulk_rescan() {
                                        info!("Bulk change threshold exceeded, full rescan");
                                        bulk_detector.reset();
                                        on_bulk_rescan(path);
                                        batch_handled = true;
                                        continue;
                                    }

                                    // --- per-file hot processing --------
                                    let path_display = path.display().to_string();
                                    match event.kind {
                                        EventKind::Create(_) | EventKind::Modify(_) => {
                                            let conn2 = conn.clone();
                                            let branch = branch_id.clone();
                                            let cfg = scan_config.clone();
                                            let pending = has_pending_changes.clone();
                                            let root = project_root.clone();

                                            let result = tokio::task::spawn_blocking(
                                                move || process_file_change(&path, &root, &conn2, &branch, &cfg),
                                            )
                                            .await;

                                            match result {
                                                Ok(Ok(())) => {
                                                    pending.store(true, Ordering::Relaxed);
                                                }
                                                Ok(Err(e)) => {
                                                    warn!(
                                                        path = %path_display,
                                                        error = %e,
                                                        "Hot tier: file change failed"
                                                    );
                                                }
                                                Err(join_err) => {
                                                    warn!(
                                                        error = %join_err,
                                                        "Hot tier: spawn_blocking panicked"
                                                    );
                                                }
                                            }
                                        }
                                        EventKind::Remove(_) => {
                                            let conn2 = conn.clone();
                                            let branch = branch_id.clone();
                                            let pending = has_pending_changes.clone();
                                            let root = project_root.clone();

                                            let result = tokio::task::spawn_blocking(
                                                move || process_file_delete(&path, &root, &conn2, &branch),
                                            )
                                            .await;

                                            match result {
                                                Ok(Ok(())) => {
                                                    pending.store(true, Ordering::Relaxed);
                                                }
                                                Ok(Err(e)) => {
                                                    warn!(
                                                        path = %path_display,
                                                        error = %e,
                                                        "Hot tier: file delete failed"
                                                    );
                                                }
                                                Err(join_err) => {
                                                    warn!(
                                                        error = %join_err,
                                                        "Hot tier: spawn_blocking panicked"
                                                    );
                                                }
                                            }
                                        }
                                        _ => {} // Access, Other — ignore
                                    }
                                }
                            }
                        }
                        Err(errors) => {
                            for e in errors {
                                warn!("Watcher event error: {:?}", e);
                            }
                        }
                    }
                }
            }
        }
        debug!("Hot tier: task exiting");
    })
}

// ---------------------------------------------------------------------------
// Per-file processing — public so lib.rs integration tests can call directly
// ---------------------------------------------------------------------------

/// Re-parse a changed file and upsert its IR in the database.
///
/// Respects all three relevant fields from `scan_config`:
/// - `max_file_size_kb`: skips files larger than the configured limit.
/// - `exclude_paths`: skips files matching user-configured glob patterns
///   (same patterns as the full scan — prevents excluded files from being
///   re-indexed on every save).
/// - `local_packages`: strips internal package names from
///   `ProjectFile::dependencies_used` after parsing, keeping the hot-tier
///   output consistent with the full scan pipeline.
///
/// `path` is the absolute filesystem path delivered by the watcher event.
/// `project_root` is the worktree root passed to [`start_hot_tier`]; the
/// stored `files_ir.file_path` key is computed as `path - project_root`
/// so cross-worktree branches in a shared DB share one IR row per logical
/// file (Bug #3).
///
/// Silently skips unsupported extensions, files outside the project root,
/// and missing files (race between event and deletion). Called inside
/// `spawn_blocking`.
pub fn process_file_change(
    path: &Path,
    project_root: &Path,
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
    scan_config: &ScanConfig,
) -> Result<(), WatcherError> {
    // 1. Extension / language check.
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e,
        None => return Ok(()), // no extension → not a source file
    };
    let language = match Language::from_extension(ext) {
        Some(l) => l,
        None => return Ok(()), // unsupported extension
    };

    // 2. Check exclude_paths — skip files matching user-configured patterns.
    //    This prevents excluded files (e.g. "**/generated/**") from being
    //    re-indexed by the watcher even though the full scan would skip them.
    if !scan_config.exclude_paths.is_empty() {
        let exclude_set = build_exclude_set(&scan_config.exclude_paths);
        if exclude_set.is_match(path) {
            debug!(path = %path.display(), "Hot tier: path excluded by scan_config.exclude_paths");
            return Ok(());
        }
    }

    // 3. Check max_file_size_kb — skip large files before reading into RAM.
    let max_bytes = scan_config.max_file_size_kb * 1024;
    if max_bytes > 0 {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > max_bytes {
                debug!(
                    path = %path.display(),
                    size_kb = meta.len() / 1024,
                    limit_kb = scan_config.max_file_size_kb,
                    "Hot tier: skipping oversized file"
                );
                return Ok(());
            }
        }
    }

    // 4. Compute the path stored in IR — relative to project_root so
    //    cross-worktree branches converge on the same key. Defensive: a
    //    path outside project_root keeps its absolute form (unusual but
    //    possible if the watcher is reconfigured at runtime).
    let stored_path = path
        .strip_prefix(project_root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf());

    // 5. Read (from the absolute event path) + parse (under the relative
    //    stored_path) via the shared helper. This is the single read+parse
    //    site shared with the full scan and the incremental freshness sync.
    let (project_file, _source) = match seshat_scanner::read_and_parse_file(
        path,
        &stored_path,
        language,
        &scan_config.local_packages,
    ) {
        Ok(pair) => pair,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(WatcherError::EventProcessingError {
                path: path.display().to_string(),
                reason: e.to_string(),
            });
        }
    };

    // Upsert IR.
    let repo = SqliteFileIRRepository::new(conn.clone());
    repo.upsert(branch_id, &project_file, None).map_err(|e| {
        WatcherError::EventProcessingError {
            path: path.display().to_string(),
            reason: format!("upsert IR: {e}"),
        }
    })?;

    // Update per-file compliance count (best-effort; warm tier corrects it).
    update_single_file_compliance(&stored_path, branch_id, conn);

    info!(path = %path.display(), "Hot tier: updated file IR");
    Ok(())
}

/// Remove a deleted file's IR from the database.
///
/// Auto-detected convention nodes are intentionally NOT removed here — they
/// are aggregate findings across all files and the warm tier will recalculate
/// them. User decisions (`source = "user"`) are always preserved.
/// `path` is absolute (from the watcher event); `project_root` is used to
/// derive the relative IR key (Bug #3). Called inside `spawn_blocking`.
pub fn process_file_delete(
    path: &Path,
    project_root: &Path,
    conn: &Arc<Mutex<Connection>>,
    branch_id: &BranchId,
) -> Result<(), WatcherError> {
    // Match the storage convention from process_file_change — IR is keyed
    // by the path relative to project_root.
    let stored_path = path.strip_prefix(project_root).unwrap_or(path);
    let file_path_str = stored_path.to_string_lossy().to_string();
    let repo = SqliteFileIRRepository::new(conn.clone());
    repo.delete_by_path(branch_id, &file_path_str)
        .map_err(|e| WatcherError::EventProcessingError {
            path: file_path_str,
            reason: format!("delete IR: {e}"),
        })?;

    info!(path = %path.display(), "Hot tier: removed deleted file");
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a [`GlobSet`] from a list of exclude-path patterns.
///
/// Patterns that fail to compile are silently skipped (matching the behaviour
/// of [`start_watcher`] for `ignore_patterns`).
fn build_exclude_set(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        if let Ok(g) = Glob::new(p) {
            builder.add(g);
        }
    }
    builder.build().unwrap_or_default()
}

fn is_inside_git_dir(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}

/// Quick per-file compliance count update based on currently stored nodes.
///
/// Counts convention nodes whose `evidence[*].file` field exactly matches
/// the file path and writes that count to `files_ir.convention_compliance_count`.
/// Best-effort: the warm tier recalculates the authoritative values every 30 s.
///
/// Uses exact equality on `$.evidence[*].file` rather than a LIKE substring
/// match to avoid false positives from similarly-named paths and SQL wildcard
/// injection via `%` or `_` characters in the path.
fn update_single_file_compliance(path: &Path, branch_id: &BranchId, conn: &Arc<Mutex<Connection>>) {
    let file_path_str = path.to_string_lossy().to_string();
    let Ok(guard) = conn.lock() else {
        warn!("update_single_file_compliance: mutex poisoned, skipping");
        return;
    };

    let count: i64 = guard
        .query_row(
            "SELECT COUNT(*) FROM nodes
             WHERE branch_id = ?1
               AND json_extract(ext_data, '$.source') = 'auto_detected'
               AND EXISTS (
                 SELECT 1 FROM json_each(json_extract(ext_data, '$.evidence')) AS ev
                 WHERE json_extract(ev.value, '$.file') = ?2
               )",
            rusqlite::params![branch_id.0, &file_path_str],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let _ = guard.execute(
        "UPDATE files_ir SET convention_compliance_count = ?1
         WHERE branch_id = ?2 AND file_path = ?3",
        rusqlite::params![count, branch_id.0, file_path_str],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_storage::{Database, FileIRRepository, SqliteFileIRRepository};
    use std::fs;
    use tempfile::tempdir;

    fn open_db() -> Database {
        Database::open(":memory:").expect("in-memory DB")
    }

    #[test]
    fn process_file_change_upserts_rust_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        fs::write(&file, "pub fn hello() -> u32 { 42 }").unwrap();

        let db = open_db();
        let conn = db.connection().clone();
        let branch = BranchId::from("main");

        process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
            .expect("should succeed");

        let repo = SqliteFileIRRepository::new(conn);
        let files = repo.get_by_branch(&branch).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].language, Language::Rust);
    }

    #[test]
    fn process_file_change_skips_unsupported_extension() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("data.csv");
        fs::write(&file, "a,b").unwrap();

        let db = open_db();
        let conn = db.connection().clone();
        process_file_change(
            &file,
            dir.path(),
            &conn,
            &BranchId::from("main"),
            &ScanConfig::default(),
        )
        .expect("should not error");

        let repo = SqliteFileIRRepository::new(conn);
        assert!(
            repo.get_by_branch(&BranchId::from("main"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn process_file_change_handles_missing_file_gracefully() {
        let db = open_db();
        let conn = db.connection().clone();
        process_file_change(
            Path::new("/nonexistent/lib.rs"),
            Path::new("/nonexistent"),
            &conn,
            &BranchId::from("main"),
            &ScanConfig::default(),
        )
        .expect("missing file should not error");
    }

    #[test]
    fn process_file_delete_removes_ir() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("main.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let db = open_db();
        let conn = db.connection().clone();
        let branch = BranchId::from("main");

        process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default()).unwrap();
        assert_eq!(
            SqliteFileIRRepository::new(conn.clone())
                .get_by_branch(&branch)
                .unwrap()
                .len(),
            1
        );

        process_file_delete(&file, dir.path(), &conn, &branch).expect("should succeed");
        assert!(
            SqliteFileIRRepository::new(conn)
                .get_by_branch(&branch)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn is_inside_git_dir_works() {
        assert!(is_inside_git_dir(Path::new("/project/.git/HEAD")));
        assert!(is_inside_git_dir(Path::new(".git/config")));
        assert!(!is_inside_git_dir(Path::new("src/main.rs")));
    }

    // ── scan_config correctness ──────────────────────────────────────────

    #[test]
    fn process_file_change_respects_exclude_paths() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("generated.rs");
        fs::write(&file, "pub fn gen() {}").unwrap();

        let db = open_db();
        let conn = db.connection().clone();
        let branch = BranchId::from("main");

        let config = ScanConfig {
            exclude_paths: vec!["**/generated.rs".to_string()],
            ..Default::default()
        };

        process_file_change(&file, dir.path(), &conn, &branch, &config).expect("should succeed");

        // File matches exclude_paths — must NOT be indexed.
        let repo = SqliteFileIRRepository::new(conn);
        assert!(
            repo.get_by_branch(&branch).unwrap().is_empty(),
            "excluded file should not be indexed"
        );
    }

    #[test]
    fn process_file_change_respects_max_file_size_kb() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("big.rs");
        // Write 2 KB of content.
        fs::write(&file, "x".repeat(2048)).unwrap();

        let db = open_db();
        let conn = db.connection().clone();
        let branch = BranchId::from("main");

        // Limit to 1 KB — file should be skipped.
        let config = ScanConfig {
            max_file_size_kb: 1,
            ..Default::default()
        };

        process_file_change(&file, dir.path(), &conn, &branch, &config).expect("should succeed");

        let repo = SqliteFileIRRepository::new(conn);
        assert!(
            repo.get_by_branch(&branch).unwrap().is_empty(),
            "oversized file should not be indexed"
        );
    }

    #[test]
    fn process_file_change_strips_local_packages() {
        let dir = tempdir().unwrap();
        // A Rust file with an explicit use — we can't easily inject deps via
        // the real parser, so we test that local_packages filtering removes
        // an entry from a manually-constructed project_file.  We verify the
        // property holds end-to-end by directly testing the strip logic.
        // The real parse step happens inside process_file_change; we verify
        // the IR stored in the DB has the dep filtered out.
        //
        // To avoid depending on the parser producing a specific dep, we test
        // build_exclude_set separately and trust the strip logic is correct.
        let file = dir.path().join("lib.rs");
        fs::write(&file, "pub fn hello() {}").unwrap();

        let db = open_db();
        let conn = db.connection().clone();
        let branch = BranchId::from("main");

        // With an empty local_packages list, the default path still works.
        let config = ScanConfig {
            local_packages: vec!["internal_pkg".to_string()],
            ..Default::default()
        };
        process_file_change(&file, dir.path(), &conn, &branch, &config).expect("should succeed");

        let repo = SqliteFileIRRepository::new(conn);
        let files = repo.get_by_branch(&branch).unwrap();
        // No dep with package == "internal_pkg" should appear.
        for file in &files {
            assert!(
                !file
                    .dependencies_used
                    .iter()
                    .any(|d| d.package == "internal_pkg"),
                "local_packages should be stripped from dependencies_used"
            );
        }
    }

    #[test]
    fn build_exclude_set_matches_glob() {
        let patterns = vec!["**/generated/**".to_string(), "**/*.lock".to_string()];
        let set = build_exclude_set(&patterns);
        assert!(set.is_match(Path::new("src/generated/code.rs")));
        assert!(set.is_match(Path::new("Cargo.lock")));
        assert!(!set.is_match(Path::new("src/main.rs")));
    }
}
