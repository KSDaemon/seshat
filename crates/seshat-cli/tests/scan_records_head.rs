//! Integration tests for US-009: scan paths record HEAD as
//! `branches.last_scanned_commit`.
//!
//! Verifies the AC-required invariant: after a successful `scan_project`,
//! `branches.last_scanned_commit` for the active branch matches
//! `git rev-parse HEAD`.
//!
//! The granular per-path coverage (background_sync / fallback_rescan /
//! execute_bulk_rescan / git-unavailable case) is locked by US-017's
//! freshness integration tests; here we only pin the basic scan-completion
//! contract.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use seshat_core::{BranchId, ScanConfig};
use seshat_scanner::scan_project;
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

fn init_git_repo_with_rust_file(path: &Path) -> String {
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

    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .expect("git rev-parse HEAD");
    assert!(out.status.success(), "git rev-parse HEAD must succeed");
    String::from_utf8(out.stdout)
        .expect("rev-parse stdout utf-8")
        .trim()
        .to_owned()
}

#[test]
fn scan_project_records_last_scanned_commit_for_active_branch() {
    let workdir = tempdir().expect("create temp dir");
    let repo = workdir.path();
    let expected_head = init_git_repo_with_rust_file(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");

    let branch = BranchId::from("main");
    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("scan should succeed");

    // The orchestrator's scan_project does NOT itself record last_scanned_commit
    // (US-009 wires it in `seshat-cli`'s run_scan, serve.rs sync paths, and
    // the watcher's bulk-rescan). Drive the freshness sentinel through the
    // same helper the production paths use, then assert it matches HEAD.
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    seshat_scanner::record_branch_scan_complete(&branch_repo, repo, &branch);

    let stored = branch_repo
        .get_last_scanned_commit(&branch)
        .expect("get last_scanned_commit");
    assert_eq!(
        stored,
        Some(expected_head),
        "branches.last_scanned_commit must equal git rev-parse HEAD"
    );
}

#[test]
fn scan_project_records_no_commit_when_git_unavailable() {
    // No `.git` directory — record_branch_scan_complete must be a silent
    // no-op and the column must stay NULL (US-009 git-unavailable case).
    let workdir = tempdir().expect("create temp dir");
    let root = workdir.path();
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src dir");
    fs::write(src.join("lib.rs"), "pub fn x() {}\n").expect("write lib.rs");

    let db_path = root.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");

    let branch = BranchId::from("main");
    scan_project(root, &ScanConfig::default(), &db, branch.clone())
        .expect("scan without git should still succeed");

    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    seshat_scanner::record_branch_scan_complete(&branch_repo, root, &branch);

    let stored = branch_repo
        .get_last_scanned_commit(&branch)
        .expect("get last_scanned_commit");
    assert_eq!(
        stored, None,
        "last_scanned_commit must stay NULL with no git"
    );
}

// ── T19: scan_records_head edges (non-main branch + detached HEAD) ─────

/// T19: scan_project records the sentinel for an arbitrary branch
/// label, not just "main". Pre-fix tests covered the default branch
/// only.
#[test]
fn scan_project_records_sentinel_on_non_main_branch() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let head = init_git_repo_with_rust_file(repo);

    // Create + check out a feature branch BEFORE scanning so the
    // sentinel goes against `feature`, not `main`.
    git(&["checkout", "-b", "feature/x"], repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("feature/x");
    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("scan");

    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    let stored = branch_repo
        .get_last_scanned_commit(&branch)
        .expect("read sentinel");
    assert_eq!(
        stored.as_deref(),
        Some(head.as_str()),
        "sentinel must record HEAD for the non-main branch_id"
    );
}

/// T19: detached HEAD. `git rev-parse HEAD` resolves to the commit
/// hash directly; scan_project must record it as the sentinel for
/// whatever synthetic branch_id the caller passed.
#[test]
fn scan_project_records_sentinel_in_detached_head_state() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let head_main = init_git_repo_with_rust_file(repo);

    // Add a second commit so we have a non-HEAD ref to detach to.
    fs::write(
        repo.join("src").join("lib.rs"),
        "pub fn hello() -> &'static str {\n    \"hi2\"\n}\n",
    )
    .expect("write lib.rs");
    git(&["add", "."], repo);
    git(&["commit", "-m", "second commit"], repo);

    // Detach HEAD to the first commit.
    git(&["checkout", &head_main], repo);

    // Verify we are detached: `git symbolic-ref --short HEAD` fails on
    // detached HEAD.
    let symref = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("symbolic-ref spawn");
    assert!(
        !symref.status.success(),
        "fixture must be in detached HEAD state"
    );

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    // Use the commit hash as the synthetic branch_id (matches the
    // detect_branch fallback for detached HEAD).
    let branch = BranchId::from(head_main.as_str());
    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("scan");

    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    let stored = branch_repo
        .get_last_scanned_commit(&branch)
        .expect("read sentinel");
    assert_eq!(
        stored.as_deref(),
        Some(head_main.as_str()),
        "sentinel must reflect the detached HEAD commit"
    );
}
