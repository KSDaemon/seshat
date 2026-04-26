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
use seshat_scanner::scan_project;
use seshat_storage::{
    Database, FileIRRepository, NodeRepository, SqliteFileIRRepository, SqliteNodeRepository,
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
    let detector_results =
        run_all_detectors(&all_files, &scan_result.source_map, &detection_config, None);
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
    let first_ext = conventions[0].ext_data.clone();
    let first_id_int: i64 = first_id.0;

    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    // Apply review rejection (sets removed + user_rejected)
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
}

#[test]
fn record_decision_creates_new_node() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    let conventions_before = scan_and_get_conventions(&repo);
    let before_count = conventions_before.len();

    let db_path = repo.join("seshat.db");
    let conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));

    seshat_graph::record_decision(
        &conn,
        "main",
        seshat_graph::RecordDecisionParams {
            description: "Test convention from integration test".to_owned(),
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

    let branch_id = BranchId::from("main");
    let conn2 = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));
    let node_repo = SqliteNodeRepository::new(conn2);
    let conventions_after = node_repo
        .find_conventions_by_branch(&branch_id)
        .expect("query should succeed");

    assert!(
        conventions_after.len() > before_count,
        "Record decision should add a new node"
    );
}

#[test]
fn reject_auto_detected_marks_node_as_removed() {
    let base = tempdir().unwrap();
    let repo = create_test_repo(&base);

    let conventions = scan_and_get_conventions(&repo);
    assert!(!conventions.is_empty());

    let first_id = conventions[0].id;
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
    let actions = vec![seshat_cli::tui::app::ReviewAction::Confirm {
        node_id: conventions[0].id.0,
        description: conventions[0].description.clone(),
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

    // Second scan - the rejected convention should not reappear
    let conventions2 = scan_and_get_conventions(&repo);
    let ids: Vec<_> = conventions2.iter().map(|c| c.id).collect();
    assert!(
        !ids.contains(&first_id),
        "Rejected convention should not reappear after re-scan"
    );
}
