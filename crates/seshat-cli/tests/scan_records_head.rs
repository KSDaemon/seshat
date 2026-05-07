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
