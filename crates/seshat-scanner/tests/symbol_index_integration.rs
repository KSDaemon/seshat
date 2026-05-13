//! Integration tests for US-002: scanner populates the symbol-index
//! tables (`symbol_definitions`, `symbol_imports`) alongside `files_ir`
//! on every scan.
//!
//! These tests exercise the full `scan_project` path so the contract
//! between the orchestrator and `FileIRRepository::upsert_with_symbol_index`
//! is locked end-to-end — not just the storage helper.

use std::collections::HashSet;
use std::fs;

use seshat_core::{BranchId, ScanConfig};
use seshat_scanner::scan_project;
use seshat_storage::{
    Database, SqliteSymbolIndexRepository, SymbolIndexRepository, extract_definitions,
    extract_imports,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn count_rows(db: &Database, sql: &str, branch: &str) -> i64 {
    let conn = db.connection().lock().expect("lock conn");
    conn.query_row(sql, [branch], |row| row.get::<_, i64>(0))
        .expect("count query")
}

/// All `(branch_id, imported_name)` pairs in `symbol_imports`.
fn all_imported_names(db: &Database, branch: &str) -> Vec<String> {
    let conn = db.connection().lock().expect("lock conn");
    let mut stmt = conn
        .prepare(
            "SELECT imported_name FROM symbol_imports WHERE branch_id = ?1 ORDER BY imported_name",
        )
        .unwrap();
    let rows = stmt
        .query_map([branch], |row| row.get::<_, String>(0))
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

/// All `(symbol_name, kind, is_public)` tuples in `symbol_definitions`.
fn all_definitions(db: &Database, branch: &str) -> Vec<(String, String, bool)> {
    let conn = db.connection().lock().expect("lock conn");
    let mut stmt = conn
        .prepare(
            "SELECT symbol_name, kind, is_public FROM symbol_definitions
             WHERE branch_id = ?1 ORDER BY symbol_name, kind",
        )
        .unwrap();
    let rows = stmt
        .query_map([branch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn create_multi_lang_fixture() -> tempfile::TempDir {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    let rs_src = root.join("src");
    fs::create_dir_all(&rs_src).unwrap();

    fs::write(
        rs_src.join("lib.rs"),
        r#"pub fn hello() -> &'static str { "hi" }

pub struct Config {
    pub debug: bool,
}

fn private_helper() {}
"#,
    )
    .unwrap();

    fs::write(
        rs_src.join("main.rs"),
        r#"use crate::lib::hello;
use std::io;

pub fn run() {
    println!("{}", hello());
}
"#,
    )
    .unwrap();

    let ts_src = root.join("frontend").join("src");
    fs::create_dir_all(&ts_src).unwrap();
    fs::write(
        ts_src.join("index.ts"),
        r#"import { hello } from './lib';
import type { Config } from './types';

export function start(): void {
    console.log(hello());
}

export const VERSION = '1.0';
"#,
    )
    .unwrap();

    dir
}

/// Fixture with both wildcard and aliased imports across Rust, Python, and TS.
fn create_aliased_wildcard_fixture() -> tempfile::TempDir {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    // Rust: wildcard, aliased single, aliased inside use_list.
    let rs_src = root.join("src");
    fs::create_dir_all(&rs_src).unwrap();
    fs::write(
        rs_src.join("main.rs"),
        r#"use std::collections::*;
use std::io::Read as MyRead;
use crate::models::{User as MyUser, Address};
use anyhow as ah;

pub fn main() {}
"#,
    )
    .unwrap();

    // Python: wildcard + multiple aliased imports.
    let py_src = root.join("pysrc");
    fs::create_dir_all(&py_src).unwrap();
    fs::write(
        py_src.join("app.py"),
        r#"import numpy as np
from os.path import join as path_join, basename
from typing import *
from collections import OrderedDict as OD

def main():
    pass
"#,
    )
    .unwrap();

    // TypeScript: namespace wildcard + aliased named imports.
    let ts_src = root.join("ts");
    fs::create_dir_all(&ts_src).unwrap();
    fs::write(
        ts_src.join("app.ts"),
        r#"import * as ns from './ns';
import { foo as fooRenamed, bar } from './lib';

export function run(): void {}
"#,
    )
    .unwrap();

    dir
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn full_scan_populates_symbol_definitions_and_imports() {
    let dir = create_multi_lang_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("scan");

    let def_count = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_definitions WHERE branch_id = ?1",
        &branch.0,
    );
    let imp_count = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_imports WHERE branch_id = ?1",
        &branch.0,
    );

    assert!(
        def_count > 0,
        "symbol_definitions must be populated after a full scan, got {def_count}"
    );
    assert!(
        imp_count > 0,
        "symbol_imports must be populated after a full scan, got {imp_count}"
    );

    // Verify concrete names show up so we know the right rows were inserted.
    let defs = all_definitions(&db, &branch.0);
    let def_names: HashSet<&str> = defs.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(def_names.contains("hello"), "hello function indexed");
    assert!(def_names.contains("Config"), "Config type indexed");
    assert!(def_names.contains("run"), "run function indexed");
    assert!(def_names.contains("start"), "TS start function indexed");
    assert!(def_names.contains("VERSION"), "TS VERSION export indexed");
}

#[test]
fn full_scan_matches_extract_helpers_row_counts() {
    // Row counts in the DB must exactly equal the per-file extraction
    // helpers — i.e. the scanner did not silently drop / duplicate anything.
    let dir = create_multi_lang_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("scan");

    let file_ir_repo = seshat_storage::SqliteFileIRRepository::new(db.connection().clone());
    use seshat_storage::FileIRRepository;
    let files = file_ir_repo.get_by_branch(&branch).expect("get files");

    let expected_defs: usize = files.iter().map(|f| extract_definitions(f).len()).sum();
    let expected_imps: usize = files.iter().map(|f| extract_imports(f).len()).sum();

    let actual_defs = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_definitions WHERE branch_id = ?1",
        &branch.0,
    ) as usize;
    let actual_imps = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_imports WHERE branch_id = ?1",
        &branch.0,
    ) as usize;

    assert_eq!(
        actual_defs, expected_defs,
        "symbol_definitions row count must match extract_definitions over every file"
    );
    assert_eq!(
        actual_imps, expected_imps,
        "symbol_imports row count must match extract_imports over every file"
    );
}

#[test]
fn re_scan_is_idempotent() {
    let dir = create_multi_lang_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("first scan");
    let def1 = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_definitions WHERE branch_id = ?1",
        &branch.0,
    );
    let imp1 = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_imports WHERE branch_id = ?1",
        &branch.0,
    );

    scan_project(dir.path(), &config, &db, branch.clone()).expect("second scan");
    let def2 = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_definitions WHERE branch_id = ?1",
        &branch.0,
    );
    let imp2 = count_rows(
        &db,
        "SELECT COUNT(*) FROM symbol_imports WHERE branch_id = ?1",
        &branch.0,
    );

    assert_eq!(def1, def2, "definitions count stable across re-scan");
    assert_eq!(imp1, imp2, "imports count stable across re-scan");
}

#[test]
fn wildcard_imports_produce_zero_rows() {
    let dir = create_aliased_wildcard_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("scan");

    let names = all_imported_names(&db, &branch.0);
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();

    assert!(
        !name_set.contains("*"),
        "wildcard `*` imports must not appear in symbol_imports, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("* as ")),
        "namespace wildcard imports (`* as ns`) must not appear in symbol_imports — got {names:?}"
    );
    assert!(
        !name_set.contains("ns"),
        "namespace alias `ns` from `import * as ns` must not appear, got {names:?}"
    );
}

#[test]
fn rust_aliased_imports_store_defining_name() {
    let dir = create_aliased_wildcard_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("scan");

    let names = all_imported_names(&db, &branch.0);
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();

    // `use std::io::Read as MyRead;` (top-level use_as_clause)
    assert!(
        name_set.contains("Read"),
        "top-level `Read as MyRead` must store defining name `Read`, got {names:?}"
    );
    assert!(
        !name_set.contains("MyRead"),
        "top-level `Read as MyRead` must NOT store alias `MyRead`, got {names:?}"
    );

    // `use crate::models::{User as MyUser, Address};` (use_list inner)
    assert!(
        name_set.contains("User"),
        "inner `User as MyUser` must store defining name `User`, got {names:?}"
    );
    assert!(
        !name_set.contains("MyUser"),
        "inner alias `MyUser` must not be stored, got {names:?}"
    );

    // `use anyhow as ah;` (no `::` separator)
    assert!(
        name_set.contains("anyhow"),
        "`use anyhow as ah` must store defining name `anyhow`, got {names:?}"
    );
    assert!(
        !name_set.contains("ah"),
        "`use anyhow as ah` must NOT store alias `ah`, got {names:?}"
    );
}

#[test]
fn python_aliased_imports_store_defining_name() {
    let dir = create_aliased_wildcard_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("scan");

    let names = all_imported_names(&db, &branch.0);
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();

    // `import numpy as np`
    assert!(
        name_set.contains("numpy"),
        "`import numpy as np` must store `numpy`, got {names:?}"
    );
    assert!(
        !name_set.contains("np"),
        "alias `np` must not appear, got {names:?}"
    );

    // `from os.path import join as path_join, basename`
    assert!(
        name_set.contains("join"),
        "`join as path_join` must store `join`, got {names:?}"
    );
    assert!(
        name_set.contains("basename"),
        "non-aliased `basename` still indexed, got {names:?}"
    );
    assert!(
        !name_set.contains("path_join"),
        "alias `path_join` must not appear, got {names:?}"
    );

    // `from collections import OrderedDict as OD`
    assert!(
        name_set.contains("OrderedDict"),
        "`OrderedDict as OD` must store `OrderedDict`, got {names:?}"
    );
    assert!(
        !name_set.contains("OD"),
        "alias `OD` must not appear, got {names:?}"
    );

    // `from typing import *` is a wildcard — must not appear.
    // (Already covered by the wildcard test, but check here for clarity.)
    assert!(
        !names.contains(&"*".to_string()),
        "wildcard `from typing import *` must be filtered out, got {names:?}"
    );
}

#[test]
fn ts_aliased_imports_store_defining_name() {
    let dir = create_aliased_wildcard_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("scan");

    let names = all_imported_names(&db, &branch.0);
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();

    // `import { foo as fooRenamed, bar } from './lib';`
    assert!(
        name_set.contains("foo"),
        "`foo as fooRenamed` must store `foo`, got {names:?}"
    );
    assert!(
        name_set.contains("bar"),
        "plain `bar` import indexed, got {names:?}"
    );
    assert!(
        !name_set.contains("fooRenamed"),
        "alias `fooRenamed` must not appear, got {names:?}"
    );
}

#[test]
fn deleting_a_file_removes_its_symbol_index_rows() {
    let dir = create_multi_lang_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();
    let branch = BranchId::from("main");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("first scan");

    let repo = SqliteSymbolIndexRepository::new(db.connection().clone());
    let def_total_before = repo.count_definitions(&branch).unwrap();

    // Delete one of the indexed files on disk.
    fs::remove_file(dir.path().join("src").join("lib.rs")).expect("remove lib.rs");

    scan_project(dir.path(), &config, &db, branch.clone()).expect("re-scan");

    // Make sure no defs/imports remain pointing at the deleted file.
    let conn = db.connection().lock().unwrap();
    let leftover: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbol_definitions
             WHERE branch_id = ?1 AND file_path = ?2",
            ["main", "src/lib.rs"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        leftover, 0,
        "deleted file must have no symbol_definitions rows left"
    );

    let leftover_imp: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbol_imports
             WHERE branch_id = ?1 AND importer_file = ?2",
            ["main", "src/lib.rs"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        leftover_imp, 0,
        "deleted file must have no symbol_imports rows left"
    );

    drop(conn);
    let def_total_after = repo.count_definitions(&branch).unwrap();
    assert!(
        def_total_after < def_total_before,
        "total definitions must shrink after deleting an indexed file (before={def_total_before}, after={def_total_after})"
    );
}
