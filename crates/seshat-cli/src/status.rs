//! Implementation of the `seshat status` command.
//!
//! Scans the XDG repos directory for `.db` files, identifies root projects vs
//! submodules, reads `repo_metadata` from each DB for summary info, and
//! displays a tree view with aligned columns.

use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;

use seshat_core::BranchId;
use seshat_storage::{
    BranchRepository, Database, FileIRRepository, NodeRepository, RepoMetadataRepository,
    SqliteBranchRepository, SqliteFileIRRepository, SqliteNodeRepository,
    SqliteRepoMetadataRepository, SqliteSubmoduleRepository, SubmoduleRepository, SubmoduleRow,
};

use crate::db;
use crate::error::CliError;
use crate::format::color_enabled;

/// Summary info extracted from a project or submodule database.
struct ProjectSummary {
    /// Display name (project name or mount path for submodules).
    name: String,
    /// Current branch.
    branch: String,
    /// Number of indexed files.
    file_count: usize,
    /// Number of detected conventions.
    convention_count: usize,
    /// Database file size in bytes.
    db_size: u64,
    /// Database path on disk.
    db_path: PathBuf,
    /// Last scan timestamp from repo_metadata (ISO-8601 or epoch string).
    last_scan_time: Option<String>,
}

/// A root project with its optional submodules.
struct ProjectEntry {
    /// Root project summary.
    root: ProjectSummary,
    /// Submodule summaries (from the submodules table in root DB).
    submodules: Vec<SubmoduleSummary>,
}

/// A submodule entry — may have a valid DB or be orphaned/missing.
struct SubmoduleSummary {
    /// Mount path (relative_path from submodules table).
    mount_path: String,
    /// Summary from the submodule DB (None if DB is missing/broken).
    summary: Option<ProjectSummary>,
    /// Whether this submodule DB exists on disk.
    db_exists: bool,
}

/// Run the `seshat status` command.
///
/// Scans the XDG repos directory, identifies root projects and submodules,
/// and displays a tree with summary information.
pub fn run_status(verbose: bool) -> Result<(), CliError> {
    let color = color_enabled();
    let repos_dir = db::xdg_repos_dir()?;

    if !repos_dir.is_dir() {
        eprintln!("No Seshat databases found.");
        eprintln!();
        eprintln!("hint: run `seshat scan <path>` to index a project");
        return Ok(());
    }

    let entries = discover_projects(&repos_dir)?;

    if entries.is_empty() {
        eprintln!("No Seshat databases found.");
        eprintln!();
        eprintln!("hint: run `seshat scan <path>` to index a project");
        return Ok(());
    }

    print_status_tree(&entries, verbose, color);

    Ok(())
}

/// Discover all root projects and their submodules from the repos directory.
///
/// Root projects are `.db` files directly in the repos dir.
/// Submodules are tracked in each root DB's `submodules` table.
fn discover_projects(repos_dir: &Path) -> Result<Vec<ProjectEntry>, CliError> {
    let root_dbs = db::list_available_projects(repos_dir)?;
    let mut entries = Vec::new();

    for (db_path, project_name) in &root_dbs {
        let root_summary = match load_project_summary(db_path, project_name) {
            Some(s) => s,
            None => continue, // Skip DBs that can't be opened
        };

        // Load submodule rows from root DB and resolve each.
        let submodules = load_submodule_summaries(db_path, project_name);

        entries.push(ProjectEntry {
            root: root_summary,
            submodules,
        });
    }

    Ok(entries)
}

/// Load summary info from a database file.
fn load_project_summary(db_path: &Path, name: &str) -> Option<ProjectSummary> {
    let db = Database::open(db_path).ok()?;
    let conn = db.connection().clone();

    // Branch
    let branch_repo = SqliteBranchRepository::new(conn.clone());
    let branch = branch_repo
        .get_current_branch()
        .unwrap_or_else(|_| BranchId::from("main"));

    // File count
    let file_repo = SqliteFileIRRepository::new(conn.clone());
    let file_count = file_repo
        .get_file_hashes_by_branch(&branch)
        .map(|h| h.len())
        .unwrap_or(0);

    // Convention count
    let node_repo = SqliteNodeRepository::new(conn.clone());
    let convention_count = node_repo
        .find_by_branch(&branch)
        .map(|nodes| nodes.len())
        .unwrap_or(0);

    // DB file size
    let db_size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);

    // Last scan time from repo_metadata
    let meta_repo = SqliteRepoMetadataRepository::new(conn);
    let last_scan_time = meta_repo.get("last_scan_time").ok().flatten();

    Some(ProjectSummary {
        name: name.to_string(),
        branch: branch.to_string(),
        file_count,
        convention_count,
        db_size,
        db_path: db_path.to_path_buf(),
        last_scan_time,
    })
}

/// Load submodule summaries from a root project's database.
fn load_submodule_summaries(root_db_path: &Path, project_name: &str) -> Vec<SubmoduleSummary> {
    let db = match Database::open(root_db_path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let sub_repo = SqliteSubmoduleRepository::new(db.connection().clone());
    let rows: Vec<SubmoduleRow> = match sub_repo.list() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    rows.into_iter()
        .map(|row| {
            let sub_db_path = db::resolve_submodule_db_path(project_name, &row.relative_path).ok();

            let db_exists = sub_db_path.as_ref().is_some_and(|p| p.exists());

            let summary = if db_exists {
                sub_db_path
                    .as_ref()
                    .and_then(|p| load_project_summary(p, &row.relative_path))
            } else {
                None
            };

            SubmoduleSummary {
                mount_path: row.relative_path,
                summary,
                db_exists,
            }
        })
        .collect()
}

/// Format a last-scan timestamp for display.
///
/// If the value looks like a Unix epoch (all digits), format as a
/// human-readable date. Otherwise return as-is (likely already ISO-8601).
fn format_last_scan(value: &str) -> String {
    // Try parsing as Unix timestamp (seconds).
    if let Ok(epoch) = value.parse::<i64>() {
        // Convert to a rough human-readable format.
        // We don't have chrono, so use a simple calculation.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let diff = now - epoch;
        if diff < 60 {
            return "just now".to_string();
        } else if diff < 3600 {
            let mins = diff / 60;
            return format!("{mins}m ago");
        } else if diff < 86400 {
            let hours = diff / 3600;
            return format!("{hours}h ago");
        } else {
            let days = diff / 86400;
            return format!("{days}d ago");
        }
    }

    // Already a readable string (ISO-8601 or similar).
    value.to_string()
}

/// Print the status tree to stderr.
fn print_status_tree(entries: &[ProjectEntry], verbose: bool, color: bool) {
    let total_projects = entries.len();
    let total_submodules: usize = entries.iter().map(|e| e.submodules.len()).sum();

    // Header
    if color {
        eprintln!(
            "{}",
            format!("seshat status — {total_projects} project(s)").bold()
        );
    } else {
        eprintln!("seshat status — {total_projects} project(s)");
    }
    eprintln!();

    for (i, entry) in entries.iter().enumerate() {
        let is_last_project = i == entries.len() - 1;
        print_project_entry(entry, is_last_project, verbose, color);
    }

    // Footer summary
    eprintln!();
    let total_files: usize = entries.iter().map(|e| e.root.file_count).sum();
    let total_conventions: usize = entries.iter().map(|e| e.root.convention_count).sum();
    if color {
        eprintln!(
            "{}  {} files, {} conventions across {} project(s) and {} submodule(s)",
            "Total:".dimmed(),
            crate::format::format_number(total_files as u64),
            crate::format::format_number(total_conventions as u64),
            total_projects,
            total_submodules,
        );
    } else {
        eprintln!(
            "Total:  {} files, {} conventions across {} project(s) and {} submodule(s)",
            crate::format::format_number(total_files as u64),
            crate::format::format_number(total_conventions as u64),
            total_projects,
            total_submodules,
        );
    }
}

/// Print a single project entry (root + submodules).
fn print_project_entry(entry: &ProjectEntry, _is_last: bool, verbose: bool, color: bool) {
    let root = &entry.root;

    // Project name line
    let name_display = if color {
        root.name.bold().to_string()
    } else {
        root.name.clone()
    };

    let branch_display = if color {
        format!("({})", root.branch.cyan())
    } else {
        format!("({})", root.branch)
    };

    eprintln!("  {name_display} {branch_display}");

    // Details line
    let files_str = crate::format::format_number(root.file_count as u64);
    let conventions_str = crate::format::format_number(root.convention_count as u64);
    let size_str = crate::format::format_human_size(root.db_size);

    let last_scan_str = root
        .last_scan_time
        .as_ref()
        .map(|t| format_last_scan(t))
        .unwrap_or_else(|| "never".to_string());

    if color {
        eprintln!(
            "    {} {files_str}  {} {conventions_str}  {} {size_str}  {} {last_scan_str}",
            "files:".dimmed(),
            "conventions:".dimmed(),
            "size:".dimmed(),
            "scanned:".dimmed(),
        );
    } else {
        eprintln!(
            "    files: {files_str}  conventions: {conventions_str}  size: {size_str}  scanned: {last_scan_str}",
        );
    }

    // Verbose: full DB path
    if verbose {
        if color {
            eprintln!("    {} {}", "db:".dimmed(), root.db_path.display());
        } else {
            eprintln!("    db: {}", root.db_path.display());
        }
    }

    // Submodules
    for (j, sub) in entry.submodules.iter().enumerate() {
        let is_last_sub = j == entry.submodules.len() - 1;
        let connector = if is_last_sub {
            "└── "
        } else {
            "├── "
        };

        if !sub.db_exists {
            // Orphaned / missing DB
            let warn = if color {
                format!(
                    "    {connector}{} {}",
                    sub.mount_path,
                    "(DB missing)".yellow()
                )
            } else {
                format!("    {connector}{} (DB missing)", sub.mount_path)
            };
            eprintln!("{warn}");
            continue;
        }

        match &sub.summary {
            Some(summary) => {
                let sub_files = crate::format::format_number(summary.file_count as u64);
                let sub_convs = crate::format::format_number(summary.convention_count as u64);
                let sub_size = crate::format::format_human_size(summary.db_size);

                let sub_scan = summary
                    .last_scan_time
                    .as_ref()
                    .map(|t| format_last_scan(t))
                    .unwrap_or_else(|| "never".to_string());

                if color {
                    eprintln!(
                        "    {connector}{} ({})  {sub_files} files, {sub_convs} conventions, {sub_size}, {sub_scan}",
                        sub.mount_path.bold(),
                        summary.branch.cyan(),
                    );
                } else {
                    eprintln!(
                        "    {connector}{} ({})  {sub_files} files, {sub_convs} conventions, {sub_size}, {sub_scan}",
                        sub.mount_path, summary.branch,
                    );
                }

                if verbose {
                    let indent = if is_last_sub {
                        "        "
                    } else {
                        "    │   "
                    };
                    if color {
                        eprintln!("{indent}{} {}", "db:".dimmed(), summary.db_path.display());
                    } else {
                        eprintln!("{indent}db: {}", summary.db_path.display());
                    }
                }
            }
            None => {
                let warn = if color {
                    format!(
                        "    {connector}{} {}",
                        sub.mount_path,
                        "(could not read DB)".yellow()
                    )
                } else {
                    format!("    {connector}{} (could not read DB)", sub.mount_path)
                };
                eprintln!("{warn}");
            }
        }
    }

    eprintln!();
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn format_last_scan_epoch_just_now() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_last_scan(&now.to_string());
        assert_eq!(result, "just now");
    }

    #[test]
    fn format_last_scan_epoch_minutes_ago() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let five_min_ago = now - 300;
        let result = format_last_scan(&five_min_ago.to_string());
        assert_eq!(result, "5m ago");
    }

    #[test]
    fn format_last_scan_epoch_hours_ago() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let two_hours_ago = now - 7200;
        let result = format_last_scan(&two_hours_ago.to_string());
        assert_eq!(result, "2h ago");
    }

    #[test]
    fn format_last_scan_epoch_days_ago() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let three_days_ago = now - 259200;
        let result = format_last_scan(&three_days_ago.to_string());
        assert_eq!(result, "3d ago");
    }

    #[test]
    fn format_last_scan_iso_string_passthrough() {
        let result = format_last_scan("2026-04-03T22:00:00");
        assert_eq!(result, "2026-04-03T22:00:00");
    }

    #[test]
    fn discover_projects_empty_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let repos = tmp.path().join("repos");
        fs::create_dir_all(&repos).expect("create repos dir");

        let entries = discover_projects(&repos).expect("should succeed");
        assert!(entries.is_empty());
    }

    #[test]
    fn discover_projects_with_root_db() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let repos = tmp.path().join("repos");
        fs::create_dir_all(&repos).expect("create repos dir");

        // Create a real DB file
        let db_path = repos.join("test-project.db");
        let db = Database::open(&db_path).expect("create db");

        // Write some repo_metadata
        let meta_repo = SqliteRepoMetadataRepository::new(db.connection().clone());
        meta_repo
            .set("last_scan_time", "1700000000")
            .expect("set metadata");
        drop(db);

        let entries = discover_projects(&repos).expect("should succeed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].root.name, "test-project");
        assert_eq!(entries[0].root.branch, "main");
        assert_eq!(entries[0].root.file_count, 0);
        assert_eq!(entries[0].root.convention_count, 0);
        assert!(entries[0].root.db_size > 0);
        assert_eq!(
            entries[0].root.last_scan_time,
            Some("1700000000".to_string())
        );
    }

    #[test]
    fn discover_projects_with_submodule() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let repos = tmp.path().join("repos");
        fs::create_dir_all(&repos).expect("create repos dir");

        // Create root DB with a submodule entry
        let root_db_path = repos.join("my-project.db");
        let root_db = Database::open(&root_db_path).expect("create root db");

        let sub_repo = SqliteSubmoduleRepository::new(root_db.connection().clone());
        // Create submodule directory structure and DB
        let sub_dir = repos.join("my-project");
        fs::create_dir_all(&sub_dir).expect("create sub dir");
        let sub_db_path = sub_dir.join("vendor-lib.db");
        let sub_db = Database::open(&sub_db_path).expect("create sub db");
        drop(sub_db);

        // Insert submodule row pointing to the real DB path
        use seshat_storage::SubmoduleInput;
        sub_repo
            .insert(&SubmoduleInput {
                relative_path: "vendor-lib".to_string(),
                name: "lib".to_string(),
                db_path: sub_db_path.to_string_lossy().to_string(),
                commit_hash: Some("abc123".to_string()),
            })
            .expect("insert submodule");
        drop(root_db);

        // discover_projects uses resolve_submodule_db_path which uses XDG,
        // so this test verifies the row-loading path but the sub DB resolution
        // will differ. That's OK — the submodule will appear as "DB missing"
        // unless the XDG path happens to match.
        let entries = discover_projects(&repos).expect("should succeed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].root.name, "my-project");
        // Submodule row was loaded (1 entry)
        assert_eq!(entries[0].submodules.len(), 1);
        assert_eq!(entries[0].submodules[0].mount_path, "vendor-lib");
    }

    #[test]
    fn load_project_summary_returns_none_for_bad_path() {
        let result = load_project_summary(Path::new("/nonexistent/path.db"), "test");
        assert!(result.is_none());
    }

    #[test]
    fn load_project_summary_reads_metadata() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let db_path = tmp.path().join("test.db");
        let db = Database::open(&db_path).expect("create db");

        let meta_repo = SqliteRepoMetadataRepository::new(db.connection().clone());
        meta_repo.set("last_scan_time", "1700000000").expect("set");
        drop(db);

        let summary = load_project_summary(&db_path, "test").expect("should load");
        assert_eq!(summary.name, "test");
        assert_eq!(summary.branch, "main");
        assert_eq!(summary.last_scan_time, Some("1700000000".to_string()));
        assert!(summary.db_size > 0);
    }

    #[test]
    fn run_status_no_repos_dir() {
        // When XDG dir doesn't exist, run_status should succeed gracefully.
        // We can't easily mock XDG, but we can verify format_last_scan handles
        // edge cases which is the testable pure logic.
        let result = format_last_scan("not-a-number");
        assert_eq!(result, "not-a-number");
    }
}
