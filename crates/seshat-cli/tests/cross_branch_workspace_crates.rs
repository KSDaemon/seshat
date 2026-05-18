//! Integration test: per-branch `workspace_crates` eliminates cross-branch
//! contamination of internal-name resolution.
//!
//! Pinned contract: when two branches declare different workspace members,
//! `query_dependencies` on the same file must resolve cross-crate imports
//! against **that branch's** `workspace_crates`, not against whichever scan
//! happened to run last.
//!
//! Fixture shape:
//!
//! - `main`   — workspace `members = ["crate_a"]`. `crate_b/` does NOT exist.
//! - `feature`— workspace `members = ["crate_a", "crate_b"]`. `crate_b/`
//!   exists with `src/greet.rs`.
//!
//! `crate_a/src/lib.rs` is identical on both branches and contains
//! `use crate_b::greet::say;`. The same import line is therefore present in
//! the IR for both scans — only the workspace membership differs.
//!
//! AC mapping (PRD §US-006):
//!   - main:    crate_b is EXTERNAL → absent from `dependencies`.
//!   - feature: crate_b is INTERNAL → resolved entry pointing at
//!     `crate_b/src/greet.rs` shows up in `dependencies`.
//!
//! Embeddings are off (no `[embedding]` config + `ScanConfig::default()`),
//! so the scan keeps well under the 10-second budget the AC asks for.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use seshat_core::{BranchId, ScanConfig};
use seshat_graph::{QueryDependenciesOptions, query_dependencies};
use seshat_scanner::scan_project;
use seshat_storage::{BranchMetadataRepository, Database, SqliteBranchMetadataRepository};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Git + fixture helpers
//
// Helpers are intentionally duplicated from `cross_branch_decisions.rs` and
// `scan_records_head.rs` (the existing per-file convention in this crate).
// Keeps each integration test self-contained so they can evolve without
// coupling.
// ---------------------------------------------------------------------------

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

/// Write the shared fixture: a workspace root manifest listing only
/// `crate_a`, plus `crate_a/Cargo.toml` and `crate_a/src/lib.rs`. The
/// lib.rs contains an import of `crate_b::greet::say` — same file on
/// both branches, so the cross-branch difference comes entirely from
/// workspace membership, not from on-disk import shape.
fn write_main_fixture(root: &Path) {
    fs::write(
        root.join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a"]
resolver = "2"
"#,
    )
    .expect("write root Cargo.toml (main)");

    let crate_a = root.join("crate_a");
    fs::create_dir_all(crate_a.join("src")).expect("create crate_a/src");
    fs::write(
        crate_a.join("Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("write crate_a/Cargo.toml");
    // crate_a/src/lib.rs imports `crate_b::greet::say` so the IR carries an
    // Import { module: "crate_b::greet", names: ["say"] } on both branches.
    // The resolver distinguishes between them only via workspace_crates.
    fs::write(
        crate_a.join("src/lib.rs"),
        r#"use crate_b::greet::say;

pub fn hello() {
    say();
}
"#,
    )
    .expect("write crate_a/src/lib.rs");
}

/// Branch off main into feature and add `crate_b/`. The root Cargo.toml is
/// rewritten so workspace members now include both crates. `crate_a` is
/// left untouched — its `lib.rs` already contains the `use crate_b::*`
/// import added on main.
fn add_crate_b_on_feature(root: &Path) {
    fs::write(
        root.join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("rewrite root Cargo.toml (feature)");

    let crate_b = root.join("crate_b");
    fs::create_dir_all(crate_b.join("src")).expect("create crate_b/src");
    fs::write(
        crate_b.join("Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("write crate_b/Cargo.toml");
    // `crate_b/src/greet.rs` is the resolution target. Its suffix
    // `greet.rs` is what `resolve_internal_crate_import("crate_b::greet")`
    // looks for after stripping the crate prefix.
    fs::write(
        crate_b.join("src/greet.rs"),
        r#"pub fn say() {
    println!("hi");
}
"#,
    )
    .expect("write crate_b/src/greet.rs");
    fs::write(
        crate_b.join("src/lib.rs"),
        r#"pub mod greet;
"#,
    )
    .expect("write crate_b/src/lib.rs");
}

/// Initialise the repo on `main` with `crate_a` only, committed.
fn init_repo(path: &Path) {
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@seshat.dev"], path);
    git(&["config", "user.name", "Seshat Test"], path);

    // seshat.db must stay out of git so per-branch scans don't dirty the
    // working tree (would block checkouts otherwise).
    fs::write(path.join(".gitignore"), "seshat.db\nseshat.db-*\n").expect("write .gitignore");
    write_main_fixture(path);

    git(&["add", "."], path);
    git(&["commit", "-m", "initial commit on main"], path);
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// Regression guard: scan main then feature on the same repo, and assert
/// that `query_dependencies(crate_a/src/lib.rs)` resolves `crate_b::greet`
/// differently per branch. Locks the per-branch read in
/// `seshat_graph::dependencies::load_internal_names` against any future
/// regression to the global `repo_metadata` slot.
#[test]
fn workspace_crates_resolves_per_branch_across_real_git_branches() {
    let workdir = tempdir().expect("tempdir");
    let repo = workdir.path();
    init_repo(repo);

    // ---- Scan #1: main, members = ["crate_a"] -----------------------------
    let db = Database::open(repo.join("seshat.db")).expect("open DB");
    let main_branch = BranchId::from("main");
    scan_project(repo, &ScanConfig::default(), &db, main_branch.clone()).expect("scan main");

    // Sanity: branch_metadata for main must contain workspace_crates and
    // crate_b must NOT be there. `analyze_manifests` may duplicate names
    // (one per source manifest), so we assert membership, not exact shape.
    let branch_meta = SqliteBranchMetadataRepository::new(db.connection().clone());
    let main_wc_json = branch_meta
        .get(&main_branch.0, "workspace_crates")
        .expect("branch_metadata.get must succeed")
        .expect("workspace_crates must be persisted for main");
    let main_wc: Vec<String> =
        serde_json::from_str(&main_wc_json).expect("workspace_crates is JSON array");
    assert!(
        main_wc.contains(&"crate_a".to_owned()),
        "main's workspace_crates must include crate_a; got {main_wc:?}",
    );
    assert!(
        !main_wc.contains(&"crate_b".to_owned()),
        "main's workspace_crates must NOT include crate_b (regression guard); got {main_wc:?}",
    );

    // ---- Switch to feature and add crate_b --------------------------------
    git(&["checkout", "-b", "feature"], repo);
    assert_eq!(current_branch(repo), "feature");
    add_crate_b_on_feature(repo);
    git(&["add", "."], repo);
    git(
        &["commit", "-m", "feature: add crate_b workspace member"],
        repo,
    );

    // ---- Scan #2: feature, members = ["crate_a", "crate_b"] ---------------
    scan_project(repo, &ScanConfig::default(), &db, BranchId::from("feature"))
        .expect("scan feature");

    let feature_wc_json = branch_meta
        .get("feature", "workspace_crates")
        .expect("branch_metadata.get must succeed")
        .expect("workspace_crates must be persisted for feature");
    let feature_wc: Vec<String> =
        serde_json::from_str(&feature_wc_json).expect("workspace_crates is JSON array");
    assert!(
        feature_wc.contains(&"crate_a".to_owned()) && feature_wc.contains(&"crate_b".to_owned()),
        "feature's workspace_crates must include both crate_a and crate_b; got {feature_wc:?}",
    );

    // Cross-branch isolation guard: scanning feature must not have
    // overwritten main's row.
    let main_wc_json_after = branch_meta
        .get(&main_branch.0, "workspace_crates")
        .expect("branch_metadata.get must succeed")
        .expect("main's workspace_crates row must survive the feature scan");
    assert_eq!(
        main_wc_json_after, main_wc_json,
        "scanning feature must not mutate main's workspace_crates row",
    );

    let conn = db.connection().clone();

    // ---- AC: feature resolves crate_b::greet to crate_b/src/greet.rs -----
    let feature_result = query_dependencies(
        &conn,
        "feature",
        "crate_a/src/lib.rs",
        QueryDependenciesOptions::default(),
    )
    .expect("query_dependencies on feature must succeed");
    let feature_resolved: Vec<_> = feature_result
        .dependencies
        .iter()
        .filter(|d| d.resolved)
        .collect();
    assert!(
        feature_resolved
            .iter()
            .any(|d| d.file_path.contains("crate_b/src/greet.rs")
                || d.file_path.contains("crate_b\\src\\greet.rs")),
        "feature branch must resolve crate_b::greet to crate_b/src/greet.rs; \
         got deps: {:?}",
        feature_result.dependencies,
    );

    // ---- AC: main treats crate_b as external → absent from `dependencies` -
    // Switch back to main on disk so the scan reflects main's tree (crate_b
    // gone from the working dir; only the previously persisted IR for main
    // is queried by branch_id).
    git(&["checkout", "main"], repo);
    assert_eq!(current_branch(repo), "main");

    let main_result = query_dependencies(
        &conn,
        &main_branch.0,
        "crate_a/src/lib.rs",
        QueryDependenciesOptions::default(),
    )
    .expect("query_dependencies on main must succeed");
    assert!(
        !main_result
            .dependencies
            .iter()
            .any(|d| d.file_path.contains("crate_b")),
        "main branch must NOT resolve any crate_b dependency (crate_b is \
         external on main); got deps: {:?}",
        main_result.dependencies,
    );
    // Stronger negative: with crate_a's only import being `crate_b::greet::say`
    // and crate_b not in main's workspace_crates, the resolved-dependency
    // list must be empty for crate_a/src/lib.rs on main.
    assert!(
        main_result.dependencies.iter().all(|d| !d.resolved),
        "main branch must have no resolved dependencies for crate_a/src/lib.rs; \
         got deps: {:?}",
        main_result.dependencies,
    );
}
