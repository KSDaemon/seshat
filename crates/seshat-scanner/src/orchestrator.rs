//! Scan orchestration — full project scan pipeline.
//!
//! Coordinates file discovery, parsing, module structure analysis,
//! manifest analysis, documentation ingestion, and persistence of all
//! results to the database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use seshat_core::{BranchId, Edge, EdgeId, NodeId, ProjectFile, ScanConfig};
use seshat_storage::{
    Database, EdgeRepository, FileIRRepository, NodeRepository, SqliteEdgeRepository,
    SqliteFileIRRepository, SqliteNodeRepository,
};

use crate::discovery::discover_files;
use crate::documentation::parse_documentation;
use crate::error::ScanError;
use crate::manifest::{ManifestType, analyze_manifests};
use crate::module_structure::build_module_graph;
use crate::parser::parse_file;

/// Summary of a completed scan operation.
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// Number of source files discovered.
    pub files_discovered: usize,
    /// Number of source files parsed (may differ from discovered if some were skipped).
    pub files_parsed: usize,
    /// Number of knowledge nodes persisted.
    pub nodes_persisted: usize,
    /// Number of edges persisted.
    pub edges_persisted: usize,
    /// Number of manifest files analyzed.
    pub manifests_analyzed: usize,
    /// Number of documentation files ingested.
    pub docs_ingested: usize,
}

/// Orchestrate a full project scan.
///
/// Pipeline:
/// 1. Discover source files (respecting `.gitignore`, size limits, patterns)
/// 2. Read and parse each file into [`ProjectFile`] IR
/// 3. Store each file's IR in the `files_ir` table
/// 4. Build module structure graph from parsed files
/// 5. Discover and analyze dependency manifests
/// 6. Discover and parse documentation files
/// 7. Persist all knowledge nodes and edges to the database
///
/// # Arguments
///
/// * `root` - The project root directory to scan.
/// * `config` - Scan configuration (exclude patterns, file size limit).
/// * `db` - The database handle for persistence.
///
/// # Returns
///
/// A [`ScanResult`] summarizing what was persisted.
pub fn scan_project(
    root: &Path,
    config: &ScanConfig,
    db: &Database,
) -> Result<ScanResult, ScanError> {
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let node_repo = SqliteNodeRepository::new(conn.clone());
    let edge_repo = SqliteEdgeRepository::new(conn);

    let branch_id = BranchId::from("main");

    // ------------------------------------------------------------------
    // Step 1: Discover source files
    // ------------------------------------------------------------------
    let discovered = discover_files(root, config)?;
    let files_discovered = discovered.len();
    tracing::info!(count = files_discovered, "Discovered source files");

    // ------------------------------------------------------------------
    // Step 2: Read & parse each file
    // ------------------------------------------------------------------
    let mut parsed_files: Vec<ProjectFile> = Vec::with_capacity(files_discovered);

    for df in &discovered {
        let source = match std::fs::read_to_string(&df.path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %df.path.display(), error = %e, "Failed to read file, skipping");
                continue;
            }
        };

        let project_file = parse_file(&df.path, &source, df.language);
        parsed_files.push(project_file);
    }

    let files_parsed = parsed_files.len();
    tracing::info!(count = files_parsed, "Parsed source files");

    // ------------------------------------------------------------------
    // Step 3: Persist file IR
    // ------------------------------------------------------------------
    for pf in &parsed_files {
        file_ir_repo.upsert(&branch_id, pf)?;
    }
    tracing::info!(count = files_parsed, "Stored file IR records");

    // ------------------------------------------------------------------
    // Step 4: Build module structure graph
    // ------------------------------------------------------------------
    let module_graph = build_module_graph(root, &parsed_files, &branch_id);

    // Persist module nodes. The module graph assigns placeholder IDs
    // (sequential from 1), but the DB assigns real IDs. We need a
    // mapping from placeholder → real ID for edge remapping.
    let mut id_remap: HashMap<NodeId, NodeId> = HashMap::new();
    let mut nodes_persisted: usize = 0;

    for node in &module_graph.nodes {
        let inserted = node_repo.insert(node)?;
        id_remap.insert(node.id, inserted.id);
        nodes_persisted += 1;
    }

    // Persist module edges with remapped source/target IDs.
    let mut edges_persisted: usize = 0;

    for edge in &module_graph.edges {
        let remapped_edge = remap_edge(edge, &id_remap);
        edge_repo.insert(&remapped_edge)?;
        edges_persisted += 1;
    }

    tracing::info!(
        nodes = nodes_persisted,
        edges = edges_persisted,
        "Persisted module structure"
    );

    // ------------------------------------------------------------------
    // Step 5: Discover and analyze dependency manifests
    // ------------------------------------------------------------------
    let manifests = discover_manifests(root)?;
    let manifests_analyzed = manifests.len();

    if !manifests.is_empty() {
        let analysis = analyze_manifests(&manifests, &parsed_files)?;
        tracing::info!(count = analysis.len(), "Analyzed dependency manifests");
    }

    // ------------------------------------------------------------------
    // Step 6: Discover and parse documentation files
    // ------------------------------------------------------------------
    let doc_files = discover_documentation(root)?;
    let docs_ingested = doc_files.len();

    for (doc_path, doc_content) in &doc_files {
        match parse_documentation(doc_path, doc_content, &branch_id) {
            Ok(doc_result) => {
                for node in &doc_result.nodes {
                    node_repo.insert(node)?;
                    nodes_persisted += 1;
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %doc_path.display(),
                    error = %e,
                    "Failed to parse documentation, skipping"
                );
            }
        }
    }

    tracing::info!(
        count = docs_ingested,
        nodes = nodes_persisted,
        "Ingested documentation"
    );

    Ok(ScanResult {
        files_discovered,
        files_parsed,
        nodes_persisted,
        edges_persisted,
        manifests_analyzed,
        docs_ingested,
    })
}

/// Remap an edge's source and target IDs using the placeholder → real ID mapping.
///
/// If an ID is not found in the mapping (shouldn't happen in normal flow),
/// the original ID is preserved.
fn remap_edge(edge: &Edge, id_remap: &HashMap<NodeId, NodeId>) -> Edge {
    Edge {
        id: EdgeId(0), // DB will assign real ID
        source_id: id_remap
            .get(&edge.source_id)
            .copied()
            .unwrap_or(edge.source_id),
        target_id: id_remap
            .get(&edge.target_id)
            .copied()
            .unwrap_or(edge.target_id),
        edge_type: edge.edge_type,
        branch_id: edge.branch_id.clone(),
        weight: edge.weight,
        metadata: edge.metadata.clone(),
    }
}

/// Discover dependency manifest files in the project root directory.
///
/// Looks for known manifest filenames (`Cargo.toml`, `package.json`,
/// `pyproject.toml`) in the root directory only (not recursively).
fn discover_manifests(root: &Path) -> Result<Vec<(PathBuf, String, ManifestType)>, ScanError> {
    let mut manifests = Vec::new();

    for filename in ManifestType::all_filenames() {
        let path = root.join(filename);
        if path.is_file() {
            let content = std::fs::read_to_string(&path).map_err(|e| ScanError::ManifestError {
                path: path.clone(),
                reason: format!("Failed to read manifest: {e}"),
            })?;

            if let Some(manifest_type) = ManifestType::from_filename(filename) {
                manifests.push((path, content, manifest_type));
            }
        }
    }

    Ok(manifests)
}

/// Discover documentation files in the project.
///
/// Walks the project directory looking for `.md`, `.json`, `.yaml`, `.yml`
/// files. Returns relative paths and their contents.
fn discover_documentation(root: &Path) -> Result<Vec<(PathBuf, String)>, ScanError> {
    let mut doc_files = Vec::new();
    let doc_extensions = ["md", "json", "yaml", "yml"];

    // Walk the root directory looking for documentation files.
    // Use a simple recursive walk since documentation files are typically
    // not in deeply nested structures and we need to check extensions.
    walk_for_docs(root, root, &doc_extensions, &mut doc_files)?;

    Ok(doc_files)
}

/// Recursively walk directories for documentation files.
fn walk_for_docs(
    current: &Path,
    root: &Path,
    extensions: &[&str],
    results: &mut Vec<(PathBuf, String)>,
) -> Result<(), ScanError> {
    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(path = %current.display(), error = %e, "Cannot read directory");
            return Ok(());
        }
    };

    for entry in entries {
        let entry = entry.map_err(ScanError::Io)?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Skip hidden files/directories and common non-doc directories.
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "__pycache__"
        {
            continue;
        }

        if path.is_dir() {
            walk_for_docs(&path, root, extensions, results)?;
        } else if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

            if extensions.contains(&ext) {
                // Validate it's actually a documentation file
                // (JSON must be a schema, YAML/YML must be OpenAPI)
                let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();

                // For JSON and YAML, only include if they match doc type detection
                if ext == "json" || ext == "yaml" || ext == "yml" {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        // Try to parse — if it's not a valid doc format, skip
                        if is_documentation_content(ext, &content) {
                            results.push((relative, content));
                        }
                    }
                } else {
                    // Markdown files are always documentation
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        results.push((relative, content));
                    }
                }
            }
        }
    }

    Ok(())
}

/// Check if file content matches a known documentation format.
///
/// JSON files must look like a JSON Schema (have `$schema`, `properties`, or
/// `type` + `title`). YAML files must have `openapi` or `swagger` top-level key.
fn is_documentation_content(ext: &str, content: &str) -> bool {
    match ext {
        "json" => {
            // Check for JSON Schema indicators
            let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
                return false;
            };
            let obj = match value.as_object() {
                Some(o) => o,
                None => return false,
            };
            obj.contains_key("$schema")
                || obj.contains_key("properties")
                || (obj.contains_key("type") && obj.contains_key("title"))
        }
        "yaml" | "yml" => {
            // Check for OpenAPI/Swagger indicators
            let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(content) else {
                return false;
            };
            let mapping = match value.as_mapping() {
                Some(m) => m,
                None => return false,
            };
            let has_openapi =
                mapping.contains_key(serde_yaml::Value::String("openapi".to_string()));
            let has_swagger =
                mapping.contains_key(serde_yaml::Value::String("swagger".to_string()));
            has_openapi || has_swagger
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ScanConfig;
    use seshat_storage::Database;
    use std::fs;
    use tempfile::tempdir;

    /// Helper: create a minimal project in a temp directory for testing.
    fn create_test_project() -> tempfile::TempDir {
        let dir = tempdir().expect("create tempdir");
        let root = dir.path();

        // Create .git directory so WalkBuilder activates .gitignore parsing
        fs::create_dir_all(root.join(".git")).unwrap();

        // Create source files
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();

        fs::write(
            src.join("main.rs"),
            r#"
use std::io;
use crate::config::Config;

pub fn main() {
    println!("hello");
}

fn helper() -> bool {
    true
}
"#,
        )
        .unwrap();

        fs::write(
            src.join("config.rs"),
            r#"
pub struct Config {
    pub name: String,
    pub debug: bool,
}

impl Config {
    pub fn new() -> Self {
        Config {
            name: String::new(),
            debug: false,
        }
    }
}
"#,
        )
        .unwrap();

        // Create a subdirectory with another file
        let utils = src.join("utils");
        fs::create_dir_all(&utils).unwrap();

        fs::write(
            utils.join("format.rs"),
            r#"
use crate::config::Config;

pub fn format_name(config: &Config) -> String {
    config.name.clone()
}
"#,
        )
        .unwrap();

        // Create a markdown doc
        fs::write(
            root.join("README.md"),
            r#"# Test Project

## Overview
A simple test project.

## Features
- Feature one
- Feature two
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn scan_project_discovers_and_parses_files() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        let result = scan_project(root, &config, &db).expect("scan should succeed");

        assert_eq!(result.files_discovered, 3, "should discover 3 .rs files");
        assert_eq!(result.files_parsed, 3, "should parse all 3 files");
    }

    #[test]
    fn scan_project_stores_ir_in_database() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        scan_project(root, &config, &db).expect("scan should succeed");

        // Verify IR records exist in database
        let conn = db.connection().clone();
        let file_ir_repo = SqliteFileIRRepository::new(conn);
        let branch_id = BranchId::from("main");

        let all_files = file_ir_repo.get_by_branch(&branch_id).expect("get files");
        assert_eq!(all_files.len(), 3, "should have 3 file IR records");
    }

    #[test]
    fn scan_project_stores_content_hash() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        scan_project(root, &config, &db).expect("scan should succeed");

        // Verify content hashes are stored
        let conn = db.connection().clone();
        let file_ir_repo = SqliteFileIRRepository::new(conn);
        let branch_id = BranchId::from("main");

        let all_files = file_ir_repo.get_by_branch(&branch_id).expect("get files");
        for pf in &all_files {
            assert!(
                !pf.content_hash.is_empty(),
                "content hash should be non-empty for {}",
                pf.path.display()
            );
        }
    }

    #[test]
    fn scan_project_persists_module_nodes() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        let result = scan_project(root, &config, &db).expect("scan should succeed");

        // We have files in src/ and src/utils/, so should have at least 2 module nodes
        assert!(
            result.nodes_persisted >= 2,
            "should persist at least 2 module nodes, got {}",
            result.nodes_persisted
        );

        // Verify nodes exist in DB
        let conn = db.connection().clone();
        let node_repo = SqliteNodeRepository::new(conn);
        let branch_id = BranchId::from("main");

        let nodes = node_repo.find_by_branch(&branch_id).expect("find nodes");
        assert!(
            nodes.len() >= 2,
            "should have at least 2 nodes in DB, got {}",
            nodes.len()
        );
    }

    #[test]
    fn scan_project_persists_edges() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        let result = scan_project(root, &config, &db).expect("scan should succeed");

        // Should have PartOf edges at least (src/utils PartOf src)
        assert!(
            result.edges_persisted >= 1,
            "should persist at least 1 edge, got {}",
            result.edges_persisted
        );

        // Verify edges exist in DB
        let conn = db.connection().clone();
        let edge_repo = SqliteEdgeRepository::new(conn);

        let part_of_edges = edge_repo
            .find_by_type(seshat_core::EdgeType::PartOf)
            .expect("find PartOf edges");
        assert!(
            !part_of_edges.is_empty(),
            "should have at least 1 PartOf edge"
        );
    }

    #[test]
    fn scan_project_ingests_documentation() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        let result = scan_project(root, &config, &db).expect("scan should succeed");

        assert!(
            result.docs_ingested >= 1,
            "should ingest at least 1 documentation file (README.md), got {}",
            result.docs_ingested
        );
    }

    #[test]
    fn scan_project_empty_directory() {
        let dir = tempdir().expect("create tempdir");
        let root = dir.path();

        // Create .git so WalkBuilder works
        fs::create_dir_all(root.join(".git")).unwrap();

        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        let result =
            scan_project(root, &config, &db).expect("scan should succeed on empty project");

        assert_eq!(result.files_discovered, 0);
        assert_eq!(result.files_parsed, 0);
        assert_eq!(result.nodes_persisted, 0);
        assert_eq!(result.edges_persisted, 0);
    }

    #[test]
    fn scan_project_respects_config_exclude_patterns() {
        let dir = create_test_project();
        let root = dir.path();

        // Exclude utils/ directory
        let config = ScanConfig {
            exclude_patterns: vec!["**/utils/**".to_string()],
            ..ScanConfig::default()
        };

        let db = Database::open(":memory:").expect("open DB");

        let result = scan_project(root, &config, &db).expect("scan should succeed");

        // Should only discover main.rs and config.rs (not utils/format.rs)
        assert_eq!(
            result.files_discovered, 2,
            "should discover 2 files (utils excluded)"
        );
    }

    #[test]
    fn discover_manifests_finds_cargo_toml() {
        let dir = tempdir().expect("create tempdir");
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "test"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();

        let manifests = discover_manifests(root).expect("discover manifests");
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].2, ManifestType::CargoToml);
    }

    #[test]
    fn discover_manifests_finds_nothing_without_manifests() {
        let dir = tempdir().expect("create tempdir");
        let manifests = discover_manifests(dir.path()).expect("discover manifests");
        assert!(manifests.is_empty());
    }

    #[test]
    fn is_documentation_content_json_schema() {
        let content = r#"{"$schema": "http://json-schema.org/draft-07/schema#", "type": "object"}"#;
        assert!(is_documentation_content("json", content));

        let content = r#"{"name": "foo", "value": 42}"#;
        assert!(!is_documentation_content("json", content));
    }

    #[test]
    fn is_documentation_content_openapi() {
        let content = "openapi: '3.0.0'\ninfo:\n  title: Test\n  version: '1.0'\npaths: {}";
        assert!(is_documentation_content("yaml", content));

        let content = "name: test\nvalue: 42";
        assert!(!is_documentation_content("yaml", content));
    }

    #[test]
    fn remap_edge_applies_id_mapping() {
        let mut remap = HashMap::new();
        remap.insert(NodeId(1), NodeId(100));
        remap.insert(NodeId(2), NodeId(200));

        let edge = Edge {
            id: EdgeId(0),
            source_id: NodeId(1),
            target_id: NodeId(2),
            edge_type: seshat_core::EdgeType::DependsOn,
            branch_id: BranchId::from("main"),
            weight: 1.0,
            metadata: None,
        };

        let remapped = remap_edge(&edge, &remap);
        assert_eq!(remapped.source_id, NodeId(100));
        assert_eq!(remapped.target_id, NodeId(200));
    }
}
