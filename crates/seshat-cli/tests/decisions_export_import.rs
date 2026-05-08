//! Integration test for US-015: `seshat decisions export` / `import`.
//!
//! Locks the AC #3 round-trip behaviour end-to-end: export a populated
//! decisions table to a JSON file, wipe the table, re-import from the file,
//! assert the result is byte-for-byte identical to what was exported.
//!
//! `run_export` / `run_import` perform their own project-resolve + path
//! discovery, so the test drives the public seams [`export_decisions_to_string`]
//! and [`import_decisions_from_str`] (the same string-level entry points the
//! file-based CLI commands call into).

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use seshat_cli::decisions::{export_decisions_to_string, import_decisions_from_str};
use seshat_cli::tui::app::{ReviewAction, apply_review_actions};
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, ScanConfig};
use seshat_detectors::{ProjectContext, aggregate_findings, run_all_detectors};
use seshat_graph::compute_description_hash;
use seshat_scanner::scan_project;
use seshat_storage::{
    Database, Decision, DecisionRepository, DecisionState, SqliteDecisionRepository,
};
use tempfile::tempdir;

/// Small Rust source tree large enough to surface multiple auto-detected
/// conventions for the round-trip test.
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

/// Same scan + detect + persist helper used by `decisions_forget.rs` and
/// `git_unavailable_fallback.rs`. Returns the auto-detected conventions
/// visible after persist.
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

/// Sort decisions by hash so two row-sets can be compared deterministically
/// regardless of the source DB's `ORDER BY decided_at DESC` shape.
fn sort_by_hash(mut rows: Vec<Decision>) -> Vec<Decision> {
    rows.sort_by(|a, b| a.description_hash.cmp(&b.description_hash));
    rows
}

/// AC #3: Round-trip export → wipe → import → table identical.
///
/// Flow:
///   1. Scan the fixture, confirm two real conventions to populate the
///      decisions table with realistic state=Approved rows.
///   2. Export the table to a JSON file on disk.
///   3. Wipe the decisions table in-place (DELETE every row).
///   4. Re-import from the file.
///   5. Assert the re-imported set is byte-for-byte identical to step 1's
///      snapshot — same description_hash, same description, same state,
///      same examples, same decided_on_branch, same decided_at, same
///      updated_at.
#[test]
fn export_then_wipe_then_import_yields_identical_table() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let conventions = scan_and_persist(repo);
    assert!(
        conventions.len() >= 2,
        "scan must produce at least two conventions for a meaningful round-trip; got {}",
        conventions.len()
    );

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn: Arc<Mutex<rusqlite::Connection>> = db.connection().clone();

    // ── Confirm two conventions so the decisions table has Approved rows. ──
    let actions: Vec<ReviewAction> = conventions
        .iter()
        .take(2)
        .map(|c| ReviewAction::Confirm {
            node_id: c.id.0,
            description: c.description.clone(),
            examples: Vec::new(),
        })
        .collect();
    apply_review_actions(&conn, "main", &actions).expect("apply Confirm");

    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let before = sort_by_hash(dec_repo.list().unwrap());
    assert_eq!(
        before.len(),
        2,
        "two Confirm actions should yield two decision rows"
    );
    for d in &before {
        assert_eq!(d.state, DecisionState::Approved);
    }

    // ── Step 2: export to a file on disk. ──────────────────────────────
    let export_path = workdir.path().join("decisions.json");
    let json = export_decisions_to_string(&db).expect("export");
    fs::write(&export_path, json.as_bytes()).expect("write export file");
    let file_contents = fs::read_to_string(&export_path).expect("read export back");
    assert_eq!(
        file_contents, json,
        "file write must be byte-for-byte the export string"
    );
    assert!(file_contents.starts_with('['));
    assert!(file_contents.trim_end().ends_with(']'));

    // ── Step 3: wipe in place. ─────────────────────────────────────────
    for d in &before {
        dec_repo.delete(&d.description_hash).unwrap();
    }
    assert!(dec_repo.list().unwrap().is_empty());

    // Sanity: with the decisions gone, those conventions re-enter the auto
    // queue (US-008 dedup signal disappeared with the rows).
    let conventions_after_wipe = scan_and_persist(repo);
    let descs: Vec<&str> = conventions_after_wipe
        .iter()
        .map(|c| c.description.as_str())
        .collect();
    for d in &before {
        assert!(
            descs.contains(&d.description.as_str()),
            "wiped convention must re-emit; got {descs:?}"
        );
    }

    // ── Step 4: re-import from the file. ───────────────────────────────
    let import_json = fs::read_to_string(&export_path).expect("read export");
    let summary = import_decisions_from_str(&db, &import_json, false).expect("import");
    assert_eq!(summary.total, before.len());
    assert_eq!(summary.inserted, before.len());
    assert_eq!(summary.updated, 0);
    assert_eq!(summary.skipped, 0);

    // ── Step 5: identical-table assertion. ─────────────────────────────
    let after = sort_by_hash(dec_repo.list().unwrap());
    assert_eq!(after.len(), before.len());
    for (b, a) in before.iter().zip(after.iter()) {
        // Decision derives PartialEq across every field — the strongest
        // "table identical" assertion available.
        assert_eq!(b, a, "round-trip mismatch on hash {}", b.description_hash);
    }

    // The freshly-imported approved convention must NOT re-enter the queue
    // on the next scan (US-008 dedup signal is back).
    let conventions_post_import = scan_and_persist(repo);
    let descs_post: Vec<&str> = conventions_post_import
        .iter()
        .map(|c| c.description.as_str())
        .collect();
    for d in &before {
        assert!(
            !descs_post.contains(&d.description.as_str()),
            "imported approved decision must dedup; got {descs_post:?}"
        );
    }
}

/// Strict mode: AC #2 — `--strict` aborts the import when an incoming hash
/// already exists in the DB. The test exports from a populated source DB,
/// then attempts to import the same payload back into the SAME DB (which
/// already has those hashes) and asserts the import fails before any writes.
#[test]
fn import_strict_aborts_when_target_already_has_hashes() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let conventions = scan_and_persist(repo);
    assert!(!conventions.is_empty());

    let db = Database::open(repo.join("seshat.db")).expect("reopen DB");
    let conn = db.connection().clone();

    // Confirm one convention so we have a row to conflict on.
    let target = &conventions[0];
    apply_review_actions(
        &conn,
        "main",
        &[ReviewAction::Confirm {
            node_id: target.id.0,
            description: target.description.clone(),
            examples: Vec::new(),
        }],
    )
    .expect("Confirm");

    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let before = dec_repo.list().unwrap();
    assert_eq!(before.len(), 1);
    let target_hash = compute_description_hash(&target.description);

    // Export, then try to import back into the SAME DB with --strict.
    let json = export_decisions_to_string(&db).expect("export");
    let err =
        import_decisions_from_str(&db, &json, true).expect_err("strict must reject same-hash");
    let msg = err.to_string();
    assert!(msg.contains("strict mode"), "got: {msg}");
    assert!(
        msg.contains(&target_hash),
        "must list conflicting hash: {msg}"
    );

    // Row unchanged; no partial writes.
    let after = dec_repo.get_by_hash(&target_hash).unwrap().unwrap();
    assert_eq!(after.state, DecisionState::Approved);
    assert_eq!(after.description, target.description);
}

// ── T12: non-strict conflict resolution (latest-decided_at-wins) ─────────

/// US-015 / FR-24: in non-strict mode, conflicts resolve by `decided_at` —
/// the row with the higher (later) timestamp wins. This tests the
/// `incoming-newer-than-existing → row replaced` half of the rule.
///
/// The unit tests in `decisions.rs::tests` lock the resolution logic
/// against synthetic JSON; this test proves the same behaviour holds
/// through the public seam after a real scan + apply_review pipeline.
#[test]
fn import_non_strict_replaces_existing_when_incoming_is_newer() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let conventions = scan_and_persist(repo);
    assert!(!conventions.is_empty());
    let target = &conventions[0];
    let target_description = target.description.clone();
    let target_hash = compute_description_hash(&target_description);

    let db = Database::open(repo.join("seshat.db")).expect("reopen DB");
    let conn = db.connection().clone();

    // Confirm the convention so a row exists at decided_at = T.
    apply_review_actions(
        &conn,
        "main",
        &[ReviewAction::Confirm {
            node_id: target.id.0,
            description: target_description.clone(),
            examples: Vec::new(),
        }],
    )
    .expect("Confirm");

    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let existing = dec_repo
        .get_by_hash(&target_hash)
        .unwrap()
        .expect("row exists after Confirm");
    let baseline_decided_at = existing.decided_at;

    // Build an import payload with the SAME hash but a strictly LATER
    // decided_at and a different category, so we can prove the
    // replacement happened.
    let newer_decided_at = baseline_decided_at + 1_000;
    let import_json = serde_json::json!([{
        "description_hash": target_hash,
        "description": target_description,
        "state": "approved",
        "nature": "convention",
        "weight": "strong",
        "category": "imported-newer",
        "reason": null,
        "examples": [],
        "decided_on_branch": "imported-branch",
        "decided_at": newer_decided_at,
        "updated_at": newer_decided_at,
    }])
    .to_string();

    let summary = import_decisions_from_str(&db, &import_json, false).expect("non-strict import");

    assert_eq!(summary.total, 1);
    assert_eq!(summary.inserted, 0);
    assert_eq!(
        summary.updated, 1,
        "newer incoming row must update existing (got {summary:?})"
    );
    assert_eq!(summary.skipped, 0);

    // The row in the DB now reflects the imported (newer) values.
    let after = dec_repo
        .get_by_hash(&target_hash)
        .unwrap()
        .expect("row remains");
    assert_eq!(after.decided_at, newer_decided_at);
    assert_eq!(after.category.as_deref(), Some("imported-newer"));
    assert_eq!(after.decided_on_branch, BranchId::from("imported-branch"));
}

/// US-015 / FR-24: incoming-older-than-existing → existing kept (skipped).
///
/// The reverse of the above: a re-import of a stale snapshot must not
/// regress newer state in the DB. The user should be able to import an
/// older export without losing newer decisions made since.
#[test]
fn import_non_strict_skips_when_incoming_is_older() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let conventions = scan_and_persist(repo);
    assert!(!conventions.is_empty());
    let target = &conventions[0];
    let target_description = target.description.clone();
    let target_hash = compute_description_hash(&target_description);

    let db = Database::open(repo.join("seshat.db")).expect("reopen DB");
    let conn = db.connection().clone();

    apply_review_actions(
        &conn,
        "main",
        &[ReviewAction::Confirm {
            node_id: target.id.0,
            description: target_description.clone(),
            examples: Vec::new(),
        }],
    )
    .expect("Confirm");

    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let existing = dec_repo
        .get_by_hash(&target_hash)
        .unwrap()
        .expect("row exists after Confirm");
    let baseline_decided_at = existing.decided_at;
    let baseline_category = existing.category.clone();

    // Import payload with same hash but STRICTLY EARLIER decided_at.
    let older_decided_at = baseline_decided_at - 1_000;
    let import_json = serde_json::json!([{
        "description_hash": target_hash,
        "description": target_description,
        "state": "rejected",
        "nature": "convention",
        "weight": "rule",
        "category": "imported-older-should-not-win",
        "reason": "stale snapshot — must not overwrite",
        "examples": [],
        "decided_on_branch": "imported-branch",
        "decided_at": older_decided_at,
        "updated_at": older_decided_at,
    }])
    .to_string();

    let summary = import_decisions_from_str(&db, &import_json, false).expect("non-strict import");

    assert_eq!(summary.total, 1);
    assert_eq!(summary.inserted, 0);
    assert_eq!(summary.updated, 0);
    assert_eq!(
        summary.skipped, 1,
        "older incoming row must be skipped (got {summary:?})"
    );

    // The row in the DB is UNCHANGED — still the original Approved row.
    let after = dec_repo
        .get_by_hash(&target_hash)
        .unwrap()
        .expect("row remains");
    assert_eq!(
        after.state,
        DecisionState::Approved,
        "stale import must not flip state from Approved to Rejected"
    );
    assert_eq!(after.decided_at, baseline_decided_at);
    assert_eq!(after.category, baseline_category);
    assert_eq!(after.decided_on_branch, BranchId::from("main"));
}

/// Mixed payload: insert one new row + update one row (incoming newer) +
/// skip one row (incoming older), verifying the summary counts are exact
/// for a non-trivial multi-row import.
#[test]
fn import_non_strict_mixed_payload_counts_each_class_exactly() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let conventions = scan_and_persist(repo);
    assert!(conventions.len() >= 2, "need ≥2 conventions for this test");

    let db = Database::open(repo.join("seshat.db")).expect("reopen DB");
    let conn = db.connection().clone();

    let conv_a = &conventions[0];
    let conv_b = &conventions[1];
    let hash_a = compute_description_hash(&conv_a.description);
    let hash_b = compute_description_hash(&conv_b.description);

    // Confirm both → two existing rows in the DB.
    apply_review_actions(
        &conn,
        "main",
        &[
            ReviewAction::Confirm {
                node_id: conv_a.id.0,
                description: conv_a.description.clone(),
                examples: Vec::new(),
            },
            ReviewAction::Confirm {
                node_id: conv_b.id.0,
                description: conv_b.description.clone(),
                examples: Vec::new(),
            },
        ],
    )
    .expect("Confirm both");

    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let row_a = dec_repo.get_by_hash(&hash_a).unwrap().unwrap();
    let row_b = dec_repo.get_by_hash(&hash_b).unwrap().unwrap();

    // Import payload:
    //   - hash_a: NEWER decided_at → update
    //   - hash_b: OLDER decided_at → skip
    //   - third row with a fresh hash → insert
    let fresh_hash = "deadbeefcafebabe";
    let import_json = serde_json::json!([
        {
            "description_hash": hash_a,
            "description": conv_a.description,
            "state": "approved",
            "nature": "convention",
            "weight": "strong",
            "category": "newer-wins",
            "reason": null,
            "examples": [],
            "decided_on_branch": "main",
            "decided_at": row_a.decided_at + 5_000,
            "updated_at": row_a.decided_at + 5_000,
        },
        {
            "description_hash": hash_b,
            "description": conv_b.description,
            "state": "approved",
            "nature": "convention",
            "weight": "strong",
            "category": "older-loses",
            "reason": null,
            "examples": [],
            "decided_on_branch": "main",
            "decided_at": row_b.decided_at - 5_000,
            "updated_at": row_b.decided_at - 5_000,
        },
        {
            "description_hash": fresh_hash,
            "description": "fresh decision from import",
            "state": "recorded",
            "nature": "decision",
            "weight": "strong",
            "category": null,
            "reason": null,
            "examples": [],
            "decided_on_branch": "main",
            "decided_at": 1_700_000_000,
            "updated_at": 1_700_000_000,
        }
    ])
    .to_string();

    let summary = import_decisions_from_str(&db, &import_json, false).expect("non-strict import");
    assert_eq!(summary.total, 3);
    assert_eq!(summary.inserted, 1, "fresh hash must insert");
    assert_eq!(summary.updated, 1, "newer hash_a must update");
    assert_eq!(summary.skipped, 1, "older hash_b must be skipped");

    // Verify on-disk state matches the count breakdown.
    let after_a = dec_repo.get_by_hash(&hash_a).unwrap().unwrap();
    assert_eq!(after_a.category.as_deref(), Some("newer-wins"));

    let after_b = dec_repo.get_by_hash(&hash_b).unwrap().unwrap();
    // hash_b's category was None before, must remain None after the
    // skipped import (NOT flipped to "older-loses").
    assert_eq!(after_b.category, row_b.category);

    let fresh = dec_repo
        .get_by_hash(fresh_hash)
        .unwrap()
        .expect("fresh row inserted");
    assert_eq!(fresh.description, "fresh decision from import");
    assert_eq!(fresh.state, DecisionState::Recorded);
}
