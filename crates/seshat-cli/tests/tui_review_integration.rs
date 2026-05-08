//! Integration tests for the TUI review wizard.
//!
//! These tests exercise the full review flow: scan → verify conventions →
//! apply review actions → verify DB state.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use std::collections::HashMap;

use seshat_core::{BranchId, DetectionConfig, KnowledgeNode};
use seshat_detectors::{aggregate_findings, run_all_detectors};
use seshat_graph::compute_description_hash;
use seshat_scanner::scan_project;
use seshat_storage::{
    Database, DecisionRepository, DecisionState, FileIRRepository, NodeRepository,
    SqliteDecisionRepository, SqliteFileIRRepository, SqliteNodeRepository,
};
use tempfile::tempdir;

fn compute_snapshot_hash(ext_data: &Option<String>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::default();
    ext_data.as_deref().unwrap_or("").hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn create_test_repo(base: &tempfile::TempDir) -> PathBuf {
    let repo = base.path().join("test-repo");
    fs::create_dir_all(&repo).unwrap();

    git_init(&repo);
    fs::write(repo.join("README.md"), "# Test Repo").unwrap();
    git_add_commit(&repo, "initial commit");

    let src = repo.join("src");
    fs::create_dir_all(&src).unwrap();

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
    .unwrap();

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
    .unwrap();

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
    .unwrap();

    repo
}

fn git_init(path: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();

    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
}

fn git_add_commit(path: &std::path::Path, message: &str) {
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
}

fn scan_and_get_conventions(repo: &std::path::Path) -> Vec<KnowledgeNode> {
    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).unwrap();

    let branch_id = BranchId::from("main");
    let scan_result = scan_project(
        repo,
        &seshat_core::ScanConfig::default(),
        &db,
        branch_id.clone(),
    )
    .expect("scan should succeed");

    // Run convention detection pipeline
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let all_files = file_ir_repo
        .get_by_branch(&branch_id)
        .expect("load files for detection");

    let detection_config = DetectionConfig::default();
    let project_context = seshat_detectors::ProjectContext::from_files(&all_files);
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

    let node_repo = SqliteNodeRepository::new(conn);
    node_repo
        .find_conventions_by_branch(&branch_id)
        .expect("query should succeed")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn scan_produces_conventions_for_review() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    let conventions = scan_and_get_conventions(&repo);
    assert!(
        !conventions.is_empty(),
        "Expected conventions to be detected"
    );
}

#[test]
fn query_conventions_excludes_user_rejected() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    let conventions = scan_and_get_conventions(&repo);
    assert!(!conventions.is_empty());

    let first_id = conventions[0].id;
    let first_description = conventions[0].description.clone();
    let first_ext = conventions[0].ext_data.clone();
    let first_id_int: i64 = first_id.0;

    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    // Apply review rejection — writes to `decisions` and soft-deletes the node.
    let snapshot_hash = compute_snapshot_hash(&first_ext.as_ref().map(|v| v.to_string()));
    let actions = vec![seshat_cli::tui::app::ReviewAction::Reject {
        node_id: first_id_int,
        snapshot_hash,
    }];
    seshat_cli::tui::app::apply_review_actions(&conn, "main", &actions).unwrap();

    let branch_id = BranchId::from("main");
    let conn2 = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));
    let node_repo = SqliteNodeRepository::new(conn2);
    let after_reject = node_repo
        .find_conventions_by_branch(&branch_id)
        .expect("query should succeed");

    let rejected_ids: Vec<_> = after_reject.iter().map(|c| c.id).collect();
    assert!(
        !rejected_ids.contains(&first_id),
        "Rejected convention should be excluded from review"
    );

    // The new source-of-truth: a `decisions` row with state='rejected'
    // keyed by description_hash.
    let hash = compute_description_hash(&first_description);
    let decision_repo = SqliteDecisionRepository::new(conn.clone());
    let decision = decision_repo
        .get_by_hash(&hash)
        .unwrap()
        .expect("decisions row should exist for rejected convention");
    assert_eq!(decision.state, DecisionState::Rejected);
    assert_eq!(decision.decided_on_branch, BranchId::from("main"));

    // End-to-end via the US-006 LEFT-JOIN review query: even if the
    // soft-delete (`removed=1`) regressed, the decisions row alone must
    // suppress the convention from the review queue.
    let (review_items, _) =
        seshat_cli::tui::app::query_conventions_for_review(&conn, "main").unwrap();
    let descriptions: Vec<_> = review_items
        .iter()
        .map(|it| it.description.as_str())
        .collect();
    assert!(
        !descriptions.contains(&first_description.as_str()),
        "Rejected convention must not surface via query_conventions_for_review (LEFT JOIN)"
    );
}

#[test]
fn record_decision_writes_to_decisions_table_not_nodes() {
    // Post-US-004: record_decision writes to the V12 `decisions` table with
    // state='recorded', NOT to the `nodes` table. This test pins the new
    // contract and the negative invariant (no user-source nodes leak).
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    // Initial scan populates auto-detected nodes; baseline counts.
    let conventions_before = scan_and_get_conventions(&repo);
    let auto_nodes_before = conventions_before.len();

    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    let description = "Test convention from integration test";
    let result = seshat_graph::record_decision(
        &conn,
        "main",
        seshat_graph::RecordDecisionParams {
            description: description.to_owned(),
            nature: "convention".to_owned(),
            weight: "strong".to_owned(),
            category: Some("testing".to_owned()),
            examples: vec![seshat_graph::decisions::ExampleInput {
                file: "src/test.rs".to_owned(),
                line: 1,
                end_line: 3,
                snippet: "fn test() {}".to_owned(),
            }],
            reason: Some("Integration test".to_owned()),
        },
    )
    .unwrap();

    // Positive invariant: a row exists in the V12 `decisions` table with the
    // expected hash, state='recorded', and the right description.
    let expected_hash = compute_description_hash(description);
    assert_eq!(result.description_hash, expected_hash);

    let conn2 = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));
    let decision_repo = SqliteDecisionRepository::new(conn2.clone());
    let decision = decision_repo
        .get_by_hash(&expected_hash)
        .unwrap()
        .expect("decisions row should exist post record_decision");
    assert_eq!(decision.state, DecisionState::Recorded);
    assert_eq!(decision.description, description);
    assert_eq!(decision.decided_on_branch, BranchId::from("main"));

    // Negative invariant: the legacy user-source node path is gone — no new
    // node was inserted in `nodes` for this decision.
    let node_repo = SqliteNodeRepository::new(conn2);
    let nodes_after = node_repo
        .find_conventions_by_branch(&BranchId::from("main"))
        .expect("query should succeed");
    assert_eq!(
        nodes_after.len(),
        auto_nodes_before,
        "record_decision must not create user-source nodes — only the V12 decisions row"
    );
    let user_source_count = nodes_after
        .iter()
        .filter(|n| {
            n.ext_data
                .as_ref()
                .and_then(|e| e.get("source"))
                .and_then(|v| v.as_str())
                == Some("user")
        })
        .count();
    assert_eq!(
        user_source_count, 0,
        "no nodes with source='user' must be created by record_decision"
    );
}

#[test]
fn reject_auto_detected_marks_node_as_removed() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    let conventions = scan_and_get_conventions(&repo);
    assert!(!conventions.is_empty());

    let first_id = conventions[0].id;
    let first_description = conventions[0].description.clone();
    let first_ext = conventions[0].ext_data.clone();
    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    // Auto-detected conventions must be rejected via review actions, not remove_decision
    let snapshot_hash = compute_snapshot_hash(&first_ext.as_ref().map(|v| v.to_string()));
    let actions = vec![seshat_cli::tui::app::ReviewAction::Reject {
        node_id: first_id.0,
        snapshot_hash,
    }];
    seshat_cli::tui::app::apply_review_actions(&conn, "main", &actions).unwrap();

    let branch_id = BranchId::from("main");
    let conn2 = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));
    let node_repo = SqliteNodeRepository::new(conn2);
    let after_remove = node_repo
        .find_conventions_by_branch(&branch_id)
        .expect("query should succeed");

    let ids: Vec<_> = after_remove.iter().map(|c| c.id).collect();
    assert!(
        !ids.contains(&first_id),
        "Removed convention should not appear in review"
    );

    // The rejection lives in `decisions` now, not as a `user_rejected=1`
    // flag on a node.
    let hash = compute_description_hash(&first_description);
    let decision_repo = SqliteDecisionRepository::new(conn.clone());
    let decision = decision_repo
        .get_by_hash(&hash)
        .unwrap()
        .expect("decisions row should exist for rejected convention");
    assert_eq!(decision.state, DecisionState::Rejected);
}

#[test]
fn fts_index_updated_after_batch_actions() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    let conventions = scan_and_get_conventions(&repo);
    assert!(!conventions.is_empty());

    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    // Apply some review actions
    let confirm_description = conventions[0].description.clone();
    let actions = vec![seshat_cli::tui::app::ReviewAction::Confirm {
        node_id: conventions[0].id.0,
        description: confirm_description.clone(),
        examples: Vec::new(),
    }];

    seshat_cli::tui::app::apply_review_actions(&conn, "main", &actions).unwrap();
    seshat_graph::rebuild_fts_index(&conn).unwrap();

    // Verify FTS5 table is accessible
    let guard = conn.lock().unwrap();
    let result: Result<i64, _> = guard.query_row(
        "SELECT COUNT(*) FROM nodes WHERE nature IN ('convention', 'observation')",
        [],
        |row| row.get(0),
    );
    assert!(result.is_ok(), "FTS index should not be corrupted");
    drop(guard);

    // The confirmed convention now lives in `decisions`, not as a node.
    let hash = compute_description_hash(&confirm_description);
    let decision_repo = SqliteDecisionRepository::new(conn.clone());
    let decision = decision_repo
        .get_by_hash(&hash)
        .unwrap()
        .expect("approved decision row should exist after batch confirm");
    assert_eq!(decision.state, DecisionState::Approved);
}

/// T16 / US-005 AC: an end-to-end test for the `Partial` review action.
/// The pre-existing `review_action_types_are_correct` is only a constructor
/// pattern-match smoke test — it never touches the DB. US-005 explicitly
/// mandates that `tui_review_integration.rs` lock the storage contract for
/// confirm / reject / partial. Confirm and reject already had end-to-end
/// tests above; this one covers the partial leg.
///
/// Asserts:
///   * `apply_review_actions` accepts `ReviewAction::Partial`.
///   * After it runs, a `decisions` row exists at
///     `compute_description_hash(description)` with `state='partial'` and
///     `decided_on_branch='main'`.
///   * The auto-detected node for that description disappears from
///     `find_conventions_by_branch` (consistent with confirm/reject).
///   * No legacy `preference` node is created — preference rows now live
///     in `decisions`, not in `nodes` (PRD US-005 AC bullet 3).
#[test]
fn partial_auto_detected_writes_decisions_row_with_state_partial() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    let conventions = scan_and_get_conventions(&repo);
    assert!(!conventions.is_empty());

    let target = &conventions[0];
    let target_id = target.id.0;
    let target_description = target.description.clone();

    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    let actions = vec![seshat_cli::tui::app::ReviewAction::Partial {
        node_id: target_id,
        description: target_description.clone(),
        original_node_id: target_id,
    }];
    seshat_cli::tui::app::apply_review_actions(&conn, "main", &actions)
        .expect("apply Partial review action");

    // The decision row exists and carries state='partial'.
    let hash = compute_description_hash(&target_description);
    let decision_repo = SqliteDecisionRepository::new(conn.clone());
    let decision = decision_repo
        .get_by_hash(&hash)
        .unwrap()
        .expect("decisions row must exist after Partial review action");
    assert_eq!(
        decision.state,
        DecisionState::Partial,
        "Partial review action must persist state='partial' in the decisions table"
    );
    assert_eq!(decision.description, target_description);
    assert_eq!(decision.decided_on_branch, BranchId::from("main"));

    // No legacy `preference` node is created — preference rows now live in
    // `decisions`, not in `nodes` (US-005 bullet 3).
    let conn2 = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));
    let node_repo = SqliteNodeRepository::new(conn2);
    let nodes_after = node_repo
        .find_conventions_by_branch(&BranchId::from("main"))
        .expect("query nodes after partial");
    let preference_count = nodes_after
        .iter()
        .filter(|n| n.nature == seshat_core::KnowledgeNature::Preference)
        .count();
    assert_eq!(
        preference_count, 0,
        "Partial review action must not create a Preference node — \
         preferences live in `decisions` with state='partial' now"
    );
}

#[test]
fn review_action_types_are_correct() {
    use seshat_cli::tui::app::ReviewAction;

    let confirm = ReviewAction::Confirm {
        node_id: 42,
        description: "test".to_owned(),
        examples: Vec::new(),
    };
    assert!(matches!(confirm, ReviewAction::Confirm { node_id: 42, .. }));

    let reject = ReviewAction::Reject {
        node_id: 42,
        snapshot_hash: 123,
    };
    assert!(matches!(reject, ReviewAction::Reject { node_id: 42, .. }));

    let partial = ReviewAction::Partial {
        node_id: 42,
        description: "test".to_owned(),
        original_node_id: 42,
    };
    assert!(matches!(partial, ReviewAction::Partial { node_id: 42, .. }));

    let skip = ReviewAction::Skip { node_id: 42 };
    assert!(matches!(skip, ReviewAction::Skip { node_id: 42 }));
}

#[test]
fn app_state_machine_navigates_correctly() {
    use seshat_cli::tui::app::{App, ConventionItem};

    let conventions: Vec<ConventionItem> = (1..=5)
        .map(|i| ConventionItem {
            node_id: i,
            description: format!("Convention {i}"),
            nature: "convention".to_owned(),
            weight: "strong".to_owned(),
            confidence_pct: 90,
            adoption_count: i as u32,
            total_count: 5,
            adoption_rate_pct: (i as f64 / 5.0 * 100.0) as u32,
            trend: "stable".to_owned(),
            source: "auto_detected".to_owned(),
            examples: Vec::new(),
            snapshot_hash: 0,
            description_hash: None,
            example_index: 0,
        })
        .collect();

    let mut app = App::new(conventions);

    assert_eq!(app.current_index, 0);
    assert!(app.current().unwrap().node_id == 1);
    assert!(!app.review_complete);

    app.next();
    assert_eq!(app.current_index, 1);
    assert!(app.current().unwrap().node_id == 2);

    app.previous();
    assert_eq!(app.current_index, 0);

    for _ in 0..4 {
        app.next();
    }
    assert_eq!(app.current_index, 4);
    assert!(app.current().unwrap().node_id == 5);
    assert!(app.review_complete);

    app.next();
    assert_eq!(app.current_index, 4);

    for _ in 0..5 {
        app.previous();
    }
    assert_eq!(app.current_index, 0);
}

#[test]
fn app_accumulates_actions_during_review() {
    use seshat_cli::tui::app::{App, ConventionItem, ReviewAction};

    let conventions: Vec<ConventionItem> = (1..=3)
        .map(|i| ConventionItem {
            node_id: i,
            description: format!("Convention {i}"),
            nature: "convention".to_owned(),
            weight: "strong".to_owned(),
            confidence_pct: 90,
            adoption_count: 1,
            total_count: 1,
            adoption_rate_pct: 100,
            trend: "stable".to_owned(),
            source: "auto_detected".to_owned(),
            examples: Vec::new(),
            snapshot_hash: 0,
            description_hash: None,
            example_index: 0,
        })
        .collect();

    let mut app = App::new(conventions);

    for _ in 0..3 {
        if let Some(conv) = app.current() {
            app.results.push(ReviewAction::Confirm {
                node_id: conv.node_id,
                description: conv.description.clone(),
                examples: conv.examples.clone(),
            });
            app.next();
        }
    }

    assert_eq!(app.results.len(), 3);
    assert!(matches!(
        &app.results[0],
        ReviewAction::Confirm { node_id: 1, .. }
    ));
    assert!(matches!(
        &app.results[1],
        ReviewAction::Confirm { node_id: 2, .. }
    ));
    assert!(matches!(
        &app.results[2],
        ReviewAction::Confirm { node_id: 3, .. }
    ));
}

#[test]
fn persisted_rejection_prevents_rerecognition() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    // First scan
    let conventions1 = scan_and_get_conventions(&repo);
    assert!(!conventions1.is_empty());

    let first_id = conventions1[0].id;
    let first_description = conventions1[0].description.clone();
    let first_ext = conventions1[0].ext_data.clone();

    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    // Reject the auto-detected convention via review action
    let snapshot_hash = compute_snapshot_hash(&first_ext.as_ref().map(|v| v.to_string()));
    let actions = vec![seshat_cli::tui::app::ReviewAction::Reject {
        node_id: first_id.0,
        snapshot_hash,
    }];
    seshat_cli::tui::app::apply_review_actions(&conn, "main", &actions).unwrap();

    // The rejection persists in `decisions` and survives re-scans because
    // `persist_conventions` now consults the decisions table on every insert.
    let hash = compute_description_hash(&first_description);
    let decision_repo = SqliteDecisionRepository::new(conn.clone());
    let decision = decision_repo
        .get_by_hash(&hash)
        .unwrap()
        .expect("rejected decision row should be present before rescan");
    assert_eq!(decision.state, DecisionState::Rejected);

    // Second scan - the rejected convention should not reappear in either
    // the auto-detected nodes or the review queue.
    let conventions2 = scan_and_get_conventions(&repo);
    let ids: Vec<_> = conventions2.iter().map(|c| c.id).collect();
    assert!(
        !ids.contains(&first_id),
        "Rejected convention should not reappear after re-scan"
    );
    let descriptions: Vec<_> = conventions2.iter().map(|c| c.description.clone()).collect();
    assert!(
        !descriptions.contains(&first_description),
        "Rejected convention description should not be re-emitted post-rescan"
    );
}
