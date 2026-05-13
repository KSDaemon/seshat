//! Integration tests for US-003: watcher hot tier keeps the symbol-index
//! tables (`symbol_definitions`, `symbol_imports`) in sync as files change
//! without a server restart.
//!
//! These tests call `process_file_change` / `process_file_delete` directly
//! (the same entry points the spawned hot-tier task uses) rather than driving
//! `notify-debouncer-full` end-to-end — FS-event delivery is not reliable
//! enough on CI to assert against.  The unit tests in `hot_tier::tests`
//! already cover the IR-only behaviour; these tests pin the new symbol-index
//! invariants added by US-003.

use std::fs;

use seshat_core::{BranchId, ScanConfig};
use seshat_storage::Database;
use seshat_watcher::{process_file_change, process_file_delete};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// SQL helpers (kept tiny so each test stays readable)
// ---------------------------------------------------------------------------

fn count_definitions(db: &Database, branch: &str, file_path: &str) -> i64 {
    let conn = db.connection().lock().expect("lock conn");
    conn.query_row(
        "SELECT COUNT(*) FROM symbol_definitions WHERE branch_id = ?1 AND file_path = ?2",
        rusqlite::params![branch, file_path],
        |row| row.get::<_, i64>(0),
    )
    .expect("count definitions")
}

fn count_imports(db: &Database, branch: &str, importer: &str) -> i64 {
    let conn = db.connection().lock().expect("lock conn");
    conn.query_row(
        "SELECT COUNT(*) FROM symbol_imports WHERE branch_id = ?1 AND importer_file = ?2",
        rusqlite::params![branch, importer],
        |row| row.get::<_, i64>(0),
    )
    .expect("count imports")
}

fn definition_names(db: &Database, branch: &str, file_path: &str) -> Vec<String> {
    let conn = db.connection().lock().expect("lock conn");
    let mut stmt = conn
        .prepare(
            "SELECT symbol_name FROM symbol_definitions
             WHERE branch_id = ?1 AND file_path = ?2 ORDER BY symbol_name",
        )
        .unwrap();
    let rows = stmt
        .query_map(rusqlite::params![branch, file_path], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// AC #1: modifying a file with `pub fn foo()` produces a `symbol_definitions`
/// row for `foo` after the hot tier processes the event.
#[test]
fn hot_tier_change_populates_symbol_index() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn foo() -> u32 { 42 }\n").unwrap();

    let db = Database::open(":memory:").expect("in-memory DB");
    let conn = db.connection().clone();
    let branch = BranchId::from("main");

    process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
        .expect("file change processed");

    let names = definition_names(&db, &branch.0, "lib.rs");
    assert!(
        names.contains(&"foo".to_string()),
        "symbol_definitions should include `foo`, got {names:?}",
    );
}

/// AC #1: imports are persisted alongside definitions on the modify path.
/// `use std::io::Read;` should produce one `symbol_imports` row with
/// `imported_name = "Read"` (defining-name, per US-002).
#[test]
fn hot_tier_change_populates_symbol_imports() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("user.rs");
    fs::write(&file, "use std::io::Read;\n\npub fn read_all() {}\n").unwrap();

    let db = Database::open(":memory:").expect("in-memory DB");
    let conn = db.connection().clone();
    let branch = BranchId::from("main");

    process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
        .expect("file change processed");

    let import_count = count_imports(&db, &branch.0, "user.rs");
    assert!(
        import_count >= 1,
        "expected at least one symbol_imports row for user.rs, got {import_count}",
    );

    // The defining name (`Read`) — not the path `std::io::Read` or any alias —
    // is what gets stored. See US-002 notes on parser convention.
    let conn = db.connection().lock().expect("lock conn");
    let names: Vec<String> = conn
        .prepare(
            "SELECT imported_name FROM symbol_imports
             WHERE branch_id = ?1 AND importer_file = ?2",
        )
        .unwrap()
        .query_map(rusqlite::params![branch.0, "user.rs"], |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        names.contains(&"Read".to_string()),
        "symbol_imports should include defining-name `Read`, got {names:?}",
    );
}

/// AC #1: re-processing an edited file replaces the previous symbol-index
/// rows rather than accumulating them. Removing `foo` should drop its row.
#[test]
fn hot_tier_change_replaces_existing_symbol_rows() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn foo() {}\npub fn bar() {}\n").unwrap();

    let db = Database::open(":memory:").expect("in-memory DB");
    let conn = db.connection().clone();
    let branch = BranchId::from("main");

    process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
        .expect("first change");
    let names = definition_names(&db, &branch.0, "lib.rs");
    assert!(names.contains(&"foo".to_string()));
    assert!(names.contains(&"bar".to_string()));

    // Edit out `foo`; only `bar` should remain after the second event.
    fs::write(&file, "pub fn bar() {}\n").unwrap();
    process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
        .expect("second change");

    let names = definition_names(&db, &branch.0, "lib.rs");
    assert!(
        !names.contains(&"foo".to_string()),
        "row for removed function `foo` must be gone, got {names:?}",
    );
    assert!(
        names.contains(&"bar".to_string()),
        "remaining function `bar` must still be present, got {names:?}",
    );
}

/// AC #2: deleting a file drops every symbol-index row for that file.
#[test]
fn hot_tier_delete_removes_symbol_rows() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("doomed.rs");
    fs::write(
        &file,
        "use std::io;\n\npub fn doomed_fn() {}\npub struct DoomedType;\n",
    )
    .unwrap();

    let db = Database::open(":memory:").expect("in-memory DB");
    let conn = db.connection().clone();
    let branch = BranchId::from("main");

    process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
        .expect("file change");
    assert!(count_definitions(&db, &branch.0, "doomed.rs") >= 2);
    assert!(count_imports(&db, &branch.0, "doomed.rs") >= 1);

    process_file_delete(&file, dir.path(), &conn, &branch).expect("file delete");

    assert_eq!(
        count_definitions(&db, &branch.0, "doomed.rs"),
        0,
        "symbol_definitions for deleted file must be empty",
    );
    assert_eq!(
        count_imports(&db, &branch.0, "doomed.rs"),
        0,
        "symbol_imports for deleted file must be empty",
    );
}

/// Regression cover for "modify file with `pub fn foo()` → row exists; remove
/// the function → row is gone" exactly as worded in US-003 AC #4. Combined
/// flow keeps the contract honest end-to-end through the watcher entry points.
#[test]
fn hot_tier_modify_then_remove_function_drops_its_row() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("lib.rs");

    fs::write(&file, "pub fn foo() -> u32 { 42 }\n").unwrap();

    let db = Database::open(":memory:").expect("in-memory DB");
    let conn = db.connection().clone();
    let branch = BranchId::from("main");

    process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
        .expect("initial change");
    assert!(
        definition_names(&db, &branch.0, "lib.rs").contains(&"foo".to_string()),
        "row for foo must exist after the initial change event",
    );

    // Remove the function (file still exists, just no `foo`).
    fs::write(&file, "// foo removed\n").unwrap();
    process_file_change(&file, dir.path(), &conn, &branch, &ScanConfig::default())
        .expect("modify event");

    assert!(
        !definition_names(&db, &branch.0, "lib.rs").contains(&"foo".to_string()),
        "row for foo must be gone after the function is removed from the file",
    );
}
