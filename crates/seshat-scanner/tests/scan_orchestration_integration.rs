//! Integration tests for the scan orchestration pipeline (US-011).
//!
//! These tests exercise the full pipeline: discover files → parse → persist
//! IR + knowledge nodes + edges to the database.

use std::fs;

use seshat_core::{BranchId, EdgeType, KnowledgeNature, Language, ScanConfig};
use seshat_scanner::scan_project;
use seshat_storage::{
    Database, EdgeRepository, FileIRRepository, NodeRepository, SqliteEdgeRepository,
    SqliteFileIRRepository, SqliteNodeRepository,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Fixture setup
// ---------------------------------------------------------------------------

/// Create a multi-file Rust project in a temp directory.
fn create_rust_fixture() -> tempfile::TempDir {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();

    // Create .git directory so WalkBuilder activates .gitignore parsing
    fs::create_dir_all(root.join(".git")).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    let models = src.join("models");
    fs::create_dir_all(&models).unwrap();
    let handlers = src.join("handlers");
    fs::create_dir_all(&handlers).unwrap();

    fs::write(
        src.join("main.rs"),
        r#"use crate::models::user::User;
use crate::handlers::api::handle_request;

pub fn main() {
    let user = User::new("test");
    handle_request(&user);
}
"#,
    )
    .unwrap();

    fs::write(
        models.join("user.rs"),
        r#"pub struct User {
    pub name: String,
}

impl User {
    pub fn new(name: &str) -> Self {
        User { name: name.to_string() }
    }
}
"#,
    )
    .unwrap();

    fs::write(
        handlers.join("api.rs"),
        r#"use crate::models::user::User;

pub fn handle_request(user: &User) {
    println!("Handling request for {}", user.name);
}

fn validate(user: &User) -> bool {
    !user.name.is_empty()
}
"#,
    )
    .unwrap();

    // Add a README
    fs::write(
        root.join("README.md"),
        r#"# Test Rust Project

## Overview
A minimal Rust project for integration testing.

## Modules
- models: Data models
- handlers: Request handlers
"#,
    )
    .unwrap();

    dir
}

/// Create a mixed-language project (Rust + TypeScript).
fn create_mixed_fixture() -> tempfile::TempDir {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Rust files
    let rs_src = root.join("src");
    fs::create_dir_all(&rs_src).unwrap();

    fs::write(
        rs_src.join("lib.rs"),
        r#"pub mod config;

pub fn init() -> bool {
    true
}
"#,
    )
    .unwrap();

    fs::write(
        rs_src.join("config.rs"),
        r#"pub struct AppConfig {
    pub port: u16,
    pub debug: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig { port: 3000, debug: false }
    }
}
"#,
    )
    .unwrap();

    // TypeScript files
    let ts_src = root.join("frontend").join("src");
    fs::create_dir_all(&ts_src).unwrap();

    fs::write(
        ts_src.join("index.ts"),
        r#"import { AppService } from './services';
import type { User } from './types';

export function main(): void {
    const service = new AppService();
    service.start();
}

export default main;
"#,
    )
    .unwrap();

    fs::write(
        ts_src.join("services.ts"),
        r#"export class AppService {
    private running: boolean = false;

    start(): void {
        this.running = true;
    }

    stop(): void {
        this.running = false;
    }
}
"#,
    )
    .unwrap();

    fs::write(
        ts_src.join("types.ts"),
        r#"export interface User {
    id: number;
    name: string;
    email: string;
}

export type UserRole = 'admin' | 'user' | 'guest';
"#,
    )
    .unwrap();

    dir
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn full_scan_rust_project_stores_ir() {
    let dir = create_rust_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(dir.path(), &config, &db, BranchId::from("main"))
        .expect("scan should succeed");

    // Verify file discovery and parsing
    assert_eq!(result.files_discovered, 3, "main.rs + user.rs + api.rs");
    assert_eq!(result.files_parsed, 3);

    // Verify IR in database
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).expect("get files");
    assert_eq!(files.len(), 3);

    // Verify each file has correct language
    for f in &files {
        assert_eq!(f.language, Language::Rust);
        assert!(!f.content_hash.is_empty());
    }
}

#[test]
fn full_scan_stores_content_hash_per_file() {
    let dir = create_rust_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    scan_project(dir.path(), &config, &db, BranchId::from("main")).expect("scan");

    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();

    // All content hashes should be unique (different file contents)
    let mut hashes: Vec<&str> = files.iter().map(|f| f.content_hash.as_str()).collect();
    hashes.sort();
    hashes.dedup();
    assert_eq!(
        hashes.len(),
        3,
        "each file should have a unique content hash"
    );
}

#[test]
fn full_scan_persists_module_nodes() {
    let dir = create_rust_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(dir.path(), &config, &db, BranchId::from("main")).expect("scan");

    // We have files in src/, src/models/, src/handlers/ => 3 module nodes (at minimum)
    // Plus documentation nodes from README.md
    assert!(
        result.nodes_persisted >= 3,
        "should persist at least 3 module nodes, got {}",
        result.nodes_persisted
    );

    // Verify nodes in DB
    let conn = db.connection().clone();
    let node_repo = SqliteNodeRepository::new(conn);
    let branch = BranchId::from("main");

    let nodes = node_repo.find_by_branch(&branch).unwrap();
    assert!(nodes.len() >= 3);

    // Check that module nodes are Fact nature
    let fact_nodes = node_repo.find_by_nature(KnowledgeNature::Fact).unwrap();
    assert!(
        fact_nodes.len() >= 3,
        "should have at least 3 Fact nodes (modules)"
    );

    // Module nodes should have ext_data with module_path
    let module_nodes: Vec<_> = fact_nodes
        .iter()
        .filter(|n| {
            n.ext_data
                .as_ref()
                .and_then(|d| d.get("source"))
                .and_then(|s| s.as_str())
                .map(|s| s == "module_structure")
                .unwrap_or(false)
        })
        .collect();
    assert!(
        module_nodes.len() >= 3,
        "should have at least 3 module_structure nodes, got {}",
        module_nodes.len()
    );
}

#[test]
fn full_scan_persists_edges() {
    let dir = create_rust_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(dir.path(), &config, &db, BranchId::from("main")).expect("scan");

    assert!(
        result.edges_persisted >= 1,
        "should persist at least 1 edge"
    );

    let conn = db.connection().clone();
    let edge_repo = SqliteEdgeRepository::new(conn);

    // Should have PartOf edges (handlers PartOf src, models PartOf src)
    let part_of = edge_repo.find_by_type(EdgeType::PartOf).unwrap();
    assert!(
        part_of.len() >= 2,
        "should have at least 2 PartOf edges (handlers, models under src), got {}",
        part_of.len()
    );

    // All edges should reference valid node IDs
    let conn2 = db.connection().clone();
    let node_repo = SqliteNodeRepository::new(conn2);

    for edge in &part_of {
        assert!(
            node_repo.get_by_id(edge.source_id).is_ok(),
            "edge source_id {} should reference a valid node",
            edge.source_id.0
        );
        assert!(
            node_repo.get_by_id(edge.target_id).is_ok(),
            "edge target_id {} should reference a valid node",
            edge.target_id.0
        );
    }
}

#[test]
fn full_scan_edge_ids_are_remapped_correctly() {
    let dir = create_rust_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    scan_project(dir.path(), &config, &db, BranchId::from("main")).expect("scan");

    let conn = db.connection().clone();
    let edge_repo = SqliteEdgeRepository::new(conn.clone());
    let node_repo = SqliteNodeRepository::new(conn);
    let branch = BranchId::from("main");

    // Get all nodes and edges
    let nodes = node_repo.find_by_branch(&branch).unwrap();
    let node_ids: Vec<i64> = nodes.iter().map(|n| n.id.0).collect();

    // All edge source/target IDs must refer to nodes that actually exist
    let all_edges = {
        let mut edges = Vec::new();
        edges.extend(edge_repo.find_by_type(EdgeType::PartOf).unwrap());
        edges.extend(edge_repo.find_by_type(EdgeType::DependsOn).unwrap());
        edges
    };

    for edge in &all_edges {
        assert!(
            node_ids.contains(&edge.source_id.0),
            "Edge source_id {} not found in node IDs {:?}",
            edge.source_id.0,
            node_ids
        );
        assert!(
            node_ids.contains(&edge.target_id.0),
            "Edge target_id {} not found in node IDs {:?}",
            edge.target_id.0,
            node_ids
        );
    }
}

#[test]
fn full_scan_mixed_language_project() {
    let dir = create_mixed_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(dir.path(), &config, &db, BranchId::from("main")).expect("scan");

    // 2 Rust files + 3 TypeScript files = 5 total
    assert_eq!(result.files_discovered, 5);
    assert_eq!(result.files_parsed, 5);

    // Verify languages in DB
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();
    let rust_files: Vec<_> = files
        .iter()
        .filter(|f| f.language == Language::Rust)
        .collect();
    let ts_files: Vec<_> = files
        .iter()
        .filter(|f| f.language == Language::TypeScript)
        .collect();

    assert_eq!(rust_files.len(), 2, "should have 2 Rust files");
    assert_eq!(ts_files.len(), 3, "should have 3 TypeScript files");
}

#[test]
fn full_scan_documentation_nodes_in_db() {
    let dir = create_rust_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(dir.path(), &config, &db, BranchId::from("main")).expect("scan");

    assert!(result.docs_ingested >= 1, "should ingest README.md");

    // Verify documentation nodes in DB
    let conn = db.connection().clone();
    let node_repo = SqliteNodeRepository::new(conn);
    let branch = BranchId::from("main");

    let all_nodes = node_repo.find_by_branch(&branch).unwrap();

    // Find documentation nodes (have "source": "documentation" in ext_data)
    let doc_nodes: Vec<_> = all_nodes
        .iter()
        .filter(|n| {
            n.ext_data
                .as_ref()
                .and_then(|d| d.get("source"))
                .and_then(|s| s.as_str())
                .map(|s| s == "documentation")
                .unwrap_or(false)
        })
        .collect();

    assert!(
        !doc_nodes.is_empty(),
        "should have documentation nodes from README.md"
    );
}

#[test]
fn full_scan_ir_contains_parsed_data() {
    let dir = create_rust_fixture();
    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    scan_project(dir.path(), &config, &db, BranchId::from("main")).expect("scan");

    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn);
    let branch = BranchId::from("main");

    let files = file_ir_repo.get_by_branch(&branch).unwrap();

    // Find the main.rs file and verify it has parsed imports
    let main_file = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("main.rs"))
        .expect("should find main.rs in IR");

    assert!(
        !main_file.imports.is_empty(),
        "main.rs should have parsed imports"
    );
    assert!(
        !main_file.functions.is_empty(),
        "main.rs should have parsed functions"
    );

    // Find user.rs and verify it has types
    let user_file = files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("user.rs"))
        .expect("should find user.rs in IR");

    assert!(
        !user_file.types.is_empty(),
        "user.rs should have parsed types (User struct)"
    );
}

#[test]
fn full_scan_empty_project() {
    let dir = tempdir().expect("create tempdir");
    fs::create_dir_all(dir.path().join(".git")).unwrap();

    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(dir.path(), &config, &db, BranchId::from("main"))
        .expect("should handle empty project");

    assert_eq!(result.files_discovered, 0);
    assert_eq!(result.files_parsed, 0);
    assert_eq!(result.nodes_persisted, 0);
    assert_eq!(result.edges_persisted, 0);
}

#[test]
fn full_scan_with_gitignore() {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Create .gitignore that ignores build/ directory
    fs::write(root.join(".gitignore"), "build/\n").unwrap();

    // Create source files
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.rs"), "fn main() {}").unwrap();

    // Create a file in ignored directory
    let build = root.join("build");
    fs::create_dir_all(&build).unwrap();
    fs::write(build.join("output.rs"), "fn output() {}").unwrap();

    let db = Database::open(":memory:").expect("open DB");
    let config = ScanConfig::default();

    let result = scan_project(root, &config, &db, BranchId::from("main")).expect("scan");

    // Should only discover main.rs, not build/output.rs
    assert_eq!(
        result.files_discovered, 1,
        "should only discover 1 file (build/ is gitignored)"
    );
}
