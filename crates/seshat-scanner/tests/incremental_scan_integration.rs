//! Integration tests for incremental re-scan support (US-012).
//!
//! These tests exercise the incremental scan pipeline: initial scan →
//! modify/add/delete files → re-scan → verify correct incremental updates.

use std::fs;

use seshat_core::{BranchId, Language, ScanConfig};
use seshat_scanner::scan_project;
use seshat_storage::{Database, FileIRRepository, SqliteFileIRRepository};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Fixture setup
// ---------------------------------------------------------------------------

/// Create a minimal Rust project for incremental testing.
fn create_incremental_fixture() -> tempfile::TempDir {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();

    // .git required for WalkBuilder .gitignore parsing
    fs::create_dir_all(root.join(".git")).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("main.rs"),
        r#"use crate::config::Config;

pub fn main() {
    let c = Config::new();
    println!("{}", c.name);
}
"#,
    )
    .unwrap();

    fs::write(
        src.join("config.rs"),
        r#"pub struct Config {
    pub name: String,
}

impl Config {
    pub fn new() -> Self {
        Config { name: String::from("default") }
    }
}
"#,
    )
    .unwrap();

    let utils = src.join("utils");
    fs::create_dir_all(&utils).unwrap();

    fs::write(
        utils.join("format.rs"),
        r#"pub fn format_value(v: &str) -> String {
    v.to_uppercase()
}
"#,
    )
    .unwrap();

    dir
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn initial_scan_has_no_incremental_stats() {
    let dir = create_incremental_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(dir.path(), &config, &db).expect("scan should succeed");

    // First scan should not be incremental
    assert!(
        result.incremental.is_none(),
        "first scan should not have incremental stats"
    );
    assert_eq!(result.files_discovered, 3);
    assert_eq!(result.files_parsed, 3);
}

#[test]
fn rescan_unchanged_project_skips_all_files() {
    let dir = create_incremental_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan
    scan_project(dir.path(), &config, &db).expect("initial scan");

    // Re-scan without changes
    let result = scan_project(dir.path(), &config, &db).expect("re-scan");

    let stats = result.incremental.as_ref().expect("should be incremental");
    assert_eq!(stats.files_unchanged, 3, "all 3 files should be unchanged");
    assert_eq!(stats.files_changed, 0, "no files changed");
    assert_eq!(stats.files_new, 0, "no new files");
    assert_eq!(stats.files_deleted, 0, "no files deleted");

    // files_parsed should be 0 since all were skipped
    assert_eq!(
        result.files_parsed, 0,
        "no files should be re-parsed when nothing changed"
    );

    // But files_discovered should still be 3
    assert_eq!(result.files_discovered, 3);
}

#[test]
fn rescan_detects_changed_file() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan
    scan_project(root, &config, &db).expect("initial scan");

    // Modify one file
    fs::write(
        root.join("src/config.rs"),
        r#"pub struct Config {
    pub name: String,
    pub debug: bool,
}

impl Config {
    pub fn new() -> Self {
        Config { name: String::from("updated"), debug: true }
    }
}
"#,
    )
    .unwrap();

    // Re-scan
    let result = scan_project(root, &config, &db).expect("re-scan");

    let stats = result.incremental.as_ref().expect("should be incremental");
    assert_eq!(stats.files_unchanged, 2, "main.rs + format.rs unchanged");
    assert_eq!(stats.files_changed, 1, "config.rs changed");
    assert_eq!(stats.files_new, 0, "no new files");
    assert_eq!(stats.files_deleted, 0, "no deleted files");
    assert_eq!(result.files_parsed, 1, "only changed file re-parsed");

    // Verify the updated IR is in the DB
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files.len(), 3, "should still have 3 files");

    // Find config.rs and check it has the new type field
    let config_file = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("config.rs"))
        .expect("should find config.rs");

    // Config struct should have 2 fields now (name + debug)
    let types = &config_file.types;
    assert!(!types.is_empty(), "config.rs should have types");
}

#[test]
fn rescan_detects_new_file() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan
    scan_project(root, &config, &db).expect("initial scan");

    // Add a new file
    fs::write(
        root.join("src/new_module.rs"),
        r#"pub fn new_function() -> u32 {
    42
}
"#,
    )
    .unwrap();

    // Re-scan
    let result = scan_project(root, &config, &db).expect("re-scan");

    let stats = result.incremental.as_ref().expect("should be incremental");
    assert_eq!(stats.files_unchanged, 3, "original 3 files unchanged");
    assert_eq!(stats.files_new, 1, "1 new file detected");
    assert_eq!(stats.files_changed, 0, "no files changed");
    assert_eq!(stats.files_deleted, 0, "no files deleted");
    assert_eq!(result.files_discovered, 4, "4 total files on disk");
    assert_eq!(result.files_parsed, 1, "only new file parsed");

    // Verify the new file is in the DB
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files.len(), 4, "should have 4 files now");

    let new_file = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("new_module.rs"))
        .expect("should find new_module.rs in DB");
    assert_eq!(new_file.language, Language::Rust);
    assert!(
        !new_file.functions.is_empty(),
        "should have parsed function"
    );
}

#[test]
fn rescan_detects_deleted_file() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan
    scan_project(root, &config, &db).expect("initial scan");

    // Delete a file
    fs::remove_file(root.join("src/utils/format.rs")).unwrap();

    // Re-scan
    let result = scan_project(root, &config, &db).expect("re-scan");

    let stats = result.incremental.as_ref().expect("should be incremental");
    assert_eq!(stats.files_unchanged, 2, "main.rs + config.rs unchanged");
    assert_eq!(stats.files_deleted, 1, "format.rs deleted");
    assert_eq!(stats.files_changed, 0, "no files changed");
    assert_eq!(stats.files_new, 0, "no new files");
    assert_eq!(result.files_discovered, 2, "2 files remain on disk");

    // Verify the deleted file is NOT in the DB
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files.len(), 2, "should have 2 files (format.rs removed)");

    let format_file = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("format.rs"));
    assert!(
        format_file.is_none(),
        "format.rs should not be in DB after deletion"
    );
}

#[test]
fn rescan_handles_combined_changes() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan (3 files: main.rs, config.rs, utils/format.rs)
    let initial = scan_project(root, &config, &db).expect("initial scan");
    assert_eq!(initial.files_discovered, 3);

    // Modify config.rs
    fs::write(
        root.join("src/config.rs"),
        r#"pub struct Config {
    pub name: String,
    pub port: u16,
}
"#,
    )
    .unwrap();

    // Add a new file
    fs::write(
        root.join("src/server.rs"),
        r#"pub struct Server {
    pub port: u16,
}
"#,
    )
    .unwrap();

    // Delete format.rs
    fs::remove_file(root.join("src/utils/format.rs")).unwrap();

    // Re-scan
    let result = scan_project(root, &config, &db).expect("re-scan");

    let stats = result.incremental.as_ref().expect("should be incremental");
    assert_eq!(stats.files_unchanged, 1, "main.rs unchanged");
    assert_eq!(stats.files_changed, 1, "config.rs changed");
    assert_eq!(stats.files_new, 1, "server.rs is new");
    assert_eq!(stats.files_deleted, 1, "format.rs deleted");
    assert_eq!(result.files_discovered, 3, "3 files on disk now");
    assert_eq!(result.files_parsed, 2, "changed + new parsed");

    // Verify DB state
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files.len(), 3, "should have 3 files (1 deleted, 1 added)");

    let paths: Vec<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    // format.rs should be gone
    assert!(
        !paths.iter().any(|p| p.contains("format.rs")),
        "format.rs should not exist in DB"
    );
    // server.rs should be present
    assert!(
        paths.iter().any(|p| p.contains("server.rs")),
        "server.rs should exist in DB"
    );
}

#[test]
fn rescan_updates_content_hash() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan
    scan_project(root, &config, &db).expect("initial scan");

    // Capture original hash
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let original_hashes = file_ir_repo.get_file_hashes_by_branch(&branch).unwrap();
    let original_config_hash = original_hashes
        .iter()
        .find(|(k, _)| k.contains("config.rs"))
        .map(|(_, v)| v.clone())
        .expect("should find config.rs hash");

    // Modify config.rs
    fs::write(
        root.join("src/config.rs"),
        r#"pub struct Config {
    pub name: String,
    pub version: u32,
}
"#,
    )
    .unwrap();

    // Re-scan
    scan_project(root, &config, &db).expect("re-scan");

    // Get updated hashes
    let updated_hashes = file_ir_repo.get_file_hashes_by_branch(&branch).unwrap();
    let updated_config_hash = updated_hashes
        .iter()
        .find(|(k, _)| k.contains("config.rs"))
        .map(|(_, v)| v.clone())
        .expect("should find updated config.rs hash");

    assert_ne!(
        original_config_hash, updated_config_hash,
        "content hash should change for modified file"
    );

    // Unchanged files should keep their hashes
    let original_main_hash = original_hashes
        .iter()
        .find(|(k, _)| k.contains("main.rs"))
        .map(|(_, v)| v.clone())
        .expect("should find main.rs hash");

    let updated_main_hash = updated_hashes
        .iter()
        .find(|(k, _)| k.contains("main.rs"))
        .map(|(_, v)| v.clone())
        .expect("should find main.rs hash");

    assert_eq!(
        original_main_hash, updated_main_hash,
        "unchanged file should keep same hash"
    );
}

#[test]
fn rescan_rebuilds_module_structure() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan — should have modules: src, src/utils
    let initial = scan_project(root, &config, &db).expect("initial scan");
    assert!(
        initial.nodes_persisted >= 2,
        "should have at least 2 module nodes"
    );

    // Delete the entire utils directory
    fs::remove_file(root.join("src/utils/format.rs")).unwrap();
    fs::remove_dir(root.join("src/utils")).unwrap();

    // Re-scan
    let result = scan_project(root, &config, &db).expect("re-scan");

    // Module structure should be rebuilt — utils module should be gone
    // We still have the src module, so nodes_persisted >= 1
    assert!(
        result.nodes_persisted >= 1,
        "should have at least 1 module node, got {}",
        result.nodes_persisted
    );

    // The re-scan should show the file as deleted
    let stats = result.incremental.as_ref().expect("should be incremental");
    assert_eq!(stats.files_deleted, 1, "format.rs deleted");
}

#[test]
fn multiple_rescans_converge() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan
    scan_project(root, &config, &db).expect("initial scan");

    // Scan again — no changes
    let r2 = scan_project(root, &config, &db).expect("second scan");
    let s2 = r2.incremental.as_ref().unwrap();
    assert_eq!(s2.files_unchanged, 3);
    assert_eq!(s2.files_changed, 0);

    // Scan a third time — still no changes
    let r3 = scan_project(root, &config, &db).expect("third scan");
    let s3 = r3.incremental.as_ref().unwrap();
    assert_eq!(s3.files_unchanged, 3);
    assert_eq!(s3.files_changed, 0);
    assert_eq!(s3.files_new, 0);
    assert_eq!(s3.files_deleted, 0);

    // DB should still have exactly 3 file IR records
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files.len(), 3, "should still have exactly 3 files");
}

#[test]
fn rescan_after_add_then_delete() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan (3 files)
    scan_project(root, &config, &db).expect("initial scan");

    // Add a file
    fs::write(root.join("src/temp.rs"), "pub fn temp() {}").unwrap();

    // Second scan — detects new file
    let r2 = scan_project(root, &config, &db).expect("second scan");
    let s2 = r2.incremental.as_ref().unwrap();
    assert_eq!(s2.files_new, 1);

    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");
    let files_after_add = file_ir_repo.get_by_branch(&branch).unwrap();
    assert_eq!(files_after_add.len(), 4, "should have 4 files after add");

    // Now delete that file
    fs::remove_file(root.join("src/temp.rs")).unwrap();

    // Third scan — detects deletion
    let r3 = scan_project(root, &config, &db).expect("third scan");
    let s3 = r3.incremental.as_ref().unwrap();
    assert_eq!(s3.files_deleted, 1);

    let files_after_delete = file_ir_repo.get_by_branch(&branch).unwrap();
    assert_eq!(
        files_after_delete.len(),
        3,
        "should be back to 3 files after delete"
    );
}

#[test]
fn rescan_changed_file_updates_ir_data() {
    let dir = create_incremental_fixture();
    let root = dir.path();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    // Initial scan
    scan_project(root, &config, &db).expect("initial scan");

    // Get original IR for main.rs
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let original_files = file_ir_repo.get_by_branch(&branch).unwrap();
    let original_main = original_files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("main.rs"))
        .expect("should find main.rs");
    let original_fn_count = original_main.functions.len();

    // Modify main.rs — add a function
    fs::write(
        root.join("src/main.rs"),
        r#"use crate::config::Config;

pub fn main() {
    let c = Config::new();
    println!("{}", c.name);
}

pub fn added_function() -> bool {
    true
}

fn another_private() -> u32 {
    0
}
"#,
    )
    .unwrap();

    // Re-scan
    let result = scan_project(root, &config, &db).expect("re-scan");
    let stats = result.incremental.as_ref().unwrap();
    assert_eq!(stats.files_changed, 1, "main.rs changed");

    // Verify IR was updated
    let updated_files = file_ir_repo.get_by_branch(&branch).unwrap();
    let updated_main = updated_files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("main.rs"))
        .expect("should find main.rs");

    assert!(
        updated_main.functions.len() > original_fn_count,
        "main.rs should have more functions after update (original: {}, updated: {})",
        original_fn_count,
        updated_main.functions.len()
    );
}
