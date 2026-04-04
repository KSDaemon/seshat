//! Implementation of the `seshat serve` command.
//!
//! Discovers the project database via smart resolution (explicit repo argument,
//! current working directory, git root walk-up, or single-DB fallback), displays
//! startup information, and starts the MCP server on stdio transport with
//! graceful Ctrl+C shutdown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use seshat_core::BranchId;
use seshat_mcp::ProjectConnection;
use seshat_storage::{
    BranchRepository, Database, SqliteBranchRepository, SqliteSubmoduleRepository,
    SubmoduleRepository, SubmoduleRow,
};

use crate::config::AppConfig;
use crate::error::CliError;

/// Metadata about a discovered scanned project database.
struct RepoInfo {
    /// Human-readable project name (derived from DB filename).
    name: String,
    /// Path to the `.db` file.
    db_path: PathBuf,
    /// Current branch stored in the database.
    branch: BranchId,
    /// Number of indexed files.
    file_count: usize,
    /// Number of convention nodes.
    convention_count: usize,
}

/// Run the serve command.
///
/// Discovers the project database (from explicit repo arg, cwd, git root, or
/// single-DB fallback), loads it, displays startup information, and starts the
/// MCP server on stdio transport.
pub fn run_serve(
    repo: Option<&Path>,
    host: Option<String>,
    port: Option<u16>,
) -> Result<(), CliError> {
    // -- Load config --------------------------------------------------
    let mut config = AppConfig::load().map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to load config: {e}"),
    })?;

    // CLI flags override config values.
    if let Some(h) = host {
        config.server.host = h;
    }
    if let Some(p) = port {
        config.server.port = p;
    }

    // -- Discover databases -------------------------------------------
    let db_path = crate::db::resolve_serve_db(repo)?;
    let db = Database::open(&db_path).map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    let repo_info = load_repo_info(&db, &db_path)?;

    // -- Load submodule connections -----------------------------------
    let submodule_rows = load_submodule_rows(&db);
    let submodules = open_submodule_connections(&submodule_rows, &repo_info.name);

    // -- Display startup info -----------------------------------------
    print_startup(&repo_info, &submodules, &config);

    // -- Start MCP server (async via tokio) ---------------------------
    let server_config = config.server.clone();
    let _start = Instant::now();

    let runtime = tokio::runtime::Runtime::new().map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to create tokio runtime: {e}"),
    })?;

    let root = ProjectConnection::new(
        db.connection().clone(),
        repo_info.name.clone(),
        repo_info.branch.to_string(),
    );

    runtime
        .block_on(async {
            let shutdown = async {
                tokio::signal::ctrl_c()
                    .await
                    .expect("failed to listen for Ctrl+C");
                eprintln!();
                eprintln!("Shutting down...");
            };

            seshat_mcp::start_stdio_with_shutdown(
                server_config,
                root,
                submodules,
                None, // call_log_path — enabled via --call-log flag in US-004
                shutdown,
                std::time::Duration::from_secs(5),
            )
            .await
        })
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("MCP server error: {e}"),
        })
}

/// Load repository metadata from the database for startup display.
fn load_repo_info(db: &Database, db_path: &Path) -> Result<RepoInfo, CliError> {
    let name = db_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_owned());

    let info = crate::db::load_project_info(db);

    Ok(RepoInfo {
        name,
        db_path: db_path.to_path_buf(),
        branch: info.branch,
        file_count: info.file_count,
        convention_count: info.convention_count,
    })
}

/// Load the list of submodule rows from the root database.
///
/// Returns an empty `Vec` if the query fails (e.g. empty DB, no submodules
/// table data).
fn load_submodule_rows(db: &Database) -> Vec<SubmoduleRow> {
    let sub_repo = SqliteSubmoduleRepository::new(db.connection().clone());
    match sub_repo.list() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!(
                "  Warning: could not read submodules table: {e}. Continuing without submodules."
            );
            Vec::new()
        }
    }
}

/// Open database connections for each submodule and build the `ProjectConnection` map.
///
/// For each submodule row, resolves the DB path, opens the database, reads its
/// branch, and wraps it in a `ProjectConnection`. If a submodule DB is missing
/// or fails to open, a warning is logged and that submodule is skipped.
fn open_submodule_connections(
    rows: &[SubmoduleRow],
    root_project_name: &str,
) -> HashMap<String, ProjectConnection> {
    let mut submodules = HashMap::new();

    for row in rows {
        let db_path =
            match crate::db::resolve_submodule_db_path(root_project_name, &row.relative_path) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "  Warning: could not resolve DB path for submodule '{}': {e}. Skipping.",
                        row.relative_path
                    );
                    continue;
                }
            };

        if !db_path.exists() {
            eprintln!(
                "  Warning: submodule DB not found at '{}'. Skipping '{}'.",
                db_path.display(),
                row.relative_path
            );
            continue;
        }

        let db = match Database::open(&db_path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "  Warning: failed to open submodule DB '{}': {e}. Skipping '{}'.",
                    db_path.display(),
                    row.relative_path
                );
                continue;
            }
        };

        // Read the submodule's branch (default to "main" if not set).
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = branch_repo
            .get_current_branch()
            .unwrap_or_else(|_| BranchId::from("main"));

        let pc = ProjectConnection::new(
            db.connection().clone(),
            row.relative_path.clone(),
            branch.to_string(),
        );

        submodules.insert(row.relative_path.clone(), pc);
    }

    submodules
}

/// Print the startup information block to stderr.
fn print_startup(
    info: &RepoInfo,
    submodules: &HashMap<String, ProjectConnection>,
    config: &AppConfig,
) {
    eprintln!("seshat v{}", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("  Repo:         {}", info.name);
    eprintln!("  Branch:       {}", info.branch);
    eprintln!("  Files:        {}", info.file_count);
    eprintln!("  Conventions:  {}", info.convention_count);
    eprintln!("  Database:     {}", info.db_path.display());
    eprintln!("  Watcher:      not available");

    if submodules.is_empty() {
        eprintln!("  Submodules:   none");
    } else {
        eprintln!("  Submodules:   {}", submodules.len());
        let mut names: Vec<&String> = submodules.keys().collect();
        names.sort();
        for name in names {
            eprintln!("    - {name}");
        }
    }

    eprintln!();
    eprintln!(
        "  Transport:    stdio ({}:{})",
        config.server.host, config.server.port
    );
    eprintln!();
    eprintln!("Ready. Waiting for MCP client connection...");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_repo_info_empty_db() {
        // Verify that load_repo_info works with an empty in-memory DB.
        let db = Database::open(":memory:").expect("in-memory db");
        let path = PathBuf::from("/tmp/test-seshat-project.db");
        let info = load_repo_info(&db, &path).expect("should succeed with empty db");
        assert_eq!(info.name, "test-seshat-project");
        assert_eq!(info.file_count, 0);
        assert_eq!(info.convention_count, 0);
        assert_eq!(info.branch, BranchId::from("main"));
    }

    #[test]
    fn load_submodule_rows_empty_db() {
        let db = Database::open(":memory:").expect("in-memory db");
        let rows = load_submodule_rows(&db);
        assert!(rows.is_empty());
    }

    #[test]
    fn load_submodule_rows_with_data() {
        use seshat_storage::{SqliteSubmoduleRepository, SubmoduleInput, SubmoduleRepository};

        let db = Database::open(":memory:").expect("in-memory db");
        let sub_repo = SqliteSubmoduleRepository::new(db.connection().clone());
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "vendor/libfoo".to_string(),
                name: "libfoo".to_string(),
                db_path: "/data/seshat/repos/proj/vendor/libfoo.db".to_string(),
                commit_hash: Some("abc123".to_string()),
            })
            .expect("insert");
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "libs/core".to_string(),
                name: "core".to_string(),
                db_path: "/data/seshat/repos/proj/libs/core.db".to_string(),
                commit_hash: Some("def456".to_string()),
            })
            .expect("insert");

        let rows = load_submodule_rows(&db);
        assert_eq!(rows.len(), 2);
        // list() returns sorted by relative_path
        assert_eq!(rows[0].relative_path, "libs/core");
        assert_eq!(rows[1].relative_path, "vendor/libfoo");
    }

    #[test]
    fn open_submodule_connections_empty_rows() {
        let submodules = open_submodule_connections(&[], "test-project");
        assert!(submodules.is_empty());
    }

    #[test]
    fn open_submodule_connections_missing_db_skipped() {
        // Create a submodule row pointing to a non-existent DB path.
        let row = SubmoduleRow {
            id: 1,
            relative_path: "vendor/nonexistent".to_string(),
            name: "nonexistent".to_string(),
            db_path: "/no/such/path.db".to_string(),
            commit_hash: Some("abc123".to_string()),
            created_at: "2026-04-03T00:00:00".to_string(),
            updated_at: "2026-04-03T00:00:00".to_string(),
        };

        let submodules = open_submodule_connections(&[row], "test-project");
        // Should be empty since the DB file doesn't exist.
        assert!(submodules.is_empty());
    }

    #[test]
    fn open_submodule_connections_with_real_dbs() {
        use std::fs;

        let project_name = "serve-test-project";
        let mount_path = "vendor/testlib";

        // Resolve where the DB should be and create it.
        let db_path =
            crate::db::resolve_submodule_db_path(project_name, mount_path).expect("resolve path");

        // Create a real DB at that path.
        let db = Database::open(&db_path).expect("create submodule DB");
        // Drop db to close the connection (it'll be reopened by open_submodule_connections).
        drop(db);

        let row = SubmoduleRow {
            id: 1,
            relative_path: mount_path.to_string(),
            name: "testlib".to_string(),
            db_path: db_path.to_string_lossy().to_string(),
            commit_hash: Some("abc123".to_string()),
            created_at: "2026-04-03T00:00:00".to_string(),
            updated_at: "2026-04-03T00:00:00".to_string(),
        };

        let submodules = open_submodule_connections(&[row], project_name);
        assert_eq!(submodules.len(), 1);
        assert!(submodules.contains_key(mount_path));

        let pc = &submodules[mount_path];
        assert_eq!(pc.name, mount_path);
        assert_eq!(pc.branch, "main"); // default branch for empty DB

        // Cleanup: remove the test DB file and parent dirs.
        let _ = fs::remove_file(&db_path);
        if let Some(parent) = db_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}
