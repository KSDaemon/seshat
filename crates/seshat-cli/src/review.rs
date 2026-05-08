//! Implementation of the `seshat review` command.
//!
//! Resolves the project DB, reads the active branch's `last_scanned_commit`,
//! and — unless `--no-sync` is passed — performs a blocking incremental sync
//! to the current `git rev-parse HEAD` before opening the TUI. The TUI then
//! reads conventions that reflect the on-disk state, not a potentially stale
//! snapshot from a previous scan.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use seshat_core::BranchId;
use seshat_scanner::{FreshnessCheck, check_branch_freshness};
use seshat_storage::{Database, SqliteBranchRepository};

use crate::config::AppConfig;
use crate::error::CliError;

/// Minimum delay between progress callback emits when stderr is a TTY.
///
/// Per US-011 AC: the progress UI updates "Files: X / Y on the same line at
/// 1Hz" — so the throttle MUST allow 1 emit per second. Using exactly 1 second
/// flickers under load when calls fall on either side of the boundary; 950 ms
/// gives a stable ~1 Hz cadence without crossing into the "more than 1 Hz" zone.
const TTY_PROGRESS_INTERVAL: Duration = Duration::from_millis(950);

/// Outcome returned by [`prepare_review_sync`] so tests can drive the freshness
/// gate without launching the (interactive) TUI.
///
/// Mirrors the variants of [`FreshnessCheck`] one-to-one for the gate cases,
/// plus a `Synced` variant that carries the file counts the AC requires the
/// progress callback to surface. `Skipped` covers `--no-sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewSyncOutcome {
    /// `--no-sync` was passed; sync gate was not consulted.
    Skipped,
    /// Freshness gate said up-to-date; no sync was run.
    UpToDate,
    /// Git was unavailable for the project root; sync was skipped silently.
    GitUnavailable,
    /// A blocking sync ran. `progress_emits` is the number of times the user-
    /// facing progress callback fired (independent of internal upsert events,
    /// which the throttle may have collapsed).
    Synced {
        old_commit: Option<String>,
        new_commit: String,
        progress_emits: usize,
    },
}

/// Run the freshness gate and (when stale) the blocking incremental sync.
///
/// Extracted from [`run_review`] so integration tests can lock the gate at the
/// same layer the CLI uses without spawning the TUI. The returned
/// [`ReviewSyncOutcome`] is precisely what determines whether `run_review` calls
/// `incremental_sync_blocking` before opening the TUI.
///
/// `progress_callback` lets tests inject a counting callback. In production,
/// `run_review` passes a stderr-printing throttled callback (see
/// [`tty_progress_printer`] / [`piped_progress_printer`]).
pub fn prepare_review_sync(
    db: &Database,
    project_root: &std::path::Path,
    branch_id: &BranchId,
    no_sync: bool,
    progress_callback: Option<&dyn Fn(usize, usize)>,
) -> ReviewSyncOutcome {
    if no_sync {
        tracing::debug!(
            branch = %branch_id.0,
            "review: --no-sync passed, skipping freshness check"
        );
        return ReviewSyncOutcome::Skipped;
    }

    // Resolve git root so the freshness check + sync work even when the user
    // ran `seshat review` from a subdirectory of a git worktree.
    let sync_root =
        crate::db::find_git_root(project_root).unwrap_or_else(|| project_root.to_path_buf());

    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    let freshness = check_branch_freshness(&branch_repo, &sync_root, branch_id);

    let (old_commit, new_commit) = match freshness {
        FreshnessCheck::UpToDate => return ReviewSyncOutcome::UpToDate,
        FreshnessCheck::GitUnavailable => return ReviewSyncOutcome::GitUnavailable,
        FreshnessCheck::Stale {
            old_commit,
            new_commit,
        } => (old_commit, new_commit),
    };

    let config = AppConfig::load().unwrap_or_default();

    // Wrap the user-supplied callback with a counter so tests can assert at
    // least one emit was observed (AC: progress callback emits at least one
    // update for a non-trivial diff). The wrapper increments BEFORE forwarding
    // so the count reflects callback invocations, not iterations of the inner
    // upsert loop.
    let emits = std::sync::atomic::AtomicUsize::new(0);
    let counted_cb = |processed: usize, total: usize| {
        emits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(cb) = progress_callback {
            cb(processed, total);
        }
    };

    crate::serve::incremental_sync_blocking(
        &sync_root,
        old_commit.as_deref(),
        &branch_id.0,
        db,
        branch_id,
        &config.scan,
        &config.detection,
        Some(&counted_cb),
    );

    ReviewSyncOutcome::Synced {
        old_commit,
        new_commit,
        progress_emits: emits.load(std::sync::atomic::Ordering::Relaxed),
    }
}

/// Build a TTY-aware throttled progress printer.
///
/// Emits `Syncing project state to <head[..7]>... Files: X / Y` to stderr,
/// rewriting the same line at most once per [`TTY_PROGRESS_INTERVAL`] (≈1 Hz).
/// The final `(total, total)` tick is always emitted (the throttle gates only
/// the intermediate updates) so the user sees the completion line.
fn tty_progress_printer(head_short: String) -> impl Fn(usize, usize) {
    let last_emit = Mutex::new(Instant::now() - TTY_PROGRESS_INTERVAL);
    move |processed: usize, total: usize| {
        let mut guard = match last_emit.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if processed < total && guard.elapsed() < TTY_PROGRESS_INTERVAL {
            return;
        }
        *guard = Instant::now();
        drop(guard);
        let mut stderr = std::io::stderr().lock();
        let _ = write!(
            stderr,
            "\rSyncing project state to {head_short}... Files: {processed} / {total}    "
        );
        let _ = stderr.flush();
    }
}

/// Build a non-TTY (piped) progress printer.
///
/// Emits one line per throttled update: `Syncing files: X / Y`. Used when
/// stderr is not a terminal (CI logs, tee, redirected output) so that lines
/// are preserved instead of being overwritten with carriage returns.
fn piped_progress_printer(head_short: String) -> impl Fn(usize, usize) {
    let last_emit = Mutex::new(Instant::now() - TTY_PROGRESS_INTERVAL);
    move |processed: usize, total: usize| {
        let mut guard = match last_emit.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if processed < total && guard.elapsed() < TTY_PROGRESS_INTERVAL {
            return;
        }
        *guard = Instant::now();
        drop(guard);
        eprintln!("Syncing project state to {head_short}: {processed} / {total} files");
    }
}

/// Run the `seshat review` command end-to-end.
///
/// 1. Resolves the project DB and active branch.
/// 2. Unless `no_sync` is set, runs the freshness gate; on `Stale`, runs an
///    incremental blocking sync to HEAD with a stderr progress UI (TTY: same-
///    line `\r` rewrite at 1 Hz; piped: one line per throttled update).
/// 3. Launches the TUI with the (now-fresh) connection.
pub fn run_review(project_path: Option<PathBuf>, no_sync: bool) -> Result<(), CliError> {
    // Resolve the project — shared resolver also used by serve/status.
    let explicit = project_path.as_deref();
    let resolved = crate::db::resolve_project(explicit, "review")?;

    // Check that the database actually exists.
    if !resolved.db_path.exists() {
        return Err(CliError::CommandFailed {
            command: "review".to_owned(),
            reason: "No database found. Run `seshat scan` first.".to_owned(),
        });
    }

    // Determine branch once and pass it through to all downstream calls.
    let branch_id_str =
        crate::db::get_current_branch(&resolved.project_root).unwrap_or_else(|| {
            tracing::debug!(
               path = %resolved.project_root.display(),
                "Could not detect git branch, defaulting to 'main'"
            );
            "main".to_string()
        });
    let branch_id = BranchId::from(branch_id_str.as_str());

    // Open via Database so the freshness check, the blocking sync, and the TUI
    // can all share one Arc<Mutex<Connection>> handle.
    let db = Database::open(&resolved.db_path).map_err(|e| CliError::CommandFailed {
        command: "review".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    // -- Freshness gate + blocking sync ---------------------------------
    if no_sync {
        prepare_review_sync(&db, &resolved.project_root, &branch_id, true, None);
    } else {
        // Pre-flight the gate so we can announce the sync BEFORE opening the
        // sync stream. We re-run the gate inside `prepare_review_sync`, but
        // the cost is two cheap reads (gix HEAD + one SELECT) — much simpler
        // than threading the FreshnessCheck out of the helper.
        let sync_root = crate::db::find_git_root(&resolved.project_root)
            .unwrap_or_else(|| resolved.project_root.clone());
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        match check_branch_freshness(&branch_repo, &sync_root, &branch_id) {
            FreshnessCheck::UpToDate => {
                tracing::debug!(branch = %branch_id.0, "review: DB is up to date with HEAD");
            }
            FreshnessCheck::GitUnavailable => {
                tracing::debug!(
                    root = %sync_root.display(),
                    "review: git unavailable, skipping freshness check"
                );
            }
            FreshnessCheck::Stale { ref new_commit, .. } => {
                let head_short: String = new_commit.chars().take(7).collect();
                // P22: PRD US-011 specifies stdout for the user-facing
                // banner ("Syncing project state to ..."). The progress
                // printer keeps writing to stderr — that's the standard
                // place for transient progress info that should not
                // pollute redirected stdout, and the printers handle TTY
                // detection on stderr internally. The TTY check here
                // gates the banner format only, against stdout, since
                // that's what the spec wires to it.
                let is_stdout_tty = std::io::stdout().is_terminal();
                if is_stdout_tty {
                    print!("Syncing project state to {head_short}... ");
                    let _ = std::io::stdout().lock().flush();
                } else {
                    println!("Syncing project state to {head_short}...");
                }
                if is_stdout_tty {
                    let printer = tty_progress_printer(head_short.clone());
                    prepare_review_sync(
                        &db,
                        &resolved.project_root,
                        &branch_id,
                        false,
                        Some(&printer),
                    );
                    // Newline after the in-place progress line, plus a "done"
                    // marker so the user knows the sync finished before the
                    // TUI takes over the screen.
                    println!("\rSyncing project state to {head_short}... done.            ");
                    let _ = std::io::stdout().lock().flush();
                } else {
                    let printer = piped_progress_printer(head_short.clone());
                    prepare_review_sync(
                        &db,
                        &resolved.project_root,
                        &branch_id,
                        false,
                        Some(&printer),
                    );
                    println!("Sync complete.");
                    let _ = std::io::stdout().lock().flush();
                }
            }
        }
    }

    let conn = db.connection().clone();
    crate::tui::run_review_tui_with_conn(&branch_id_str, &conn)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn run_review_nonexistent_project_returns_error() {
        let result = run_review(
            Some(PathBuf::from("/tmp/seshat-nonexistent-review-test-xyz")),
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn run_review_with_some_path_sets_deref() {
        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join("seshat.db");

        std::fs::write(&db_path, "fake db").unwrap();

        let result = run_review(Some(tmp.path().to_path_buf()), false);
        assert!(result.is_err());
    }

    #[test]
    fn run_review_file_instead_of_directory_error() {
        let tmp = tempdir().unwrap();
        let file_path = tmp.path().join("just_a_file");
        std::fs::write(&file_path, "hello").unwrap();
        let result = run_review(Some(file_path), false);
        assert!(result.is_err());
    }

    // ── prepare_review_sync ─────────────────────────────────────────────

    #[test]
    fn prepare_review_sync_returns_skipped_when_no_sync_passed() {
        let dir = tempdir().expect("tempdir");
        let db = Database::open(":memory:").expect("open db");
        let branch = BranchId::from("main");

        let outcome = prepare_review_sync(&db, dir.path(), &branch, true, None);
        assert_eq!(outcome, ReviewSyncOutcome::Skipped);
    }

    #[test]
    fn prepare_review_sync_returns_git_unavailable_for_non_git_directory() {
        let dir = tempdir().expect("tempdir");
        let db = Database::open(":memory:").expect("open db");
        let branch = BranchId::from("main");

        let outcome = prepare_review_sync(&db, dir.path(), &branch, false, None);
        assert_eq!(outcome, ReviewSyncOutcome::GitUnavailable);
    }
}
