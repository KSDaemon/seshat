//! Integration tests for US-016: cross-branch decisions survive a merge.
//!
//! Locks the merge-aware contract end-to-end: a decision recorded on a
//! feature branch must dedup the same auto-detected convention when work
//! lands on main — and must outlive the feature branch itself, because the
//! V12 `decisions` table is project-wide (keyed by `description_hash`,
//! decoupled from `branches`).
//!
//! Each test stands up a real git repo via `tempdir() + git init`, so the
//! freshness sentinel + branch-scoped node bookkeeping behave the same way
//! they do under a real `seshat scan` invocation.
//!
//! AC mapping (PRD §US-016):
//!   1. approve_on_feature_persists_after_merge_to_main
//!   2. reject_on_feature_persists_after_merge_to_main
//!   3. decision_survives_branch_deletion

use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use seshat_cli::tui::app::{ReviewAction, apply_review_actions};
use seshat_core::{BranchId, DetectionConfig, KnowledgeNode, ScanConfig};
use seshat_detectors::{ProjectContext, aggregate_findings, run_all_detectors};
use seshat_graph::compute_description_hash;
use seshat_scanner::scan_project;
use seshat_storage::{
    Database, DecisionRepository, DecisionState, FileIRRepository, NodeRepository,
    SqliteDecisionRepository, SqliteFileIRRepository, SqliteNodeRepository,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Git + fixture helpers
// ---------------------------------------------------------------------------

/// Run a git command in `cwd`; panic with a useful message on failure that
/// includes both stdout and stderr (so multi-step setup failures are
/// debuggable without re-running with `RUST_BACKTRACE`).
fn git(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed in {cwd:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Capture `git rev-parse <rev>` from `cwd` as a trimmed SHA.
fn rev_parse(rev: &str, cwd: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git rev-parse {rev} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "git rev-parse {rev} must succeed in {cwd:?}"
    );
    String::from_utf8(out.stdout)
        .expect("rev-parse stdout utf-8")
        .trim()
        .to_owned()
}

/// `git symbolic-ref --short HEAD` — the active branch label, used to assert
/// the test really did the checkout it thinks it did.
fn current_branch(cwd: &Path) -> String {
    let out = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(cwd)
        .output()
        .expect("git symbolic-ref HEAD");
    assert!(out.status.success(), "git symbolic-ref --short HEAD failed");
    String::from_utf8(out.stdout)
        .expect("symbolic-ref utf-8")
        .trim()
        .to_owned()
}

/// The same snapshot-hash recipe the production `reject_convention` path
/// uses internally (`crates/seshat-cli/src/tui/app.rs::compute_snapshot_hash`).
/// Reproduced here because the helper is `pub(crate)` and the integration
/// test lives outside the crate. The Reject AC requires this hash to match
/// the node's current `ext_data` snapshot — otherwise the optimistic
/// concurrency check in `reject_convention` aborts.
fn compute_snapshot_hash(ext_data: &Option<String>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::default();
    ext_data.as_deref().unwrap_or("").hash(&mut hasher);
    hasher.finish()
}

/// Drop a Rust source tree into `root`. Identical shape to the fixtures used
/// by `decisions_forget.rs` and `git_unavailable_fallback.rs`, kept in sync
/// so the same auto-detected conventions surface and the test can target
/// them by index without depending on detector ordering.
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

/// Initialise a git repo at `path`, drop the Rust source tree in, commit it
/// on `main`, and return the resulting HEAD SHA. The repo is reproducible
/// across runs (fixed user.name / user.email) so test failures bisect
/// cleanly.
///
/// `seshat.db` is `.gitignore`d up front — without that, the per-branch
/// scans inside the test would leave dirty working-tree state that blocks
/// the simulated-merge `git checkout` (the seshat artifact is not under
/// version control in real projects either).
fn init_repo_on_main(path: &Path) -> String {
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@seshat.dev"], path);
    git(&["config", "user.name", "Seshat Test"], path);

    fs::write(path.join(".gitignore"), "seshat.db\nseshat.db-*\n").expect("write .gitignore");
    write_rust_sources(path);

    git(&["add", "."], path);
    git(&["commit", "-m", "initial commit on main"], path);
    rev_parse("HEAD", path)
}

/// Branch off main into `feature`, add an unrelated file, and commit it so
/// `feature_head != main_head` (the realistic shape of a side branch). The
/// AC's "simulated merge" step is meaningful only when the two refs disagree
/// before the merge — otherwise nothing actually moves.
///
/// Returns the new HEAD on `feature`.
fn create_feature_branch_with_extra_commit(path: &Path) -> String {
    git(&["checkout", "-b", "feature"], path);
    fs::write(
        path.join("src").join("notes.rs"),
        "//! Side-branch addition; not load-bearing for the test, just makes\n\
         //! feature's HEAD diverge from main's HEAD.\n\
         pub fn note() -> &'static str { \"note\" }\n",
    )
    .expect("write notes.rs");
    git(&["add", "."], path);
    git(&["commit", "-m", "feature: add notes module"], path);
    rev_parse("HEAD", path)
}

/// Mirror the production scan + detect + persist pipeline on the currently
/// checked-out working tree, using the supplied `branch_id` for node
/// bookkeeping. Returns the auto-detected conventions visible after persist
/// for this branch (i.e. what the review queue would surface).
///
/// Same shape as `decisions_forget.rs::scan_and_persist` — duplicated rather
/// than factored out because each test file needs to evolve independently
/// and the helper is small.
fn scan_and_persist(repo: &Path, branch: &BranchId) -> Vec<KnowledgeNode> {
    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");

    let scan_result =
        scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("scan must succeed");

    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let all_files = file_ir_repo
        .get_by_branch(branch)
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

    seshat_graph::persist_and_index(&conn, branch, &aggregated, &all_findings)
        .expect("persist conventions");

    let node_repo = SqliteNodeRepository::new(conn);
    node_repo
        .find_conventions_by_branch(branch)
        .expect("query conventions")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// AC #1: approve a convention on `feature`, fast-forward `main` to
/// `feature`'s HEAD (i.e. simulate a merge), and assert the convention is
/// NOT re-emitted by the next scan on `main`. The decisions table is
/// project-wide, so the dedup signal travels with the merge.
#[test]
fn approve_on_feature_persists_after_merge_to_main() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let main_head = init_repo_on_main(repo);

    // Sanity: scan on main produces at least one auto-detected convention.
    let main_branch = BranchId::from("main");
    let conventions_main = scan_and_persist(repo, &main_branch);
    assert!(
        !conventions_main.is_empty(),
        "scan on main must produce auto-detected conventions"
    );

    // Branch off → feature has its own HEAD.
    let feature_head = create_feature_branch_with_extra_commit(repo);
    assert_ne!(
        main_head, feature_head,
        "feature must diverge from main before the simulated merge"
    );
    assert_eq!(current_branch(repo), "feature");

    // Scan on feature; the same convention is auto-detected against the
    // feature branch_id (source content is the same plus an extra file).
    let feature_branch = BranchId::from("feature");
    let conventions_feature = scan_and_persist(repo, &feature_branch);
    assert!(
        !conventions_feature.is_empty(),
        "scan on feature must produce auto-detected conventions"
    );
    let target = &conventions_feature[0];
    let target_description = target.description.clone();
    let target_node_id = target.id.0;
    let target_hash = compute_description_hash(&target_description);

    // Approve the convention on feature.
    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn: Arc<Mutex<rusqlite::Connection>> = db.connection().clone();
    apply_review_actions(
        &conn,
        "feature",
        &[ReviewAction::Confirm {
            node_id: target_node_id,
            description: target_description.clone(),
            examples: Vec::new(),
        }],
    )
    .expect("apply Confirm on feature");

    // The decision row records `feature` as the originating branch.
    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let decision = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash")
        .expect("decision row must exist after Confirm");
    assert_eq!(decision.state, DecisionState::Approved);
    assert_eq!(decision.decided_on_branch, BranchId::from("feature"));

    // Simulated merge: switch back to main and fast-forward main's ref to
    // feature's HEAD. `git merge --ff-only` is the most realistic shape of
    // "PR merged into main" without spelunking through `update-ref`.
    git(&["checkout", "main"], repo);
    git(&["merge", "--ff-only", "feature"], repo);
    assert_eq!(current_branch(repo), "main");
    assert_eq!(
        rev_parse("HEAD", repo),
        feature_head,
        "main HEAD must equal feature HEAD after the simulated merge"
    );

    // Rescan on main → the decided convention must NOT be re-emitted.
    // `persist_conventions` consults the project-wide decisions table on
    // every insert (US-008) so the cross-branch decision dedups here.
    let conventions_main_after = scan_and_persist(repo, &main_branch);
    let descriptions_after: Vec<_> = conventions_main_after
        .iter()
        .map(|c| c.description.clone())
        .collect();
    assert!(
        !descriptions_after.contains(&target_description),
        "approved convention must not re-emit on main after merge from feature; \
         got {descriptions_after:?}"
    );

    // The decision row is unchanged — same hash, same state, same origin.
    let decision_after = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash post-merge")
        .expect("decision row must persist across merge + rescan");
    assert_eq!(decision_after.state, DecisionState::Approved);
    assert_eq!(decision_after.decided_on_branch, BranchId::from("feature"));
}

/// AC #2: same flow as #1 but with `Reject` instead of `Confirm`. The
/// negative-decision path uses the same description-hash dedup channel, so
/// rejections must travel through merges identically to approvals.
#[test]
fn reject_on_feature_persists_after_merge_to_main() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let main_head = init_repo_on_main(repo);

    let main_branch = BranchId::from("main");
    scan_and_persist(repo, &main_branch);

    let feature_head = create_feature_branch_with_extra_commit(repo);
    assert_ne!(main_head, feature_head);

    let feature_branch = BranchId::from("feature");
    let conventions_feature = scan_and_persist(repo, &feature_branch);
    assert!(!conventions_feature.is_empty());
    let target = &conventions_feature[0];
    let target_description = target.description.clone();
    let target_node_id = target.id.0;
    let target_ext = target.ext_data.clone();
    let target_hash = compute_description_hash(&target_description);

    // The Reject path requires the snapshot_hash to match the node's
    // current ext_data — same optimistic concurrency check the TUI does.
    let snapshot_hash = compute_snapshot_hash(&target_ext.as_ref().map(|v| v.to_string()));

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn: Arc<Mutex<rusqlite::Connection>> = db.connection().clone();
    apply_review_actions(
        &conn,
        "feature",
        &[ReviewAction::Reject {
            node_id: target_node_id,
            snapshot_hash,
        }],
    )
    .expect("apply Reject on feature");

    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let decision = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash")
        .expect("decision row must exist after Reject");
    assert_eq!(decision.state, DecisionState::Rejected);
    assert_eq!(decision.decided_on_branch, BranchId::from("feature"));

    // Simulated merge.
    git(&["checkout", "main"], repo);
    git(&["merge", "--ff-only", "feature"], repo);
    assert_eq!(rev_parse("HEAD", repo), feature_head);

    // Rescan on main → the rejected convention must NOT be re-emitted.
    let conventions_main_after = scan_and_persist(repo, &main_branch);
    let descriptions_after: Vec<_> = conventions_main_after
        .iter()
        .map(|c| c.description.clone())
        .collect();
    assert!(
        !descriptions_after.contains(&target_description),
        "rejected convention must not re-emit on main after merge from feature; \
         got {descriptions_after:?}"
    );

    // Decision row is unchanged.
    let decision_after = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash post-merge")
        .expect("decision row must persist across merge + rescan");
    assert_eq!(decision_after.state, DecisionState::Rejected);
    assert_eq!(decision_after.decided_on_branch, BranchId::from("feature"));
}

/// T1: regression guard for **non-fast-forward** merges. The original AC #1
/// covers a `--ff-only` merge (linear history, main's ref jumps directly to
/// feature's HEAD). Real-world PR merges often produce a true 3-way merge
/// commit instead — main has diverged with its own work between the branch
/// point and the merge. The decision-by-hash dedup must hold there too:
/// it's keyed by `description_hash`, not by commit topology.
///
/// This test diverges main with an unrelated commit BEFORE merging feature
/// in via `git merge --no-ff -m`, producing an actual merge commit (two
/// parents). It then re-scans on main and asserts the previously-approved
/// convention does NOT re-appear in the review queue.
#[test]
fn approve_on_feature_persists_after_non_ff_merge_to_main() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    let main_head_initial = init_repo_on_main(repo);

    let main_branch = BranchId::from("main");
    scan_and_persist(repo, &main_branch);

    let feature_head = create_feature_branch_with_extra_commit(repo);
    assert_ne!(main_head_initial, feature_head);

    // Approve a convention on feature.
    let feature_branch = BranchId::from("feature");
    let conventions_feature = scan_and_persist(repo, &feature_branch);
    assert!(
        !conventions_feature.is_empty(),
        "feature scan must produce auto-detected conventions"
    );
    let target = &conventions_feature[0];
    let target_description = target.description.clone();
    let target_node_id = target.id.0;
    let target_hash = compute_description_hash(&target_description);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn: Arc<Mutex<rusqlite::Connection>> = db.connection().clone();
    apply_review_actions(
        &conn,
        "feature",
        &[ReviewAction::Confirm {
            node_id: target_node_id,
            description: target_description.clone(),
            examples: Vec::new(),
        }],
    )
    .expect("apply Confirm on feature");

    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let approved = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash")
        .expect("decision row must exist after Confirm");
    assert_eq!(approved.state, DecisionState::Approved);

    // Diverge main with its own commit so the upcoming merge cannot
    // fast-forward — `git merge --no-ff` will create a true 3-way merge
    // commit with two parents.
    git(&["checkout", "main"], repo);
    fs::write(
        repo.join("src").join("main_only.rs"),
        "//! Divergent commit on main; forces a non-FF merge below.\n\
         pub fn main_only() -> &'static str { \"main\" }\n",
    )
    .expect("write main_only.rs");
    git(&["add", "."], repo);
    git(&["commit", "-m", "main: add main-only module"], repo);
    let main_head_pre_merge = rev_parse("HEAD", repo);
    assert_ne!(main_head_pre_merge, feature_head);
    assert_ne!(main_head_pre_merge, main_head_initial);

    // Force a real merge commit (two parents). --no-ff guarantees the
    // commit even when fast-forward would be possible; here it's actually
    // required because the histories diverged.
    git(
        &[
            "merge",
            "--no-ff",
            "-m",
            "Merge branch 'feature' into main",
            "feature",
        ],
        repo,
    );
    let merge_commit = rev_parse("HEAD", repo);
    assert_ne!(
        merge_commit, feature_head,
        "non-FF merge must produce a new merge commit, not a FF to feature"
    );
    assert_ne!(
        merge_commit, main_head_pre_merge,
        "merge commit must move main forward"
    );

    // Two parents — the canonical signal that this was a true 3-way merge.
    let parents = rev_parse("HEAD^@", repo);
    assert_eq!(
        parents.lines().count(),
        2,
        "non-FF merge must have exactly two parents; got: {parents:?}"
    );

    // Rescan on main after the merge: the approved convention must NOT
    // re-emit. This is the contract — dedup is by description_hash, not
    // by commit topology.
    let conventions_main_after = scan_and_persist(repo, &main_branch);
    let descriptions_after: Vec<_> = conventions_main_after
        .iter()
        .map(|c| c.description.clone())
        .collect();
    assert!(
        !descriptions_after.contains(&target_description),
        "approved convention must not re-emit on main after a NON-FF merge; \
         got {descriptions_after:?}"
    );

    // Decision row is unchanged — same hash, same origin branch, same state.
    let decision_after = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash post-merge")
        .expect("decision row must persist across non-FF merge + rescan");
    assert_eq!(decision_after.state, DecisionState::Approved);
    assert_eq!(decision_after.decided_on_branch, BranchId::from("feature"));
}

/// AC #3: approve on `feature`, then delete the feature branch entirely.
/// The decisions table is project-wide and has no FK back into `branches`,
/// so the row must survive — and the next scan on `main` must still dedup
/// the convention. This pins the "decisions are decoupled from branches"
/// invariant from the V12 schema.
#[test]
fn decision_survives_branch_deletion() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    init_repo_on_main(repo);

    let main_branch = BranchId::from("main");
    scan_and_persist(repo, &main_branch);

    create_feature_branch_with_extra_commit(repo);
    let feature_branch = BranchId::from("feature");
    let conventions_feature = scan_and_persist(repo, &feature_branch);
    assert!(!conventions_feature.is_empty());
    let target = &conventions_feature[0];
    let target_description = target.description.clone();
    let target_node_id = target.id.0;
    let target_hash = compute_description_hash(&target_description);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("reopen DB");
    let conn: Arc<Mutex<rusqlite::Connection>> = db.connection().clone();
    apply_review_actions(
        &conn,
        "feature",
        &[ReviewAction::Confirm {
            node_id: target_node_id,
            description: target_description.clone(),
            examples: Vec::new(),
        }],
    )
    .expect("apply Confirm on feature");

    // Sanity: the decision row exists, scoped to feature.
    let dec_repo = SqliteDecisionRepository::new(conn.clone());
    let decision_before = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash")
        .expect("decision row must exist after Confirm");
    assert_eq!(decision_before.state, DecisionState::Approved);
    assert_eq!(decision_before.decided_on_branch, BranchId::from("feature"));

    // Delete the feature branch outright (no merge). `-D` force-deletes
    // even though feature is not merged into main; main is the active
    // branch since checkout switches there before the delete.
    git(&["checkout", "main"], repo);
    git(&["branch", "-D", "feature"], repo);

    // Scan on main → convention must NOT be re-emitted.
    let conventions_main_after = scan_and_persist(repo, &main_branch);
    let descriptions_after: Vec<_> = conventions_main_after
        .iter()
        .map(|c| c.description.clone())
        .collect();
    assert!(
        !descriptions_after.contains(&target_description),
        "approved convention must not re-emit on main even after feature branch is deleted; \
         got {descriptions_after:?}"
    );

    // The decision row still exists, unchanged — proves V12 decisions are
    // decoupled from `branches` (no FK cascade, no orphan-branch cleanup).
    let decision_after = dec_repo
        .get_by_hash(&target_hash)
        .expect("get_by_hash post branch-delete")
        .expect("decision row must outlive the originating branch");
    assert_eq!(decision_after.state, DecisionState::Approved);
    assert_eq!(decision_after.decided_on_branch, BranchId::from("feature"));
    assert_eq!(decision_after.description, target_description);
}
