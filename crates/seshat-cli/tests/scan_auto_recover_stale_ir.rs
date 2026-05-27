//! End-to-end regression test for the "stale IR cache hard-fails scan" bug.
//!
//! Before this fix, running `seshat scan` against a DB whose `files_ir`
//! rows were written by an older `IR_SCHEMA_VERSION` failed with:
//!
//! ```text
//! scan failed: Storage error: SQLite error: Conversion error from type Blob
//! at index: 0, Stale IR: cached version 7 != current version 8
//! ```
//!
//! The user had to delete the DB file by hand and lose every user-curated
//! `decisions` row in the process. The scan path now auto-recovers by
//! clearing the stale cache up-front, and this test pins that contract:
//!
//! 1. A stale `files_ir` row is detected and removed.
//! 2. A fresh scan over the same project succeeds.
//! 3. The `decisions` table is untouched across the wipe + rescan.
//! 4. After the rescan, the cache holds current-schema rows again.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use rusqlite::params;
use seshat_core::{BranchId, ScanConfig};
use seshat_scanner::scan_project;
use seshat_storage::{
    Database, FileIRRepository, IR_SCHEMA_VERSION, SqliteFileIRRepository, wipe_stale_ir_cache,
};
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

fn init_git_repo_with_rust_file(path: &Path) {
    git(&["init", "-b", "main"], path);
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
}

/// Insert a `files_ir` row tagged with an arbitrary `ir_schema_version`
/// and a placeholder blob — simulates a row written by an older binary.
fn insert_stale_files_ir_row(db: &Database, branch: &str, file_path: &str, version: i64) {
    let conn = db.connection().lock().unwrap();
    conn.execute(
        "INSERT INTO files_ir
            (branch_id, file_path, language, content_hash, ir_data, ir_schema_version,
             last_commit_date, updated_at)
         VALUES (?1, ?2, 'rust', 'h', ?3, ?4, NULL, datetime('now'))",
        params![branch, file_path, vec![0u8, 0u8, 0u8], version],
    )
    .expect("insert stale files_ir row");
}

/// Seed a user-curated `decisions` row so we can prove the wipe doesn't
/// disturb it.
fn seed_decision(db: &Database, hash: &str, text: &str) {
    let conn = db.connection().lock().unwrap();
    conn.execute(
        "INSERT INTO decisions
            (description_hash, description, state, nature, weight,
             decided_on_branch, decided_at)
         VALUES (?1, ?2, 'recorded', 'decision', 'strong', 'main', 1700000000)",
        params![hash, text],
    )
    .expect("seed decision");
}

fn count_decisions(db: &Database) -> i64 {
    let conn = db.connection().lock().unwrap();
    conn.query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn scan_auto_recovers_from_stale_ir_cache() {
    let workdir = tempdir().expect("create temp dir");
    let repo = workdir.path();
    init_git_repo_with_rust_file(repo);

    let db_path = repo.join("seshat.db");
    let branch = BranchId::from("main");

    // -- Set up the "broken" state ----------------------------------------
    // Pre-populate the DB with a stale v7 row, two user-curated decisions,
    // then close the DB so the rest of the test starts from disk.
    {
        let db = Database::open(&db_path).expect("open DB");
        insert_stale_files_ir_row(&db, "main", "src/lib.rs", 7);
        seed_decision(&db, "h1", "decision A");
        seed_decision(&db, "h2", "decision B");
    }

    // -- Drive the auto-recovery path -------------------------------------
    // This is what `run_scan` does up-front. Without this call, the
    // subsequent `scan_project` would crash at `get_by_branch` time on
    // the v7 blob.
    let db = Database::open(&db_path).expect("reopen DB");
    let report = wipe_stale_ir_cache(&db).expect("wipe stale IR");
    assert_eq!(
        report.stale_count, 1,
        "must detect and wipe exactly one stale row"
    );
    assert_eq!(report.cached_versions, vec![7]);

    // -- Run a real scan over the project ---------------------------------
    scan_project(repo, &ScanConfig::default(), &db, branch.clone())
        .expect("scan must succeed after auto-recovery");

    // -- Assertions -------------------------------------------------------
    let repo_files = SqliteFileIRRepository::new(db.connection().clone());

    // The scan should have re-parsed lib.rs into a current-version row.
    let files = repo_files
        .get_by_branch(&branch)
        .expect("get_by_branch must NOT fail with stale-IR error");
    assert_eq!(files.len(), 1, "scan should re-populate the cache");
    assert_eq!(files[0].path.to_string_lossy(), "src/lib.rs");

    // Every row in `files_ir` must now carry the current IR_SCHEMA_VERSION.
    {
        let conn = db.connection().lock().unwrap();
        let any_stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files_ir WHERE ir_schema_version != ?1",
                params![i64::from(IR_SCHEMA_VERSION)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            any_stale, 0,
            "no stale rows should remain after auto-recovery + scan"
        );
    }

    // Decisions are project-wide user data — the recovery path MUST NOT
    // touch them, even though it cleared the IR cache.
    assert_eq!(
        count_decisions(&db),
        2,
        "all user-curated decisions must survive the auto-recovery + rescan"
    );
}

#[test]
fn second_scan_is_a_noop_for_the_recovery_path() {
    // Locks the idempotency contract: once the cache is current, subsequent
    // scans must not log "wiped N rows" — the wipe helper should report
    // zero stale rows.
    let workdir = tempdir().expect("create temp dir");
    let repo = workdir.path();
    init_git_repo_with_rust_file(repo);

    let db_path = repo.join("seshat.db");
    let db = Database::open(&db_path).expect("open DB");
    let branch = BranchId::from("main");

    // First scan: cache is fresh from the start, so the wipe is a no-op.
    let first = wipe_stale_ir_cache(&db).expect("wipe 1");
    assert!(first.is_empty(), "fresh DB should have nothing to wipe");
    scan_project(repo, &ScanConfig::default(), &db, branch.clone()).expect("first scan");

    // Second scan: cache is full of current-schema rows from scan #1.
    let second = wipe_stale_ir_cache(&db).expect("wipe 2");
    assert!(
        second.is_empty(),
        "after a successful scan, the cache must be current — wipe must be a no-op"
    );
}
