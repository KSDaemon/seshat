//! Scan orchestration — full and incremental project scan pipeline.
//!
//! Coordinates file discovery, parsing, module structure analysis,
//! manifest analysis, documentation ingestion, and persistence of all
//! results to the database.
//!
//! On re-scan, unchanged files (same content hash) are skipped. Changed
//! files are re-parsed and their IR updated. New files are parsed and
//! inserted. Deleted files have their IR removed from the database.
//! Module structure (nodes + edges) is rebuilt from the full set of
//! parsed files on every scan.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use seshat_core::{BranchId, Edge, EdgeId, NodeId, ProjectFile, ScanConfig};
use seshat_storage::{
    Database, EdgeRepository, FileIRRepository, NodeRepository, SqliteEdgeRepository,
    SqliteFileIRRepository, SqliteNodeRepository,
};

use crate::discovery::discover_files;
use crate::documentation::parse_documentation;
use crate::error::ScanError;
use crate::git_dates::collect_git_file_dates;
use crate::manifest::{ManifestAnalysis, ManifestType, analyze_manifests};
use crate::module_structure::build_module_graph;
use crate::parser::{content_hash, parse_file};

/// Progress events emitted by [`scan_project`].
///
/// The callback receives these events at key pipeline stages, allowing
/// the CLI to drive progress indicators (spinner, progress bar, etc.).
#[derive(Debug, Clone)]
pub enum ScanProgress {
    /// File discovery phase: `count` files found so far.
    Discovering { count: usize },
    /// Discovery complete. `total` files will be scanned.
    DiscoveryDone { total: usize },
    /// Git history collection phase is starting.
    CollectingGitHistory,
    /// Git history collection complete.
    GitHistoryDone,
    /// A file has been processed (parsed or skipped). `done` of `total`.
    Scanning { done: usize, total: usize },
    /// Scanning (parse) phase complete.
    ScanningDone,
    /// Persisting IR and building module graph (steps 4-7).
    BuildingModuleGraph,
    /// Module graph build complete.
    ModuleGraphDone,
    /// Analyzing manifests and documentation (steps 8-9).
    AnalyzingProjectFiles,
    /// Manifest/docs analysis complete.
    ProjectFilesDone,

    // -- Submodule progress events (emitted by the scan orchestrator in US-004+) --
    /// A submodule was detected in `.gitmodules`.
    /// `path` is the relative mount path (e.g. `"vendor/lib"`).
    SubmoduleDetected { path: String },
    /// A submodule scan is starting.
    /// `path` is the relative mount path, `name` is the short directory name.
    ScanningSubmodule { path: String, name: String },
    /// A submodule scan completed successfully.
    /// `path` is the relative mount path.
    ScanningSubmoduleDone { path: String },
    /// A submodule is up-to-date (commit hash unchanged since last scan).
    /// `path` is the relative mount path, `hash` is the current commit hash.
    SubmoduleUpToDate { path: String, hash: String },
    /// A submodule was skipped (not initialized, excluded, etc.).
    /// `path` is the relative mount path, `reason` explains why.
    SubmoduleSkipped { path: String, reason: String },
}

/// No-op progress callback — used when caller does not need progress.
fn noop_progress(_: &ScanProgress) {}

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
    /// Manifest analysis results (dependency declarations + usage stats).
    pub manifest_analyses: Vec<ManifestAnalysis>,
    /// Incremental scan statistics (present on re-scans).
    pub incremental: Option<IncrementalStats>,
    /// Git file dates collected during the scan (file path → last commit timestamp).
    /// Exposed so that callers (e.g., CLI) can use them for trend computation
    /// without re-running `collect_git_file_dates()`.
    pub file_dates: HashMap<PathBuf, i64>,
    /// Submodule paths excluded from root discovery (always excluded — they get
    /// their own separate DBs). Empty when the project has no `.gitmodules`.
    pub excluded_submodules: Vec<String>,
}

/// Statistics for an incremental re-scan.
#[derive(Debug, Clone, Default)]
pub struct IncrementalStats {
    /// Files unchanged (same content hash) — skipped re-parsing.
    pub files_unchanged: usize,
    /// Files whose content changed — re-parsed and IR updated.
    pub files_changed: usize,
    /// New files not in previous scan — parsed and inserted.
    pub files_new: usize,
    /// Files deleted since last scan — IR removed from DB.
    pub files_deleted: usize,
}

/// Orchestrate a project scan with automatic incremental support.
///
/// Convenience wrapper that calls [`scan_project_with_progress`] with a
/// no-op callback.
pub fn scan_project(
    root: &Path,
    config: &ScanConfig,
    db: &Database,
) -> Result<ScanResult, ScanError> {
    scan_project_with_progress(root, config, db, noop_progress)
}

/// Orchestrate a project scan with automatic incremental support and
/// progress reporting.
///
/// If the database already contains file IR records for the branch,
/// the scan runs incrementally:
/// - Unchanged files (same content hash) are skipped
/// - Changed files are re-parsed and their IR updated
/// - New files are parsed and inserted
/// - Deleted files have their IR removed
///
/// Module structure (nodes + edges) is always rebuilt from the full set
/// of currently-valid parsed files (combining unchanged from DB + newly
/// parsed).
///
/// # Arguments
///
/// * `root` - The project root directory to scan.
/// * `config` - Scan configuration (exclude patterns, file size limit).
/// * `db` - The database handle for persistence.
/// * `on_progress` - Callback invoked at key pipeline stages.
///
/// # Returns
///
/// A [`ScanResult`] summarizing what was persisted.
pub fn scan_project_with_progress(
    root: &Path,
    config: &ScanConfig,
    db: &Database,
    on_progress: impl Fn(&ScanProgress),
) -> Result<ScanResult, ScanError> {
    let conn = db.connection().clone();
    let file_ir_repo = SqliteFileIRRepository::new(conn.clone());
    let node_repo = SqliteNodeRepository::new(conn.clone());
    let edge_repo = SqliteEdgeRepository::new(conn);

    let branch_id = BranchId::from("main");

    // ------------------------------------------------------------------
    // Step 1: Discover source files
    // ------------------------------------------------------------------
    let discovery_result = discover_files(root, config)?;
    let discovered = discovery_result.files;
    let excluded_submodules = discovery_result.excluded_submodules;
    let files_discovered = discovered.len();
    on_progress(&ScanProgress::Discovering {
        count: files_discovered,
    });
    on_progress(&ScanProgress::DiscoveryDone {
        total: files_discovered,
    });
    tracing::info!(count = files_discovered, "Discovered source files");

    // ------------------------------------------------------------------
    // Step 1b: Collect git file dates
    // ------------------------------------------------------------------
    on_progress(&ScanProgress::CollectingGitHistory);
    let git_file_dates = collect_git_file_dates(root)?;
    on_progress(&ScanProgress::GitHistoryDone);
    if !git_file_dates.is_empty() {
        tracing::info!(
            files_with_dates = git_file_dates.len(),
            "Collected git file dates"
        );
    }

    // ------------------------------------------------------------------
    // Step 2: Check for existing data (incremental mode)
    // ------------------------------------------------------------------
    let stored_hashes = file_ir_repo.get_file_hashes_by_branch(&branch_id)?;
    let is_incremental = !stored_hashes.is_empty();

    // Build a set of discovered file paths (relative, as stored in DB)
    let discovered_paths: HashSet<String> = discovered
        .iter()
        .map(|df| df.path.to_string_lossy().to_string())
        .collect();

    // ------------------------------------------------------------------
    // Step 3: Read, hash, and selectively parse files
    // ------------------------------------------------------------------
    let mut parsed_files: Vec<ProjectFile> = Vec::with_capacity(files_discovered);
    let mut incremental_stats = IncrementalStats::default();

    let mut scan_done: usize = 0;
    for df in &discovered {
        let file_path_str = df.path.to_string_lossy().to_string();

        let source = match std::fs::read_to_string(&df.path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %df.path.display(), error = %e, "Failed to read file, skipping");
                scan_done += 1;
                on_progress(&ScanProgress::Scanning {
                    done: scan_done,
                    total: files_discovered,
                });
                continue;
            }
        };

        if is_incremental {
            // Compute hash first to check if file changed
            let new_hash = content_hash(&source);

            if let Some(stored_hash) = stored_hashes.get(&file_path_str) {
                if *stored_hash == new_hash {
                    // Unchanged — skip re-parsing, load existing IR from DB
                    incremental_stats.files_unchanged += 1;
                    tracing::debug!(path = %df.path.display(), "File unchanged, skipping re-parse");
                    scan_done += 1;
                    on_progress(&ScanProgress::Scanning {
                        done: scan_done,
                        total: files_discovered,
                    });
                    continue;
                }
                // Changed — re-parse
                incremental_stats.files_changed += 1;
                tracing::debug!(path = %df.path.display(), "File changed, re-parsing");
            } else {
                // New file
                incremental_stats.files_new += 1;
                tracing::debug!(path = %df.path.display(), "New file, parsing");
            }
        }

        let project_file = parse_file(&df.path, &source, df.language);
        parsed_files.push(project_file);
        scan_done += 1;
        on_progress(&ScanProgress::Scanning {
            done: scan_done,
            total: files_discovered,
        });
    }
    on_progress(&ScanProgress::ScanningDone);

    let files_parsed = parsed_files.len();
    tracing::info!(count = files_parsed, "Parsed source files");

    on_progress(&ScanProgress::BuildingModuleGraph);

    // ------------------------------------------------------------------
    // Step 4: Handle deleted files (present in DB but not on disk)
    // ------------------------------------------------------------------
    if is_incremental {
        for stored_path in stored_hashes.keys() {
            if !discovered_paths.contains(stored_path) {
                tracing::info!(path = %stored_path, "File deleted, removing IR from DB");
                // Ignore NotFound errors (defensive)
                let _ = file_ir_repo.delete_by_path(&branch_id, stored_path);
                incremental_stats.files_deleted += 1;
            }
        }
    }

    // ------------------------------------------------------------------
    // Step 5: Persist file IR (new and changed files)
    // ------------------------------------------------------------------
    for pf in &parsed_files {
        let commit_date = git_file_dates.get(pf.path.as_path()).copied();
        file_ir_repo.upsert(&branch_id, pf, commit_date)?;
    }
    tracing::info!(count = files_parsed, "Stored file IR records");

    // ------------------------------------------------------------------
    // Step 6: Gather all current parsed files for module graph
    //
    // For incremental scans, we need the full set: unchanged files
    // (loaded from DB) + newly parsed files.
    // ------------------------------------------------------------------
    let all_parsed_files = if is_incremental && incremental_stats.files_unchanged > 0 {
        // Load all IR from DB (which now has the updated set)
        file_ir_repo.get_by_branch(&branch_id)?
    } else {
        // Fresh scan or all files changed — use what we just parsed
        parsed_files.clone()
    };

    // ------------------------------------------------------------------
    // Step 7: Rebuild module structure graph
    //
    // On re-scan, delete old module nodes and edges first, then
    // re-insert. This is simpler and more correct than trying to diff
    // the module graph.
    // ------------------------------------------------------------------
    if is_incremental {
        let deleted_edges = edge_repo.delete_by_branch(&branch_id)?;
        let deleted_nodes = node_repo.delete_by_branch(&branch_id)?;
        tracing::debug!(
            nodes = deleted_nodes,
            edges = deleted_edges,
            "Cleared old module structure for rebuild"
        );
    }

    let module_graph = build_module_graph(root, &all_parsed_files, &branch_id);

    // Persist module nodes with placeholder → real ID remapping.
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

    on_progress(&ScanProgress::ModuleGraphDone);
    on_progress(&ScanProgress::AnalyzingProjectFiles);

    // ------------------------------------------------------------------
    // Step 8: Discover and analyze dependency manifests
    // ------------------------------------------------------------------
    let manifests = discover_manifests(root)?;
    let manifests_analyzed = manifests.len();

    let manifest_analyses = if !manifests.is_empty() {
        let analysis = analyze_manifests(&manifests, &all_parsed_files)?;
        tracing::info!(count = analysis.len(), "Analyzed dependency manifests");
        analysis
    } else {
        Vec::new()
    };

    // ------------------------------------------------------------------
    // Step 9: Discover and parse documentation files
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

    on_progress(&ScanProgress::ProjectFilesDone);

    Ok(ScanResult {
        files_discovered,
        files_parsed,
        nodes_persisted,
        edges_persisted,
        manifests_analyzed,
        docs_ingested,
        manifest_analyses,
        incremental: if is_incremental {
            Some(incremental_stats)
        } else {
            None
        },
        file_dates: git_file_dates,
        excluded_submodules,
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
            let Ok(value) = serde_yml::from_str::<serde_yml::Value>(content) else {
                return false;
            };
            let mapping = match value.as_mapping() {
                Some(m) => m,
                None => return false,
            };
            let has_openapi = mapping.contains_key(serde_yml::Value::String("openapi".to_string()));
            let has_swagger = mapping.contains_key(serde_yml::Value::String("swagger".to_string()));
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

    #[test]
    fn scan_project_incremental_skips_unchanged() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        // Initial scan
        let r1 = scan_project(root, &config, &db).expect("first scan");
        assert!(r1.incremental.is_none(), "first scan is not incremental");
        assert_eq!(r1.files_parsed, 3);

        // Re-scan without changes
        let r2 = scan_project(root, &config, &db).expect("second scan");
        assert!(r2.incremental.is_some(), "second scan is incremental");
        let stats = r2.incremental.unwrap();
        assert_eq!(stats.files_unchanged, 3);
        assert_eq!(stats.files_changed, 0);
        assert_eq!(stats.files_new, 0);
        assert_eq!(stats.files_deleted, 0);
        assert_eq!(r2.files_parsed, 0, "no files re-parsed");
    }

    #[test]
    fn scan_project_incremental_detects_modification() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        // Initial scan
        scan_project(root, &config, &db).expect("first scan");

        // Modify a file
        fs::write(
            root.join("src/config.rs"),
            "pub struct Config { pub name: String, pub extra: bool }\n",
        )
        .unwrap();

        // Re-scan
        let r2 = scan_project(root, &config, &db).expect("second scan");
        let stats = r2.incremental.unwrap();
        assert_eq!(stats.files_changed, 1, "config.rs changed");
        assert_eq!(stats.files_unchanged, 2, "main.rs + format.rs unchanged");
        assert_eq!(r2.files_parsed, 1, "only changed file parsed");
    }

    #[test]
    fn scan_project_incremental_detects_addition() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        scan_project(root, &config, &db).expect("first scan");

        // Add a new file
        fs::write(root.join("src/extra.rs"), "pub fn extra() {}").unwrap();

        let r2 = scan_project(root, &config, &db).expect("second scan");
        let stats = r2.incremental.unwrap();
        assert_eq!(stats.files_new, 1);
        assert_eq!(stats.files_unchanged, 3);
        assert_eq!(r2.files_discovered, 4);
    }

    #[test]
    fn scan_project_incremental_detects_deletion() {
        let dir = create_test_project();
        let root = dir.path();
        let db = Database::open(":memory:").expect("open DB");
        let config = ScanConfig::default();

        scan_project(root, &config, &db).expect("first scan");

        // Delete a file
        fs::remove_file(root.join("src/utils/format.rs")).unwrap();

        let r2 = scan_project(root, &config, &db).expect("second scan");
        let stats = r2.incremental.unwrap();
        assert_eq!(stats.files_deleted, 1);
        assert_eq!(stats.files_unchanged, 2);
        assert_eq!(r2.files_discovered, 2);

        // Verify DB no longer has the deleted file
        let conn = db.connection().clone();
        let file_ir_repo = SqliteFileIRRepository::new(conn);
        let branch = BranchId::from("main");
        let files = file_ir_repo.get_by_branch(&branch).unwrap();
        assert_eq!(files.len(), 2);
    }
}
