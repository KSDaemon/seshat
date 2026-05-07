//! Integration tests for US-017: `seshat serve` freshness gate.
//!
//! `run_serve` runs an MCP server until Ctrl+C, so we cannot drive it
//! end-to-end. Instead we lock the gate at the same layer `run_serve` uses to
//! decide whether to spawn `background_sync`:
//!
//! ```ignore
//! let sync_old_branch = old_branch_for_sync.filter(|b| *b != final_branch.0);
//! let head_change_hint: Option<String> = match check_branch_freshness(...) {
//!     UpToDate | GitUnavailable => None,
//!     Stale { old_commit, .. }  => old_commit,
//! };
//! let needs_sync = sync_old_branch.is_some() || head_change_hint.is_some();
//! ```
//!
//! Each test sets up a real git fixture (or a non-git tempdir) and asserts the
//! `needs_sync` boolean would resolve correctly via [`would_serve_trigger_sync`],
//! which mirrors the production gate verbatim. Mirroring is the contract: if a
//! future change rewrites `run_serve`'s gate, the test helper here must be
//! updated in lockstep, and that mismatch will be visible in code review.
//!
//! AC mapping (PRD §US-017):
//!   1. serve_detects_branch_label_change_and_syncs (existing path — guard
//!      regression: branch label changed since last serve startup)
//!   2. serve_detects_same_branch_head_change_and_syncs (US-010's path: HEAD
//!      advanced on the same branch)
//!   3. serve_skips_sync_when_head_unchanged (UpToDate, no branch switch)
//!   4. serve_skips_sync_when_git_unavailable (no git → no sync, no warnings)
//!
//! Companion file: `serve_head_change.rs` covers AC#2 in isolation as US-010's
//! original deliverable; its test is preserved unchanged because removing it
//! would break US-010's PRD-locked artefact. The same-branch HEAD test here is
//! a deliberate regression-guard duplicate.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use seshat_cli::db::detect_branch;
use seshat_core::{BranchId, ScanConfig};
use seshat_scanner::{
    FreshnessCheck, check_branch_freshness, record_branch_scan_complete, scan_project,
};
use seshat_storage::{BranchRepository, Database, SqliteBranchRepository};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Git + fixture helpers
// ---------------------------------------------------------------------------

/// Run a git command in `cwd`; panic with a useful message that includes both
/// stdout and stderr so multi-step setup failures are debuggable without
/// re-running with `RUST_BACKTRACE`. (Pattern from US-016's
/// `cross_branch_decisions.rs`.)
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

/// `git symbolic-ref --short HEAD` — the active branch label, used to assert
/// the test really did the checkout it thinks it did.
fn current_branch_label(cwd: &Path) -> String {
    let out = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(cwd)
        .output()
        .expect("git symbolic-ref --short HEAD");
    assert!(
        out.status.success(),
        "git symbolic-ref must succeed in {cwd:?}"
    );
    String::from_utf8(out.stdout)
        .expect("symbolic-ref stdout utf-8")
        .trim()
        .to_owned()
}

/// Initialise a git repo at `path` with `--initial-branch=main`, commit a
/// single Rust source file, and return the resulting HEAD SHA.
///
/// `.gitignore`s `seshat.db` BEFORE the initial commit so per-branch scans
/// don't dirty the working tree (otherwise subsequent `git checkout`s abort
/// with "local changes would be overwritten" — see US-016's learnings).
fn init_repo_on_main(path: &Path) -> String {
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@seshat.dev"], path);
    git(&["config", "user.name", "Seshat Test"], path);

    // Avoid contaminating the worktree with the test DB on per-branch scans.
    fs::write(path.join(".gitignore"), "seshat.db\nseshat.db-*\n").expect("write .gitignore");

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

/// Modify `src/lib.rs` and commit on the current branch. Returns the new HEAD.
fn commit_modification_on_current_branch(path: &Path) -> String {
    fs::write(
        path.join("src").join("lib.rs"),
        "pub fn hello() -> &'static str {\n    \"hello, world\"\n}\n",
    )
    .expect("modify lib.rs");
    git(&["add", "."], path);
    git(&["commit", "-m", "follow-up commit"], path);
    rev_parse_head(path)
}

/// Create branch `feature` from current HEAD, check it out, add a commit, and
/// return the feature-branch HEAD SHA.
fn create_and_checkout_feature_with_commit(path: &Path) -> String {
    git(&["checkout", "-b", "feature"], path);
    fs::write(
        path.join("src").join("util.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .expect("write util.rs");
    git(&["add", "."], path);
    git(&["commit", "-m", "add util.rs on feature"], path);
    rev_parse_head(path)
}

/// Mirror of `run_serve`'s `needs_sync` boolean. The two inputs reflect the
/// state `run_serve` has after auto-scan/branch-switch resolution:
///
///   - `recorded_branch` — `repo_info.branch` after `handle_branch_switch`
///     ran (i.e. `final_branch`).
///   - `previous_branch` — the branch the DB was tracking BEFORE the switch
///     (`old_branch_for_sync`); `None` for the auto-scan path.
///
/// Returns the same boolean `run_serve` evaluates at `serve.rs:676`:
///
/// ```ignore
/// let head_change_hint: Option<String> = match check_branch_freshness(...) {
///     UpToDate | GitUnavailable                => None,
///     Stale { old_commit, new_commit }         => old_commit, // <-- the contract
/// };
/// let sync_old_branch = old_branch_for_sync.filter(|b| *b != final_branch.0);
/// let needs_sync = sync_old_branch.is_some() || head_change_hint.is_some();
/// ```
///
/// Note the production gate's subtle contract: `head_change_hint = old_commit`,
/// so a `Stale` result with `old_commit: None` (never-scanned branch / pre-US-009
/// DB) does NOT fire the head-change leg. That case can only fire via the
/// branch-switch leg. This mirroring is exact — diverging from it would
/// invalidate the test as a contract.
///
/// Tests assert via this helper rather than by spawning `run_serve` (which
/// blocks on stdio MCP forever).
fn would_serve_trigger_sync<R: BranchRepository>(
    branch_repo: &R,
    root: &Path,
    recorded_branch: &BranchId,
    previous_branch: Option<&str>,
) -> bool {
    let sync_old_branch = previous_branch.filter(|b| *b != recorded_branch.0);
    let head_change_hint: Option<String> =
        match check_branch_freshness(branch_repo, root, recorded_branch) {
            FreshnessCheck::UpToDate | FreshnessCheck::GitUnavailable => None,
            FreshnessCheck::Stale { old_commit, .. } => old_commit,
        };
    sync_old_branch.is_some() || head_change_hint.is_some()
}

// ---------------------------------------------------------------------------
// AC tests
// ---------------------------------------------------------------------------

/// AC#1 (existing path — guard regression): the user `git checkout`s to a new
/// branch, then restarts `seshat serve`. The DB still tracks the old branch as
/// "current", so `handle_branch_switch` switches over and `old_branch_for_sync`
/// yields a Some that's different from `final_branch` — flipping `needs_sync`
/// to true via `sync_old_branch.is_some()`.
///
/// The test:
///   1. Init repo on `main`, scan, record sentinel.
///   2. Create `feature` branch with an extra commit (HEAD now on feature).
///   3. Verify `detect_branch` returns "feature" (the gate's input).
///   4. Compute `needs_sync` via the gate-mirroring helper, with
///      `recorded_branch = feature` (after handle_branch_switch) and
///      `previous_branch = Some("main")` (the DB's pre-switch tracked branch)
///      — assert it's `true` because `"main" != "feature"`.
///   5. Even if the freshness check on `feature` says UpToDate (the new branch
///      was never scanned, so `last_scanned_commit` is NULL → Stale-with-None
///      old, but irrelevant here), the gate still triggers via the
///      branch-switch leg.
#[test]
fn serve_detects_branch_label_change_and_syncs() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    assert!(
        !repo.join(".git").exists(),
        "tempdir must not inherit a parent .git"
    );

    let _head_main = init_repo_on_main(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let main = BranchId::from("main");

    // Initial scan on main + record sentinel.
    scan_project(repo, &ScanConfig::default(), &db, main.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    record_branch_scan_complete(&branch_repo, repo, &main);

    // The user creates a feature branch and switches to it.
    let _head_feature = create_and_checkout_feature_with_commit(repo);
    assert_eq!(
        current_branch_label(repo),
        "feature",
        "git HEAD must point to feature after the checkout"
    );
    assert_eq!(
        detect_branch(repo),
        "feature",
        "detect_branch must reflect the checked-out branch — this is the input \
         that flips repo_info.branch in run_serve"
    );

    // Mirror handle_branch_switch's outcome: final_branch becomes "feature";
    // old_branch_for_sync was Some("main") (the DB's pre-switch branch).
    let feature = BranchId::from("feature");
    let needs_sync = would_serve_trigger_sync(&branch_repo, repo, &feature, Some(main.0.as_str()));
    assert!(
        needs_sync,
        "needs_sync must be true after a branch label change (sync_old_branch \
         leg of the gate); recorded={feature:?}, previous=Some(\"main\")"
    );
}

/// AC#2: HEAD advances on the same branch (the user `git pull`-ed). The
/// freshness gate's `head_change_hint` leg flips `needs_sync` to true.
///
/// Mirrors `serve_head_change.rs::serve_freshness_gate_returns_stale_when_head_advances_on_same_branch`
/// intentionally — that test was US-010's deliverable; this duplicate is the
/// US-017 regression guard so a future refactor that breaks one test surfaces
/// in both files (defence in depth). The duplication cost is one extra ~30
/// LOC test against the same gate primitive; the benefit is clear AC mapping.
#[test]
fn serve_detects_same_branch_head_change_and_syncs() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    assert!(!repo.join(".git").exists());

    let head1 = init_repo_on_main(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let main = BranchId::from("main");

    scan_project(repo, &ScanConfig::default(), &db, main.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    record_branch_scan_complete(&branch_repo, repo, &main);

    // Sanity: sentinel matches HEAD, gate is UpToDate.
    assert_eq!(
        check_branch_freshness(&branch_repo, repo, &main),
        FreshnessCheck::UpToDate
    );

    // User `git pull`s — HEAD advances to head2 on the same branch.
    let head2 = commit_modification_on_current_branch(repo);
    assert_ne!(head1, head2, "follow-up commit must produce a new SHA");
    assert_eq!(
        current_branch_label(repo),
        "main",
        "branch label must still be main"
    );

    let result = check_branch_freshness(&branch_repo, repo, &main);
    assert_eq!(
        result,
        FreshnessCheck::Stale {
            old_commit: Some(head1.clone()),
            new_commit: head2,
        },
        "freshness gate must surface head1 as old_commit hint"
    );

    // Gate-mirror: previous_branch == recorded_branch ("main"), so
    // sync_old_branch is None; needs_sync flips solely via head_change_hint.
    let needs_sync = would_serve_trigger_sync(&branch_repo, repo, &main, Some("main"));
    assert!(
        needs_sync,
        "needs_sync must be true on a same-branch HEAD change \
         (head_change_hint leg of the gate); previous=Some(\"main\")=recorded"
    );
}

/// AC#3: sentinel matches HEAD AND no branch switch. `check_branch_freshness`
/// returns `UpToDate`; both legs of the gate are false; `needs_sync = false`.
#[test]
fn serve_skips_sync_when_head_unchanged() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    assert!(!repo.join(".git").exists());

    let head1 = init_repo_on_main(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let main = BranchId::from("main");

    scan_project(repo, &ScanConfig::default(), &db, main.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    record_branch_scan_complete(&branch_repo, repo, &main);

    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&main)
            .expect("read sentinel"),
        Some(head1),
        "sentinel must be recorded for the active branch"
    );
    assert_eq!(
        check_branch_freshness(&branch_repo, repo, &main),
        FreshnessCheck::UpToDate,
        "freshness gate must be UpToDate when sentinel matches HEAD"
    );

    // Gate-mirror: recorded == previous AND UpToDate ⇒ both legs false.
    let needs_sync = would_serve_trigger_sync(&branch_repo, repo, &main, Some("main"));
    assert!(
        !needs_sync,
        "needs_sync must be false on UpToDate same-branch startup"
    );
}

/// AC#4: non-git directory. `check_branch_freshness` short-circuits to
/// `GitUnavailable`; `head_change_hint` is `None`; with no branch change
/// either, `needs_sync = false`. No warnings, no panics, sentinel stays NULL.
#[test]
fn serve_skips_sync_when_git_unavailable() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    assert!(
        !repo.join(".git").exists(),
        "non-git fixture must really have no .git"
    );

    // No `git init` — plain directory with one source file.
    let src = repo.join("src");
    fs::create_dir_all(&src).expect("create src dir");
    fs::write(
        src.join("lib.rs"),
        "pub fn hello() -> &'static str {\n    \"hi\"\n}\n",
    )
    .expect("write lib.rs");

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let main = BranchId::from("main");

    // `detect_branch` falls back to "main" in non-git dirs (US-012 contract).
    assert_eq!(detect_branch(repo), "main");

    scan_project(repo, &ScanConfig::default(), &db, main.clone()).expect("initial scan");
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    // record_branch_scan_complete is a silent no-op on git-unavailable per
    // US-009 — sentinel stays NULL.
    record_branch_scan_complete(&branch_repo, repo, &main);
    assert_eq!(
        branch_repo
            .get_last_scanned_commit(&main)
            .expect("read sentinel"),
        None,
        "sentinel must remain NULL when git is unavailable"
    );

    assert_eq!(
        check_branch_freshness(&branch_repo, repo, &main),
        FreshnessCheck::GitUnavailable,
        "freshness gate must short-circuit to GitUnavailable in non-git dirs"
    );

    // Gate-mirror: recorded == previous (no branch switch) AND GitUnavailable
    // ⇒ both legs false ⇒ no sync.
    let needs_sync = would_serve_trigger_sync(&branch_repo, repo, &main, Some("main"));
    assert!(
        !needs_sync,
        "needs_sync must be false on git-unavailable startup"
    );
}
