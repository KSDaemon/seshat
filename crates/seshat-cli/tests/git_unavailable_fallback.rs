//! Integration tests for US-012: git-unavailable single-branch fallback.
//!
//! Verifies that the full lifecycle — scan + review + decide + rescan —
//! works in a non-git tmp directory, locking the synthetic single-branch
//! contract end-to-end. The individual freshness-check helper cases
//! (`scan_records_head.rs::scan_project_records_no_commit_when_git_unavailable`,
//! `review_freshness.rs::review_handles_git_unavailable_gracefully`) pin
//! their respective layers; this file pins the merged user-facing flow.
//!
//! AC mapping (PRD §US-012):
//! 1. `detect_branch` returns "main" when no `.git` is found
//!    → [`detect_branch_falls_back_to_main_when_no_git`]
//! 2. Freshness comparisons treat git rev-parse HEAD failure as "no change"
//!    → [`full_lifecycle_works_without_git`] (asserts
//!    `check_branch_freshness == GitUnavailable`)
//! 3. All scan paths set `last_scanned_commit = NULL` when git is unavailable
//!    → [`full_lifecycle_works_without_git`] (sentinel stays NULL across
//!    both initial scan AND rescan)
//! 4. Decision flow operates as on a single-branch project
//!    (`decided_on_branch = "main"`)
//!    → [`full_lifecycle_works_without_git`]
//! 5. Integration test: scan + review + decide + rescan in a non-git tmp dir
//!    → decisions persist, no errors
//!    → [`full_lifecycle_works_without_git`]

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use seshat_cli::tui::app::{ReviewAction, apply_review_actions};
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, ScanConfig};
use seshat_detectors::{ProjectContext, aggregate_findings, run_all_detectors};
use seshat_graph::compute_description_hash;
use seshat_scanner::{
    FreshnessCheck, check_branch_freshness, record_branch_scan_complete, scan_project,
};
use seshat_storage::{
    BranchRepository, Database, DecisionRepository, DecisionState, FileIRRepository,
    NodeRepository, SqliteBranchRepository, SqliteDecisionRepository, SqliteFileIRRepository,
    SqliteNodeRepository,
};
use tempfile::tempdir;

/// Drop a small tree of Rust source files into `root` so the detector
/// pipeline has enough material to surface at least one convention.
/// Crucially, this helper does NOT call `git init` — `root` stays a
/// plain directory for the duration of the test.
fn write_rust_sources(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src dir");

    fs::write(
        src.join("lib.rs"),
        r#"
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}

pub fn multiply(a: i32, b: i32) -> i32 {
    a * b
}
"#,
    )
    .expect("write lib.rs");

    fs::write(
        src.join("main.rs"),
        r#"
mod lib;

fn main() {
    let result = lib::add(1, 2);
    println!("{}", result);
}
"#,
    )
    .expect("write main.rs");

    fs::write(
        src.join("errors.rs"),
        r#"
use std::fmt;

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    InvalidInput(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::NotFound(msg) => write!(f, "Not found: {msg}"),
            AppError::InvalidInput(msg) => write!(f, "Invalid input: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}
"#,
    )
    .expect("write errors.rs");
}

/// Run the production scan + detection + persist pipeline against `repo`,
/// then call the post-scan freshness hook (`record_branch_scan_complete`)
/// the way `seshat scan` and the serve/watcher sync paths do. Returns the
/// auto-detected conventions visible after persist.
///
/// This mirrors `crates/seshat-cli/tests/tui_review_integration.rs::scan_and_get_conventions`
/// but with the post-scan hook layered on so the test actually exercises
/// the AC#3 invariant (`last_scanned_commit = NULL` when git is unavailable).
fn scan_and_persist(repo: &Path) -> Vec<KnowledgeNode> {
    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");

    let branch_id = BranchId::from("main");
    let scan_result = scan_project(repo, &ScanConfig::default(), &db, branch_id.clone())
        .expect("scan must succeed without git");

    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let all_files = file_ir_repo
        .get_by_branch(&branch_id)
        .expect("load files for detection");

    let detection_config = DetectionConfig::default();
    let project_context = ProjectContext::from_files(&all_files);
    let detector_results = run_all_detectors(
        &all_files,
        &scan_result.source_map,
        &detection_config,
        &project_context,
        None,
    );
    let all_findings: Vec<seshat_core::ConventionFinding> = detector_results
        .into_iter()
        .flat_map(|dr| dr.findings)
        .collect();

    let aggregated = aggregate_findings(
        &all_findings,
        &detection_config,
        &HashMap::new(),
        chrono::Utc::now().timestamp(),
    );

    seshat_graph::persist_and_index(&conn, &branch_id, &aggregated, &all_findings)
        .expect("persist conventions");

    // Production paths (run_scan, background_sync, fallback_rescan,
    // execute_bulk_rescan) all call this helper after a successful scan.
    // It MUST be a silent no-op when git is unavailable (US-009 AC).
    let branch_repo = SqliteBranchRepository::new(conn.clone());
    record_branch_scan_complete(&branch_repo, repo, &branch_id);

    let node_repo = SqliteNodeRepository::new(conn);
    node_repo
        .find_conventions_by_branch(&branch_id)
        .expect("query conventions")
}

/// AC#1: `detect_branch` returns "main" when no `.git` is found. This is
/// the "synthetic-branch identity" that everything else in this story
/// relies on — without it, decisions would be scoped to whatever weird
/// fallback string `detect_branch` produced.
#[test]
fn detect_branch_falls_back_to_main_when_no_git() {
    let workdir = tempdir().expect("tempdir");
    // Sanity: the temp dir really has no .git anywhere up the tree —
    // tempdir() uses /tmp on macOS/Linux, which is not inside a git repo.
    assert!(
        !workdir.path().join(".git").exists(),
        "fixture must NOT contain a .git directory"
    );

    let branch = seshat_cli::db::detect_branch(workdir.path());
    assert_eq!(
        branch, "main",
        "non-git dir must yield the synthetic 'main'"
    );
}

/// AC#5 (the omnibus integration test): scan + review + decide + rescan
/// in a non-git tmp dir → decisions persist, no errors. Exercises every
/// AC point in one flow:
///
/// - AC#1: `detect_branch` returns "main"
/// - AC#2: `check_branch_freshness` returns `GitUnavailable` → no sync triggered
/// - AC#3: `last_scanned_commit` stays NULL across both the initial scan AND the rescan
/// - AC#4: the decision row's `decided_on_branch` is "main"
/// - AC#5: the decision survives the rescan AND the convention is not re-emitted
#[test]
fn full_lifecycle_works_without_git() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);
    assert!(
        !repo.join(".git").exists(),
        "fixture MUST NOT be a git repo for the AC to be meaningful"
    );

    // AC#1: synthetic-branch identity.
    let branch_str = seshat_cli::db::detect_branch(repo);
    assert_eq!(
        branch_str, "main",
        "non-git dir must yield the synthetic 'main' (AC#1)"
    );
    let branch = BranchId::from(branch_str.as_str());

    // ── First scan ──────────────────────────────────────────────────────
    let conventions1 = scan_and_persist(repo);
    assert!(
        !conventions1.is_empty(),
        "scan must surface at least one auto-detected convention even without git"
    );

    // AC#3: sentinel must stay NULL after a successful scan when git is unavailable.
    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn = db.connection().clone();
    let branch_repo = SqliteBranchRepository::new(conn.clone());
    let sentinel_after_first = branch_repo
        .get_last_scanned_commit(&branch)
        .expect("read sentinel");
    assert_eq!(
        sentinel_after_first, None,
        "last_scanned_commit must stay NULL after scan when git is unavailable (AC#3)"
    );

    // AC#2: the freshness gate must short-circuit to GitUnavailable so neither
    // `seshat serve` nor `seshat review` trigger a sync.
    assert_eq!(
        check_branch_freshness(&branch_repo, repo, &branch),
        FreshnessCheck::GitUnavailable,
        "freshness gate must report GitUnavailable for a non-git dir (AC#2)"
    );

    // ── Decide: confirm the first auto-detected convention via the review action path ──
    let first = &conventions1[0];
    let first_description = first.description.clone();
    let conn_arc: Arc<Mutex<rusqlite::Connection>> = conn.clone();
    let actions = vec![ReviewAction::Confirm {
        node_id: first.id.0,
        description: first_description.clone(),
        examples: Vec::new(),
    }];
    apply_review_actions(&conn_arc, "main", &actions)
        .expect("apply_review_actions must succeed without git");

    // AC#4: the decision row exists, is keyed by description_hash, has
    // state=Approved, and decided_on_branch="main" (the synthetic identity).
    let decision_repo = SqliteDecisionRepository::new(conn_arc.clone());
    let hash = compute_description_hash(&first_description);
    let decision = decision_repo
        .get_by_hash(&hash)
        .expect("get_by_hash should succeed")
        .expect("decision row must exist after Confirm");
    assert_eq!(decision.state, DecisionState::Approved);
    assert_eq!(
        decision.decided_on_branch,
        BranchId::from("main"),
        "decision must be scoped to the synthetic 'main' branch (AC#4)"
    );

    // ── Rescan ─────────────────────────────────────────────────────────
    let conventions2 = scan_and_persist(repo);

    // AC#5: the decision survives the rescan.
    let decision_after = decision_repo
        .get_by_hash(&hash)
        .expect("get_by_hash should succeed post-rescan")
        .expect("decision row must persist across rescans (AC#5)");
    assert_eq!(decision_after.state, DecisionState::Approved);
    assert_eq!(decision_after.decided_on_branch, BranchId::from("main"));

    // AC#5 (cont.): the confirmed convention is NOT re-emitted as auto-detected
    // because `persist_conventions` (US-008) consults the decisions table on
    // every insert. The merge-aware contract holds even with no git history.
    let post_rescan_descriptions: Vec<_> =
        conventions2.iter().map(|c| c.description.clone()).collect();
    assert!(
        !post_rescan_descriptions.contains(&first_description),
        "decided convention must not be re-emitted on rescan (AC#5); got {post_rescan_descriptions:?}"
    );

    // AC#3 (cont.): the sentinel must STILL be NULL after the rescan.
    let sentinel_after_rescan = branch_repo
        .get_last_scanned_commit(&branch)
        .expect("read sentinel post-rescan");
    assert_eq!(
        sentinel_after_rescan, None,
        "last_scanned_commit must stay NULL across rescans without git (AC#3)"
    );
}

/// Defensive guard: `find_conventions_by_branch` queried with the synthetic
/// "main" branch returns rows in a non-git directory. Without this the AC#4
/// "decision flow operates as on a single-branch project" claim is unprovable
/// — every decision would be scoped to "main" but the queries that read
/// decisions/nodes back must agree.
#[test]
fn queries_scoped_to_main_return_results_in_non_git_dir() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);
    assert!(!repo.join(".git").exists());

    let conventions = scan_and_persist(repo);
    assert!(!conventions.is_empty());

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let conn = db.connection().clone();

    // The branch registry contains exactly one branch — "main" — because
    // every scan path registers the active branch via `ensure_branch_exists`
    // (US-003) and `detect_branch` always falls back to "main" without git.
    let branch_repo = SqliteBranchRepository::new(conn.clone());
    let branches = branch_repo.list_branches().expect("list branches");
    assert!(
        branches.contains(&BranchId::from("main")),
        "branches table must contain the synthetic 'main' branch; got {branches:?}"
    );

    // Node lookups scoped to "main" return the auto-detected conventions
    // produced by the scan — proves the same-branch read path works.
    let node_repo = SqliteNodeRepository::new(conn);
    let by_main = node_repo
        .find_conventions_by_branch(&BranchId::from("main"))
        .expect("query nodes scoped to main");
    assert!(
        !by_main.is_empty(),
        "find_conventions_by_branch('main') must return rows in a non-git dir"
    );
}
