//! Integration tests for US-011: `seshat review` blocks on incremental sync.
//!
//! `run_review` ends in an interactive TUI which can't be driven end-to-end
//! from a test, so we lock the gate at the same layer the CLI uses to decide
//! whether to sync: [`seshat_cli::review::prepare_review_sync`]. Its return
//! value (`ReviewSyncOutcome`) is precisely what `run_review` keys off. The
//! TUI launches with the same `Database` handle the sync wrote into, so
//! "TUI receives fresh data" is equivalent to "the DB the gate returned was
//! sync'd to current HEAD".
//!
//! These tests cover the AC paths:
//! - stale `last_scanned_commit` → sync runs, `files_ir` reflects HEAD
//! - progress callback emits at least one update for a non-trivial diff
//! - git unavailable → sync skipped silently, TUI launches without errors
//! - `--no-sync` → freshness gate not consulted

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use seshat_cli::review::{ReviewSyncOutcome, prepare_review_sync};
use seshat_core::{BranchId, ScanConfig};
use seshat_scanner::{record_branch_scan_complete, scan_project};
use seshat_storage::{
    BranchRepository, Database, FileIRRepository, SqliteBranchRepository, SqliteFileIRRepository,
};
use tempfile::tempdir;

fn git(args: &[&str], cwd: &Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(status.success(), "git {args:?} failed in {cwd:?}");
}

fn rev_parse_head(cwd: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .expect("git rev-parse HEAD");
    assert!(out.status.success(), "git rev-parse HEAD must succeed");
    String::from_utf8(out.stdout)
        .expect("rev-parse stdout utf-8")
        .trim()
        .to_owned()
}

/// Initialise a git repo at `path`, commit a single Rust source file, and
/// return the resulting HEAD SHA.
fn init_repo_with_initial_commit(path: &Path) -> String {
    git(&["init"], path);
    git(&["config", "user.email", "test@seshat.dev"], path);
    git(&["config", "user.name", "Seshat Test"], path);

    let src = path.join("src");
    fs::create_dir_all(&src).expect("create src dir");
    fs::write(
        src.join("lib.rs"),
        "pub fn hello() -> &'static str {\n    \"hi\"\n}\n",
    )
    .expect("write lib.rs");

    git(&["add", "."], path);
    git(&["commit", "-m", "initial commit"], path);
    rev_parse_head(path)
}

/// Make a second commit that ADDS a new Rust source file. A new file (rather
/// than a modification) is the cleanest way to get the diff sync to do real
/// work — `incremental_sync_blocking` upserts files whose tree-oid changed,
/// and a new path is unambiguously a non-skip.
fn make_follow_up_commit_adding_file(path: &Path) -> String {
    let new_file = path.join("src").join("util.rs");
    fs::write(&new_file, "pub fn answer() -> u32 { 42 }\n").expect("write util.rs");
    git(&["add", "."], path);
    git(&["commit", "-m", "add util.rs"], path);
    rev_parse_head(path)
}

/// AC: with a stale `last_scanned_commit`, `prepare_review_sync` runs the
/// blocking sync, the freshness sentinel advances, and `files_ir` contains the
/// newly-added file (i.e. "TUI receives fresh data").
#[test]
fn review_blocks_on_sync_when_head_changed() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let head1 = init_repo_with_initial_commit(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("main");

    // Initial scan + sentinel — emulates a successful prior `seshat review`.
    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    record_branch_scan_complete(&branch_repo, repo, &branch);
    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&branch)
            .expect("read sentinel"),
        Some(head1.clone()),
        "sentinel must be recorded at head1 before the follow-up commit"
    );

    // The user runs `git pull` (here: a follow-up commit). HEAD advances to
    // head2 and a new source file appears on disk.
    let head2 = make_follow_up_commit_adding_file(repo);
    assert_ne!(head1, head2, "follow-up commit must produce a new SHA");

    // Sanity: the DB does NOT yet know about the new file.
    let file_ir_repo = SqliteFileIRRepository::new(db.connection().clone());
    let pre_sync_files = file_ir_repo
        .get_file_hashes_by_branch(&branch)
        .expect("pre-sync file list");
    assert!(
        !pre_sync_files.iter().any(|(p, _)| p.ends_with("util.rs")),
        "util.rs should NOT be in files_ir before the sync; got {pre_sync_files:?}"
    );

    // Drive the same gate `run_review` uses.
    let outcome = prepare_review_sync(&db, repo, &branch, false, None);
    match outcome {
        ReviewSyncOutcome::Synced {
            old_commit,
            new_commit,
            progress_emits,
        } => {
            assert_eq!(old_commit, Some(head1));
            assert_eq!(new_commit, head2.clone());
            // Two callbacks fire per processed file (top-of-iteration, plus
            // the final (total, total) tick) — at least one emit is required.
            assert!(
                progress_emits >= 1,
                "progress callback must fire at least once for a non-trivial diff (got {progress_emits})"
            );
        }
        other => panic!("expected Synced, got {other:?}"),
    }

    // The sentinel must now match HEAD…
    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&branch)
            .expect("read sentinel post-sync"),
        Some(head2),
        "sentinel must advance to head2 after the blocking sync completes"
    );

    // …and the TUI's connection (which is the SAME Database handle used by
    // the sync) sees the newly-added file in `files_ir`. This is the
    // "TUI receives fresh data" half of the AC.
    let post_sync_files = file_ir_repo
        .get_file_hashes_by_branch(&branch)
        .expect("post-sync file list");
    assert!(
        post_sync_files.iter().any(|(p, _)| p.ends_with("util.rs")),
        "util.rs MUST appear in files_ir after the blocking sync; got {post_sync_files:?}"
    );
}

/// AC: progress callback emits at least one update for a non-trivial diff.
///
/// Exercises `prepare_review_sync`'s wrapper-counter alongside an explicit
/// user-supplied callback, asserting both fire on the same diff.
#[test]
fn review_progress_updates_emitted_during_sync() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let _head1 = init_repo_with_initial_commit(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("main");

    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    record_branch_scan_complete(&branch_repo, repo, &branch);

    let _head2 = make_follow_up_commit_adding_file(repo);

    let user_emits = AtomicUsize::new(0);
    let last_total = AtomicUsize::new(0);
    let cb = |processed: usize, total: usize| {
        user_emits.fetch_add(1, Ordering::Relaxed);
        // Each emit must report a non-decreasing processed counter and a
        // stable total. Pin total via a "last seen" comparison.
        let prev_total = last_total.swap(total, Ordering::Relaxed);
        if prev_total != 0 {
            assert_eq!(
                prev_total, total,
                "total must be stable across progress emits"
            );
        }
        assert!(
            processed <= total,
            "processed ({processed}) must not exceed total ({total})"
        );
    };

    let outcome = prepare_review_sync(&db, repo, &branch, false, Some(&cb));

    match outcome {
        ReviewSyncOutcome::Synced { progress_emits, .. } => {
            assert!(
                progress_emits >= 1,
                "wrapper-counter must record at least one emit (got {progress_emits})"
            );
            assert_eq!(
                progress_emits,
                user_emits.load(Ordering::Relaxed),
                "wrapper-counter and user-supplied callback must fire on the same events"
            );
        }
        other => panic!("expected Synced, got {other:?}"),
    }

    assert!(
        last_total.load(Ordering::Relaxed) > 0,
        "the final emit must report a non-zero total"
    );
}

/// AC: git unavailable → no sync runs, no errors, no warnings.
#[test]
fn review_handles_git_unavailable_gracefully() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    // No `git init` here — `repo` is a plain temp directory.

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("main");

    // Even if a sentinel is somehow set (e.g. legacy data), git-unavailable
    // wins per US-009 / US-010 — `prepare_review_sync` must short-circuit.
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    branch_repo
        .ensure_branch_exists(&branch)
        .expect("ensure branch");

    let cb_emits = AtomicUsize::new(0);
    let cb = |_processed: usize, _total: usize| {
        cb_emits.fetch_add(1, Ordering::Relaxed);
    };

    let outcome = prepare_review_sync(&db, repo, &branch, false, Some(&cb));
    assert_eq!(
        outcome,
        ReviewSyncOutcome::GitUnavailable,
        "non-git directory must short-circuit the freshness gate"
    );
    assert_eq!(
        cb_emits.load(Ordering::Relaxed),
        0,
        "no sync must run on git-unavailable, so the progress callback must not fire"
    );
    // Sentinel stays NULL since no scan completed (git-unavailable path).
    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&branch)
            .expect("read sentinel"),
        None,
        "last_scanned_commit must remain NULL when git is unavailable"
    );
}

/// AC: `--no-sync` flag bypasses the freshness gate entirely (emergency / debug).
#[test]
fn review_no_sync_flag_skips_freshness_gate() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let head1 = init_repo_with_initial_commit(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("main");

    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    record_branch_scan_complete(&branch_repo, repo, &branch);

    // Make the gate STALE on disk so a non-no_sync invocation would sync.
    let head2 = make_follow_up_commit_adding_file(repo);
    assert_ne!(head1, head2);

    let cb_emits = AtomicUsize::new(0);
    let cb = |_processed: usize, _total: usize| {
        cb_emits.fetch_add(1, Ordering::Relaxed);
    };

    // no_sync=true → must return Skipped without running the sync.
    let outcome = prepare_review_sync(&db, repo, &branch, true, Some(&cb));
    assert_eq!(outcome, ReviewSyncOutcome::Skipped);
    assert_eq!(
        cb_emits.load(Ordering::Relaxed),
        0,
        "--no-sync must not invoke the sync path"
    );
    // Sentinel must NOT have advanced — that's the contract of --no-sync:
    // open the TUI on the existing snapshot.
    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&branch)
            .expect("read sentinel"),
        Some(head1),
        "--no-sync must leave last_scanned_commit untouched"
    );
}

/// AC: `prepare_review_sync` reports `UpToDate` when the sentinel matches HEAD;
/// no sync runs and the progress callback is never invoked.
#[test]
fn review_skips_sync_when_head_unchanged() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let head1 = init_repo_with_initial_commit(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("main");

    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    record_branch_scan_complete(&branch_repo, repo, &branch);

    let cb_emits = AtomicUsize::new(0);
    let cb = |_processed: usize, _total: usize| {
        cb_emits.fetch_add(1, Ordering::Relaxed);
    };

    let outcome = prepare_review_sync(&db, repo, &branch, false, Some(&cb));
    assert_eq!(outcome, ReviewSyncOutcome::UpToDate);
    assert_eq!(
        cb_emits.load(Ordering::Relaxed),
        0,
        "UpToDate must not invoke the progress callback"
    );
    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&branch)
            .expect("read sentinel"),
        Some(head1),
        "sentinel must be unchanged on UpToDate path"
    );
}
