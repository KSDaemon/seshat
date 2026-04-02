//! Implementation of the `seshat serve` command.
//!
//! Discovers the most recently modified scanned project database, displays
//! startup information (repo name, branch, file count, conventions count),
//! and starts the MCP server on stdio transport with graceful Ctrl+C shutdown.

use std::path::{Path, PathBuf};
use std::time::Instant;

use seshat_core::BranchId;
use seshat_storage::{
    BranchRepository, Database, FileIRRepository, NodeRepository, SqliteBranchRepository,
    SqliteFileIRRepository, SqliteNodeRepository,
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
/// Discovers scanned project databases, loads the most recent one, displays
/// startup information, and starts the MCP server on stdio transport.
pub fn run_serve(host: Option<String>, port: Option<u16>) -> Result<(), CliError> {
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
    let db_path = discover_db()?;
    let db = Database::open(&db_path).map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to open database: {e}"),
    })?;

    let repo_info = load_repo_info(&db, &db_path)?;

    // -- Display startup info -----------------------------------------
    print_startup(&repo_info, &config);

    // -- Start MCP server (async via tokio) ---------------------------
    let server_config = config.server.clone();
    let start = Instant::now();

    let runtime = tokio::runtime::Runtime::new().map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to create tokio runtime: {e}"),
    })?;

    let conn = db.connection().clone();
    let repo_name = repo_info.name.clone();
    let branch_str = repo_info.branch.to_string();

    runtime.block_on(async {
        let shutdown = async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for Ctrl+C");
            eprintln!();
            eprintln!("Shutting down...");
        };

        seshat_mcp::start_stdio_with_shutdown(
            server_config,
            conn,
            repo_name,
            branch_str,
            shutdown,
            std::time::Duration::from_secs(5),
        )
        .await
        .map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("MCP server error: {e}"),
        })?;

        let uptime = start.elapsed();
        eprintln!("Server stopped. Uptime: {}", format_duration(uptime));

        Ok(())
    })
}

/// Discover the most recently modified `.db` file in the seshat data directory.
///
/// Looks in `$XDG_DATA_HOME/seshat/repos/` (typically `~/.local/share/seshat/repos/`
/// on Linux/macOS) for any `*.db` files. Returns the most recently modified one.
///
/// # Errors
///
/// Returns an error if no `.db` files are found (suggests running `seshat scan` first).
fn discover_db() -> Result<PathBuf, CliError> {
    let data_dir = dirs::data_dir().ok_or_else(|| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: "could not determine XDG data directory".to_owned(),
    })?;

    let repos_dir = data_dir.join("seshat").join("repos");

    if !repos_dir.is_dir() {
        return Err(CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: "no scanned projects found.\n\
                 hint: run `seshat scan <path>` first to index a project"
                .to_owned(),
        });
    }

    let mut db_files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

    let entries = std::fs::read_dir(&repos_dir).map_err(|e| CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!("failed to read repos directory: {e}"),
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("failed to read directory entry: {e}"),
        })?;

        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "db") {
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(modified) = meta.modified() {
                    db_files.push((path, modified));
                }
            }
        }
    }

    if db_files.is_empty() {
        return Err(CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!(
                "no scanned projects found in {}.\n\
                 hint: run `seshat scan <path>` first to index a project",
                repos_dir.display()
            ),
        });
    }

    // Sort by modification time, most recent first.
    db_files.sort_by(|a, b| b.1.cmp(&a.1));

    let (chosen, _) = &db_files[0];

    if db_files.len() > 1 {
        tracing::info!(
            "Multiple databases found ({}), using most recently modified: {}",
            db_files.len(),
            chosen.display()
        );
    }

    Ok(chosen.clone())
}

/// Load repository metadata from the database for startup display.
fn load_repo_info(db: &Database, db_path: &Path) -> Result<RepoInfo, CliError> {
    let conn = db.connection().clone();

    // Get project name from DB filename.
    let name = db_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_owned());

    // Get current branch.
    let branch_repo = SqliteBranchRepository::new(conn.clone());
    let branch = branch_repo
        .get_current_branch()
        .unwrap_or_else(|_| BranchId::from("main"));

    // Count files (lightweight: only reads path + hash columns).
    let file_repo = SqliteFileIRRepository::new(conn.clone());
    let file_count = file_repo
        .get_file_hashes_by_branch(&branch)
        .map(|h| h.len())
        .unwrap_or(0);

    // Count convention nodes.
    let node_repo = SqliteNodeRepository::new(conn);
    let convention_count = node_repo
        .find_by_branch(&branch)
        .map(|nodes| nodes.len())
        .unwrap_or(0);

    Ok(RepoInfo {
        name,
        db_path: db_path.to_path_buf(),
        branch,
        file_count,
        convention_count,
    })
}

/// Print the startup information block to stderr.
fn print_startup(info: &RepoInfo, config: &AppConfig) {
    eprintln!("seshat v{}", env!("CARGO_PKG_VERSION"));
    eprintln!();
    eprintln!("  Repo:         {}", info.name);
    eprintln!("  Branch:       {}", info.branch);
    eprintln!("  Files:        {}", info.file_count);
    eprintln!("  Conventions:  {}", info.convention_count);
    eprintln!("  Database:     {}", info.db_path.display());
    eprintln!("  Watcher:      not available");
    eprintln!();
    eprintln!(
        "  Transport:    stdio ({}:{})",
        config.server.host, config.server.port
    );
    eprintln!();
    eprintln!("Ready. Waiting for MCP client connection...");
}

/// Format a duration as a human-readable string.
fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_seconds_only() {
        let d = std::time::Duration::from_secs(42);
        assert_eq!(format_duration(d), "42s");
    }

    #[test]
    fn format_duration_minutes_and_seconds() {
        let d = std::time::Duration::from_secs(125);
        assert_eq!(format_duration(d), "2m 5s");
    }

    #[test]
    fn format_duration_hours() {
        let d = std::time::Duration::from_secs(3661);
        assert_eq!(format_duration(d), "1h 1m 1s");
    }

    #[test]
    fn format_duration_zero() {
        let d = std::time::Duration::from_secs(0);
        assert_eq!(format_duration(d), "0s");
    }

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
}
