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

use seshat_cli::CliError;
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

// ── T11: error-path coverage required by US-014 ──────────────────────────

/// US-014 AC: "Error: hash not found, ambiguous prefix, multiple hashes match."
/// All three error paths must surface as a typed CLI error so the unattended
/// branch of `run_forget` can map them to a non-zero exit code.
///
/// This test covers the **too-short prefix** case. The minimum is 4 chars
/// (described by `MIN_FORGET_PREFIX_LEN` in the production module). Anything
/// shorter must error before the DB is consulted — there is no point doing
/// a full-table scan to filter by a 3-char prefix that would match almost
/// anything.
#[test]
fn forget_decision_rejects_too_short_prefix() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");

    // Empty string and 1-3 char prefixes are all below the floor.
    for short in ["", "a", "ab", "abc"] {
        let result = forget_decision_with_database(&db, short);
        match result {
            Err(CliError::InvalidArgument(msg)) => {
                assert!(
                    msg.contains("too short") || msg.contains("at least"),
                    "error for '{short}' must mention the length floor; got: {msg}"
                );
            }
            other => panic!(
                "forget('{short}') must return InvalidArgument for too-short prefix, \
                 got: {other:?}"
            ),
        }
    }
}

/// US-014 AC: hash-not-found error path.
///
/// Empty decisions table + a well-formed (≥4 char) prefix that resolves to
/// nothing must surface a typed error, NOT a silent success or a "deleted 0
/// rows" no-op. Caller scripts checking the exit code rely on this contract.
#[test]
fn forget_decision_errors_when_hash_not_found() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");

    // No decisions inserted; any well-formed prefix yields zero matches.
    // "deadbeef" is non-empty, ≥4 chars, and (with overwhelming probability)
    // matches nothing.
    let result = forget_decision_with_database(&db, "deadbeef");
    match result {
        Err(CliError::CommandFailed { reason, .. }) => {
            assert!(
                reason.contains("no decision matches") || reason.contains("not found"),
                "error must mention the absence of a match; got: {reason}"
            );
            assert!(
                reason.contains("deadbeef"),
                "error must echo the offending hash so the user can self-debug; \
                 got: {reason}"
            );
        }
        other => panic!(
            "forget on empty decisions table must return CommandFailed; \
             got: {other:?}"
        ),
    }
}

/// US-014 AC: ambiguous-prefix error path.
///
/// When a (≥4 char) prefix matches more than one decision, the CLI must
/// refuse and list the matched hashes so the user can lengthen the prefix
/// and disambiguate. Silently picking one of the matches would risk
/// deleting the wrong decision.
///
/// We seed two decisions with description_hashes that share the first
/// 4 hex characters. The hash function is content-derived, so we craft
/// a single-char prefix collision via the fact that 16 hex chars give
/// 65536 possible 4-char prefixes — collisions exist. To avoid relying
/// on that statistic, this test inserts the two rows directly via the
/// repository with hand-picked hashes that share a 4-char prefix.
#[test]
fn forget_decision_errors_on_ambiguous_prefix() {
    use seshat_storage::{Decision, DecisionNature, DecisionWeight, ExampleEvidence};

    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let conn = db.connection().clone();
    let dec_repo = SqliteDecisionRepository::new(conn);

    // Two decisions sharing a 4-char prefix — chosen explicitly so the
    // test is independent of the hash function's distribution.
    let now = chrono::Utc::now().timestamp();
    let mk = |hash: &str, desc: &str| Decision {
        description_hash: hash.to_owned(),
        description: desc.to_owned(),
        state: DecisionState::Recorded,
        nature: DecisionNature::Decision,
        weight: DecisionWeight::Strong,
        category: None,
        reason: None,
        examples: Vec::<ExampleEvidence>::new(),
        decided_on_branch: BranchId::from("main"),
        decided_at: now,
        updated_at: now,
    };
    dec_repo
        .upsert(&mk("abcd1111aaaaaaaa", "first decision"))
        .expect("seed first decision");
    dec_repo
        .upsert(&mk("abcd2222bbbbbbbb", "second decision"))
        .expect("seed second decision");

    let result = forget_decision_with_database(&db, "abcd");
    match result {
        Err(CliError::CommandFailed { reason, .. }) => {
            assert!(
                reason.contains("ambiguous"),
                "error must call out the ambiguity by name; got: {reason}"
            );
            // The error must list both candidates so the user can
            // lengthen the prefix to disambiguate.
            assert!(
                reason.contains("abcd1111") && reason.contains("abcd2222"),
                "error must list the matched hashes for self-disambiguation; \
                 got: {reason}"
            );
        }
        other => panic!("ambiguous prefix must return CommandFailed; got: {other:?}"),
    }

    // Both rows must remain — nothing was deleted.
    assert!(
        dec_repo.get_by_hash("abcd1111aaaaaaaa").unwrap().is_some(),
        "first row must survive a refused ambiguous forget"
    );
    assert!(
        dec_repo.get_by_hash("abcd2222bbbbbbbb").unwrap().is_some(),
        "second row must survive a refused ambiguous forget"
    );
}

// ── T13: forget round-trip across all four states ───────────────────────

/// T13: the existing forget round-trip uses Approved. Verify that
/// forget works identically for Rejected, Partial, and Recorded —
/// the lookup is by `description_hash`, so state must not affect it.
#[test]
fn forget_decision_works_for_rejected_partial_recorded_states() {
    use seshat_storage::{
        Decision, DecisionNature, DecisionRepository, DecisionState, DecisionWeight,
        ExampleEvidence, SqliteDecisionRepository,
    };

    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    write_rust_sources(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let conn = db.connection().clone();
    let dec_repo = SqliteDecisionRepository::new(conn);

    let now = chrono::Utc::now().timestamp();
    for (hash, state) in [
        ("aaaa1111aaaaaaaa", DecisionState::Rejected),
        ("bbbb2222bbbbbbbb", DecisionState::Partial),
        ("cccc3333cccccccc", DecisionState::Recorded),
    ] {
        dec_repo
            .upsert(&Decision {
                description_hash: hash.to_owned(),
                description: format!("desc for {hash}"),
                state,
                nature: DecisionNature::Decision,
                weight: DecisionWeight::Strong,
                category: None,
                reason: None,
                examples: Vec::<ExampleEvidence>::new(),
                decided_on_branch: BranchId::from("main"),
                decided_at: now,
                updated_at: now,
            })
            .expect("seed");
    }

    for hash in ["aaaa1111aaaaaaaa", "bbbb2222bbbbbbbb", "cccc3333cccccccc"] {
        let removed = forget_decision_with_database(&db, hash)
            .unwrap_or_else(|e| panic!("forget {hash}: {e}"));
        assert_eq!(removed.description_hash, hash);
        assert!(
            dec_repo.get_by_hash(hash).unwrap().is_none(),
            "row at {hash} must be gone after forget"
        );
    }
}
