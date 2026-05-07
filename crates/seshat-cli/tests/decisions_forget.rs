//! Integration test for US-014: `seshat decisions forget`.
//!
//! Locks the AC behaviour end-to-end:
//!   scan → confirm convention → forget decision → rescan re-emits convention
//!
//! `run_forget` opens stdin to confirm, so the test drives the unattended
//! seam [`seshat_cli::decisions::forget_decision_with_database`] (the same
//! resolve-then-delete pair the `--yes` branch of `run_forget` invokes).

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use seshat_cli::decisions::forget_decision_with_database;
use seshat_cli::tui::app::{ReviewAction, apply_review_actions};
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, ScanConfig};
use seshat_detectors::{ProjectContext, aggregate_findings, run_all_detectors};
use seshat_graph::compute_description_hash;
use seshat_scanner::scan_project;
use seshat_storage::{Database, DecisionRepository, DecisionState, SqliteDecisionRepository};
use tempfile::tempdir;

/// Drop a small Rust source tree into `root` so the detector pipeline has
/// enough material to surface at least one auto-detected convention.
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

/// Mirror the production scan + detect + persist pipeline, returning the
/// auto-detected conventions visible after persist. Same shape as the helper
/// in `git_unavailable_fallback.rs::scan_and_persist`, minus the post-scan
/// freshness sentinel write — it isn't load-bearing for THIS test (US-014's
/// AC is about the decision lifecycle, not the freshness sentinel).
fn scan_and_persist(repo: &Path) -> Vec<KnowledgeNode> {
    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");

    let branch_id = BranchId::from("main");
    let scan_result = scan_project(repo, &ScanConfig::default(), &db, branch_id.clone())
        .expect("scan must succeed");

    let conn = db.connection().clone();
    let file_ir_repo = seshat_storage::SqliteFileIRRepository::new(conn.clone());
    let all_files = seshat_storage::FileIRRepository::get_by_branch(&file_ir_repo, &branch_id)
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

    let node_repo = seshat_storage::SqliteNodeRepository::new(conn);
    seshat_storage::NodeRepository::find_conventions_by_branch(&node_repo, &branch_id)
        .expect("query conventions")
}

/// AC: forget approved decision → next scan re-emits the convention.
///
/// Flow:
///   1. Scan → assert at least one auto-detected convention surfaces.
///   2. Confirm the first convention via the TUI review-action path.
///   3. Assert the V12 `decisions` row exists with `state=Approved`.
///   4. Rescan → assert the convention is NOT re-emitted (US-008 dedup).
///   5. `forget_decision_with_database(db, &full_hash)` → assert the row is
///      gone and the returned `Decision` carries the original metadata.
///   6. Rescan → assert the convention IS re-emitted as auto-detected
///      (i.e. the dedup signal disappeared with the row).
#[test]
fn forget_decision_round_trip_re_emits_on_next_scan() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    // ── Step 1: initial scan produces auto-detected conventions ────────
    let conventions1 = scan_and_persist(repo);
    assert!(
        !conventions1.is_empty(),
        "scan must produce at least one auto-detected convention"
    );
    let target = &conventions1[0];
    let target_description = target.description.clone();
    let target_node_id = target.id.0;
    let target_hash = compute_description_hash(&target_description);

    // ── Step 2: confirm the convention via the review-action path ──────
    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn: Arc<Mutex<rusqlite::Connection>> = db.connection().clone();
    let actions = vec![ReviewAction::Confirm {
        node_id: target_node_id,
        description: target_description.clone(),
        examples: Vec::new(),
    }];
    apply_review_actions(&conn, "main", &actions).expect("apply Confirm");

    // ── Step 3: row exists with state=Approved ─────────────────────────
    let repo_handle = SqliteDecisionRepository::new(conn.clone());
    let decision_before = repo_handle
        .get_by_hash(&target_hash)
        .expect("get_by_hash")
        .expect("decision row must exist after Confirm");
    assert_eq!(decision_before.state, DecisionState::Approved);
    assert_eq!(decision_before.description, target_description);

    // ── Step 4: rescan → convention NOT re-emitted (US-008 dedup) ──────
    let conventions2 = scan_and_persist(repo);
    let descriptions2: Vec<_> = conventions2.iter().map(|c| c.description.clone()).collect();
    assert!(
        !descriptions2.contains(&target_description),
        "approved convention must not be re-emitted before forget; got {descriptions2:?}"
    );

    // ── Step 5: forget the decision ────────────────────────────────────
    // Use the FULL hash here (covers the "lookup by full description_hash"
    // half of the AC); a separate test below uses a 4-char prefix for the
    // "ambiguity-free prefix" half.
    let removed = forget_decision_with_database(&db, &target_hash).expect("forget");
    assert_eq!(removed.description_hash, target_hash);
    assert_eq!(removed.state, DecisionState::Approved);
    assert_eq!(removed.description, target_description);
    assert!(
        repo_handle.get_by_hash(&target_hash).unwrap().is_none(),
        "decisions row must be hard-deleted after forget"
    );

    // ── Step 6: rescan → convention IS re-emitted ──────────────────────
    let conventions3 = scan_and_persist(repo);
    let descriptions3: Vec<_> = conventions3.iter().map(|c| c.description.clone()).collect();
    assert!(
        descriptions3.contains(&target_description),
        "convention must re-enter the auto-detected queue after forget; \
         got {descriptions3:?}"
    );
}

/// AC: lookup-by-prefix half (≥4 chars) — same end-to-end flow but the
/// `forget` call passes a 4-character prefix instead of the full 16-char
/// hash. Pins that the prefix path is wired through the same delete logic
/// without losing the round-trip semantics.
#[test]
fn forget_decision_resolves_unambiguous_prefix() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let conventions = scan_and_persist(repo);
    assert!(!conventions.is_empty());
    let target = &conventions[0];
    let target_description = target.description.clone();
    let target_node_id = target.id.0;
    let target_hash = compute_description_hash(&target_description);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn: Arc<Mutex<rusqlite::Connection>> = db.connection().clone();
    apply_review_actions(
        &conn,
        "main",
        &[ReviewAction::Confirm {
            node_id: target_node_id,
            description: target_description.clone(),
            examples: Vec::new(),
        }],
    )
    .expect("apply Confirm");

    // 4 chars is the documented minimum prefix length (compute_description_hash
    // returns 16 hex chars, so 4-char buckets give 16-bit discrimination).
    let prefix: String = target_hash.chars().take(4).collect();
    let removed = forget_decision_with_database(&db, &prefix).expect("forget by prefix");
    assert_eq!(removed.description_hash, target_hash);

    let repo_handle = SqliteDecisionRepository::new(conn);
    assert!(repo_handle.get_by_hash(&target_hash).unwrap().is_none());
}
