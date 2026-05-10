//! Integration tests for US-010: `seshat serve` detects HEAD change on the
//! same branch and triggers a background sync.
//!
//! `run_serve` runs an MCP server until Ctrl+C, which is impractical to drive
//! end-to-end. Instead we lock the freshness gate at the same layer
//! `run_serve` uses to decide whether to spawn `background_sync`:
//! [`seshat_scanner::check_branch_freshness`]. The mapping is exact —
//! `head_change_hint.is_some()` in `run_serve` evaluates to the same boolean
//! as `matches!(check, FreshnessCheck::Stale { .. })`, and that boolean is
//! what sets `needs_sync` and triggers the background thread.
//!
//! Broader coverage (branch-label change, git-unavailable, progress callback)
//! is owned by US-017's `serve_freshness.rs`.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use seshat_core::{BranchId, ScanConfig};
use seshat_scanner::{FreshnessCheck, check_branch_freshness, scan_project};
use seshat_storage::{BranchRepository, Database, SqliteBranchRepository};
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
    git(&["init", "-b", "main"], path);
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

/// Make a second commit that modifies an existing file. Returns the new HEAD SHA.
fn make_follow_up_commit(path: &Path) -> String {
    fs::write(
        path.join("src").join("lib.rs"),
        "pub fn hello() -> &'static str {\n    \"hello, world\"\n}\n",
    )
    .expect("modify lib.rs");
    git(&["add", "."], path);
    git(&["commit", "-m", "follow-up commit"], path);
    rev_parse_head(path)
}

/// AC integration test: the freshness gate that `run_serve` uses to spawn
/// `background_sync` returns `Stale` precisely when `last_scanned_commit`
/// no longer matches `git rev-parse HEAD` for the active branch.
///
/// Drives the full pipeline:
///   1. real `git init` + commit (head1)
///   2. `scan_project` (so the repo has IR)
///   3. `record_branch_scan_complete` (so `last_scanned_commit = head1`)
///   4. `check_branch_freshness` → must be `UpToDate`
///   5. second commit (head2 != head1) — i.e. the user `git pull`-ed
///   6. `check_branch_freshness` → must be `Stale { old_commit: Some(head1), new_commit: head2 }`
///
/// Step 6 is what `run_serve` checks at startup; a `Stale` return triggers
/// the same `background_sync` thread that a branch switch would.
#[test]
fn serve_freshness_gate_returns_stale_when_head_advances_on_same_branch() {
    let workdir = tempdir().expect("create temp dir");
    let repo = workdir.path();
    let head1 = init_repo_with_initial_commit(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("main");

    // Scan + record sentinel — emulates a successful prior `seshat serve`
    // startup that ran scan_project and called record_branch_scan_complete.
    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    seshat_scanner::record_branch_scan_complete(&branch_repo, repo, &branch);

    // Sanity: sentinel matches head1, freshness gate says UpToDate.
    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&branch)
            .expect("read sentinel"),
        Some(head1.clone()),
        "sentinel must be recorded at head1"
    );
    assert_eq!(
        check_branch_freshness(&branch_repo, repo, &branch),
        FreshnessCheck::UpToDate,
        "freshness gate must report UpToDate when sentinel matches HEAD"
    );

    // User `git pull`s on the same branch — HEAD advances to head2.
    let head2 = make_follow_up_commit(repo);
    assert_ne!(head1, head2, "follow-up commit must produce a new SHA");

    // The next `seshat serve` startup runs the freshness gate, which must
    // now report Stale and surface head1 as the OLD commit hint.
    let result = check_branch_freshness(&branch_repo, repo, &branch);
    assert_eq!(
        result,
        FreshnessCheck::Stale {
            old_commit: Some(head1),
            new_commit: head2,
        },
        "freshness gate must report Stale with the previously-recorded \
         commit as old_commit when HEAD advances on the same branch"
    );
}
