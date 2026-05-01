//! Shared database path utilities used by both `scan` and `serve` commands.
//!
//! All Seshat databases live in `$XDG_DATA_HOME/seshat/repos/{project_name}.db`
//! (typically `~/.local/share/seshat/repos/` on Linux/macOS).

use std::path::{Path, PathBuf};

use rusqlite::params;
use seshat_core::BranchId;
use seshat_storage::{
    BranchRepository, Database, FileIRRepository, IR_SCHEMA_VERSION, NodeRepository,
    SqliteBranchRepository, SqliteFileIRRepository, SqliteNodeRepository,
};

use crate::error::CliError;

/// Import ByteSlice for BStr::to_str() conversion.
use gix::bstr::ByteSlice;

/// Branch names that are never garbage-collected, regardless of git state.
const PROTECTED_BRANCHES: &[&str] = &["main", "master"];

/// Result of resolving what to serve — either an existing database or a
/// project root that needs auto-scanning.
pub(crate) enum ServeTarget {
    /// An existing `.db` file was found — serve it normally (zero behavior change).
    ExistingDb {
        db_path: PathBuf,
        project_root: PathBuf,
    },
    /// No `.db` file found — auto-scan the project root on startup.
    AutoScan {
        project_root: PathBuf,
        db_path: PathBuf,
    },
}

/// Resolved project information: database path and project root directory.
///
/// Used as the shared resolver for all commands that need to locate a project
/// database (serve, review, status). Whether the DB exists on disk is NOT
/// checked here — the caller decides how to handle missing databases.
pub struct ResolvedProject {
    /// Path to the `.db` file (may or may not exist yet).
    pub db_path: PathBuf,
    /// Project root directory on disk (used for branch detection, etc.).
    pub project_root: PathBuf,
}

/// Current Unix timestamp in seconds (since epoch).
pub(crate) fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Core project summary info loadable from any seshat database.
///
/// Used by both `serve` and `status` commands to avoid duplicating the
/// same branch + file-count + convention-count queries.
pub(crate) struct ProjectInfo {
    /// Active branch.
    pub branch: BranchId,
    /// Number of indexed source files.
    pub file_count: usize,
    /// Number of convention nodes.
    pub convention_count: usize,
}

/// Load core project summary info from a database.
///
/// Queries branch, file count, and convention count. Uses "main" as the
/// default branch if no explicit branch has been set.
pub(crate) fn load_project_info(db: &Database) -> ProjectInfo {
    let conn = db.connection().clone();

    let branch_repo = SqliteBranchRepository::new(conn.clone());
    let branch = branch_repo.get_current_branch().unwrap_or_else(|_| {
        tracing::debug!("Could not detect git branch from DB, defaulting to 'main'");
        BranchId::from("main")
    });

    let file_repo = SqliteFileIRRepository::new(conn.clone());
    let file_count = file_repo
        .get_file_hashes_by_branch(&branch)
        .map(|h| h.len())
        .unwrap_or(0);

    let node_repo = SqliteNodeRepository::new(conn);
    let convention_count = node_repo
        .find_by_branch(&branch)
        .map(|nodes| nodes.len())
        .unwrap_or(0);

    ProjectInfo {
        branch,
        file_count,
        convention_count,
    }
}

/// Count files in a database for a given branch, ignoring `ir_schema_version`.
///
/// Unlike `load_project_info`, this query does **not** filter by the current
/// `IR_SCHEMA_VERSION`, so it returns the correct count even when the database
/// was scanned with an older schema version.
pub(crate) fn count_files_any_schema(db: &Database, branch_id: &str) -> usize {
    let conn = db.connection().clone();
    let Ok(guard) = conn.lock() else { return 0 };
    guard
        .query_row(
            "SELECT COUNT(*) FROM files_ir WHERE branch_id = ?1",
            params![branch_id],
            |row| row.get::<_, usize>(0),
        )
        .unwrap_or(0)
}

/// Count convention nodes in a database for a given branch.
pub(crate) fn count_conventions(db: &Database, branch_id: &str) -> usize {
    let conn = db.connection().clone();
    let Ok(guard) = conn.lock() else { return 0 };
    guard
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE branch_id = ?1",
            params![branch_id],
            |row| row.get::<_, usize>(0),
        )
        .unwrap_or(0)
}

/// Returns `true` when all rows in `files_ir` for the given branch already
/// have the current `IR_SCHEMA_VERSION`, or the table is empty.
///
/// Used by the scan command to decide whether a submodule whose git commit
/// hash hasn't changed still needs to be re-scanned (because the IR schema
/// was bumped since the last scan).
pub(crate) fn submodule_ir_schema_is_current(db: &Database, branch_id: &str) -> bool {
    let conn = db.connection().clone();
    let Ok(guard) = conn.lock() else { return true };

    // Count rows that are NOT on the current schema version.
    let stale_count: i64 = guard
        .query_row(
            "SELECT COUNT(*) FROM files_ir
             WHERE branch_id = ?1 AND ir_schema_version != ?2",
            params![branch_id, i64::from(IR_SCHEMA_VERSION)],
            |row| row.get(0),
        )
        .unwrap_or(0);

    stale_count == 0
}

/// Get the XDG repos directory: `$XDG_DATA_HOME/seshat/repos/`.
pub(crate) fn xdg_repos_dir() -> Result<PathBuf, CliError> {
    let data_dir = dirs::data_dir().ok_or_else(|| CliError::CommandFailed {
        command: "seshat".to_owned(),
        reason: "could not determine XDG data directory".to_owned(),
    })?;

    Ok(data_dir.join("seshat").join("repos"))
}

/// Extract project name from the last component of a path.
///
/// ```text
/// ~/Projects/walt-chat-backend  → "walt-chat-backend"
/// ~/Projects/walt-chat-backend/ → "walt-chat-backend"
/// ```
pub(crate) fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Resolve the database path for a project root directory.
///
/// Returns `$XDG_DATA_HOME/seshat/repos/{project_name}.db`.
/// The file may or may not exist yet (scan creates it, serve expects it).
pub(crate) fn resolve_db_path(root: &Path) -> Result<PathBuf, CliError> {
    let name = project_name(root);
    let repos_dir = xdg_repos_dir()?;
    Ok(repos_dir.join(format!("{name}.db")))
}

/// Resolve the database path for a submodule within a project.
///
/// Returns `$XDG_DATA_HOME/seshat/repos/{project_name}/{mount_path}.db`.
/// Parent directories are created automatically via [`std::fs::create_dir_all`].
///
/// # Example
///
/// ```text
/// resolve_submodule_db_path("my-app", "libs/shared")
///   → ~/.local/share/seshat/repos/my-app/libs/shared.db
/// ```
pub(crate) fn resolve_submodule_db_path(
    project_name: &str,
    mount_path: &str,
) -> Result<PathBuf, CliError> {
    let repos_dir = xdg_repos_dir()?;
    let db_path = repos_dir
        .join(project_name)
        .join(format!("{mount_path}.db"));

    // Ensure parent directories exist (e.g., repos/my-app/libs/ for libs/shared.db).
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CliError::CommandFailed {
            command: "scan".to_owned(),
            reason: format!("failed to create submodule database directory: {e}"),
        })?;
    }

    Ok(db_path)
}

/// Maximum iterations for walk-up in find_git_root to prevent symlink cycles.
const GIT_ROOT_MAX_ITERATIONS: u32 = 64;

/// Walk up from `from` to find the nearest `.git` directory.
///
/// Handles git worktrees where `.git` is a file containing `gitdir: <path>`
/// instead of a directory — resolves to the main repository root.
///
/// Returns the parent of `.git` (the repository root).
/// Returns `None` if no `.git` is found before reaching the filesystem root
/// or hitting the iteration limit (symlink cycle protection).
pub fn find_git_root(from: &Path) -> Option<PathBuf> {
    let mut current = if from.is_absolute() {
        from.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(from)
    };

    for _ in 0..GIT_ROOT_MAX_ITERATIONS {
        let git_path = current.join(".git");
        if git_path.is_dir() {
            return Some(current);
        }
        if git_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&git_path) {
                if let Some(gitdir) = content.strip_prefix("gitdir: ") {
                    let gitdir_path = PathBuf::from(gitdir.trim());
                    let raw_resolved = if gitdir_path.is_absolute() {
                        gitdir_path
                    } else {
                        git_path.parent()?.join(gitdir_path)
                    };
                    // Normalize the resolved path (handle .. components).
                    let mut normalized = PathBuf::new();
                    for component in raw_resolved.components() {
                        match component {
                            std::path::Component::ParentDir => {
                                normalized.pop();
                            }
                            _ => {
                                normalized.push(component);
                            }
                        }
                    }
                    // Walk up from resolved gitdir to find the main repo root
                    // (which has HEAD or config).
                    let mut candidate = normalized.clone();
                    for _ in 0..GIT_ROOT_MAX_ITERATIONS {
                        if let Some(parent) = candidate.parent() {
                            if parent.join("HEAD").exists() || parent.join("config").exists() {
                                // If found directory is a .git directory, return its parent (the repo root).
                                if parent.file_name().map(|n| n == ".git").unwrap_or(false) {
                                    return parent
                                        .parent()
                                        .map(PathBuf::from)
                                        .or(Some(parent.to_path_buf()));
                                }
                                return Some(parent.to_path_buf());
                            }
                            if !candidate.pop() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        }
        if !current.pop() {
            return None;
        }
    }

    // Hit iteration limit — likely a symlink cycle.
    tracing::warn!(
        path = %from.display(),
        "find_git_root reached iteration limit; possible symlink cycle"
    );
    None
}

/// Detect the current git branch for the given path.
///
/// Uses `get_current_branch` which resolves worktree `.git` files correctly,
/// handles detached HEAD (returns short commit hash), and normalizes path
/// components. Falls back to `"main"` on any error with a debug trace.
pub fn detect_branch(path: &Path) -> String {
    get_current_branch(path).unwrap_or_else(|| {
        tracing::debug!(path = %path.display(), "Could not detect git branch, defaulting to 'main'");
        "main".to_string()
    })
}

/// Get the current git branch name for the repository containing `path`.
///
/// Reads the HEAD file directly, handling both normal repos and worktrees
/// (where `.git` is a file with `gitdir:` prefix).
///
/// Returns `Some(branch_name)` when HEAD points to a branch reference
/// (e.g., `refs/heads/main` → `"main"`).
/// Returns `Some(commit_hash)` when HEAD is detached.
/// Returns `None` when HEAD cannot be read.
pub fn get_current_branch(path: &Path) -> Option<String> {
    read_head_file(path)
}

/// Read the HEAD file directly and extract branch name.
///
/// Handles both normal repos (`.git` is a directory) and worktrees
/// (`.git` is a file with `gitdir:` prefix).
fn read_head_file(path: &Path) -> Option<String> {
    let git_dir = find_git_dir(path)?;

    let head_path = match &git_dir {
        GitDir::Dir(dir) => dir.join("HEAD"),
        GitDir::File(file) => {
            if let Ok(content) = std::fs::read_to_string(file) {
                if let Some(gitdir) = content.strip_prefix("gitdir: ") {
                    let gitdir_path = PathBuf::from(gitdir.trim());
                    let resolved = if gitdir_path.is_absolute() {
                        gitdir_path
                    } else {
                        file.parent()?.join(gitdir_path)
                    };
                    return read_head_from_gitdir(&resolved);
                }
            }
            return None;
        }
    };

    let content = match std::fs::read_to_string(&head_path) {
        Ok(c) => c,
        Err(_) => return None,
    };

    if let Some(rest) = content.strip_prefix("ref: ") {
        let ref_name = rest.trim().to_string();
        if ref_name.starts_with("refs/heads/") {
            return Some(ref_name.trim_start_matches("refs/heads/").to_string());
        }
    }

    // Detached HEAD — content is a commit hash (e.g., "a1b2c3d4...")
    // Accept both full (40-char) and abbreviated hashes (>= 7 chars).
    let trimmed = content.trim();
    if trimmed.len() >= 7 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(trimmed.to_string());
    }

    None
}

/// Locate the `.git` directory or file, walking up from `path`.
enum GitDir {
    Dir(PathBuf),
    File(PathBuf),
}

fn find_git_dir(path: &Path) -> Option<GitDir> {
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };

    loop {
        let git_path = current.join(".git");
        if git_path.is_dir() {
            return Some(GitDir::Dir(git_path));
        }
        if git_path.is_file() {
            return Some(GitDir::File(git_path));
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Read the HEAD file from a resolved git directory path.
fn read_head_from_gitdir(gitdir: &Path) -> Option<String> {
    let head_path = gitdir.join("HEAD");
    let content = match std::fs::read_to_string(&head_path) {
        Ok(c) => c,
        Err(_) => return None,
    };

    if let Some(rest) = content.strip_prefix("ref: ") {
        let ref_name = rest.trim().to_string();
        if ref_name.starts_with("refs/heads/") {
            return Some(ref_name.trim_start_matches("refs/heads/").to_string());
        }
    }

    // Detached HEAD — content is a commit hash (e.g., "a1b2c3d4...")
    let trimmed = content.trim();
    if trimmed.len() == 40 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(trimmed.to_string());
    }

    None
}

/// Discover local git branch names for the repository containing `path`.
///
/// Uses `gix` to walk all local branches under `refs/heads/`.
/// Returns an empty vec if the path is not in a git repository or no branches exist.
pub fn get_git_branches(path: &Path) -> Vec<String> {
    let repo = match gix::open(path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut branches = Vec::new();

    if let Ok(all_refs) = repo.references() {
        if let Ok(mut local_branches) = all_refs.local_branches() {
            while let Some(Ok(entry)) = local_branches.next() {
                let full_name = entry.name().as_bstr();
                let name_str = full_name.to_str().unwrap_or("");
                if let Some(short_name) = name_str.strip_prefix("refs/heads/") {
                    branches.push(short_name.to_string());
                }
            }
        }
    }

    branches
}

/// Check whether the given path is a valid git repository.
///
/// Returns `true` if `gix::open` succeeds, `false` otherwise.
fn is_valid_git_repo(path: &Path) -> bool {
    gix::open(path).is_ok()
}

/// Compare branches stored in the database against branches that exist in git.
///
/// Deletes branch snapshots from the database for branches that exist in the DB
/// but no longer have a corresponding local git branch.
///
/// Safety rules:
/// - Never deletes `main` or `master` branches
/// - Never deletes the current branch (detected from git)
///
/// Returns the list of deleted branch names.
pub fn gc_branch_snapshots(db: &Database, repo_path: &Path) -> Result<Vec<String>, CliError> {
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());

    // Get branches stored in the database
    let db_branches = branch_repo
        .list_branches()
        .map_err(|e| CliError::CommandFailed {
            command: "gc_branch_snapshots".to_owned(),
            reason: format!("failed to list branches from database: {e}"),
        })?;

    if db_branches.is_empty() {
        return Ok(Vec::new());
    }

    // Validate that the path is a git repository
    if !is_valid_git_repo(repo_path) {
        tracing::warn!(
            repo_path = %repo_path.display(),
            "repo_path is not a valid git repository; skipping git branch comparison"
        );
    }

    // Get current git branches
    let git_branches = get_git_branches(repo_path);
    let git_set: std::collections::HashSet<&str> =
        git_branches.iter().map(|s| s.as_str()).collect();

    // Get current branch name
    let current_branch = get_current_branch(repo_path).unwrap_or_default();

    let mut deleted = Vec::new();

    for branch_id in &db_branches {
        let name = &branch_id.0;

        // Never GC protected branches
        if PROTECTED_BRANCHES.contains(&name.as_str()) {
            continue;
        }

        // Never GC current branch
        if name == &current_branch {
            continue;
        }

        // Only GC branches that don't exist in git anymore
        if git_set.contains(name.as_str()) {
            continue;
        }

        // Safe to delete
        tracing::info!(
            branch = %name,
            current_branch = %current_branch,
            "Deleting orphan branch snapshot"
        );

        branch_repo
            .delete_branch(branch_id)
            .map_err(|e| CliError::CommandFailed {
                command: "gc_branch_snapshots".to_owned(),
                reason: format!("failed to delete branch '{name}': {e}"),
            })?;

        deleted.push(name.clone());
    }

    if !deleted.is_empty() {
        tracing::info!(
            deleted_count = deleted.len(),
            deleted_branches = ?deleted,
            "Branch snapshot garbage collection complete"
        );
    }

    Ok(deleted)
}

/// List all `.db` files in the repos directory.
///
/// Returns `(path, project_name)` pairs sorted alphabetically by name.
pub(crate) fn list_available_projects(
    repos_dir: &Path,
) -> Result<Vec<(PathBuf, String)>, CliError> {
    if !repos_dir.is_dir() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(repos_dir).map_err(|e| CliError::CommandFailed {
        command: "seshat".to_owned(),
        reason: format!("failed to read repos directory: {e}"),
    })?;

    let mut projects: Vec<(PathBuf, String)> = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| CliError::CommandFailed {
            command: "seshat".to_owned(),
            reason: format!("failed to read directory entry: {e}"),
        })?;

        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "db") {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if !name.is_empty() {
                projects.push((path, name));
            }
        }
    }

    projects.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(projects)
}

/// Try to read the `project_root` value from `repo_metadata` in the given DB.
/// Returns `None` if the DB can't be opened, or the key doesn't exist.
fn read_project_root_from_db(db_path: &Path) -> Option<PathBuf> {
    use seshat_storage::{Database, RepoMetadataRepository, SqliteRepoMetadataRepository};

    let db = Database::open(db_path).ok()?;
    let meta_repo = SqliteRepoMetadataRepository::new(db.connection().clone());
    let root_str = match meta_repo.get("project_root") {
        Ok(Some(s)) => s,
        _ => return None,
    };
    Some(PathBuf::from(root_str))
}

/// Common project resolution logic used by all commands.
///
/// Determines the project root directory and the expected database path for a
/// project. Whether the DB file actually exists on disk is NOT checked here —
/// the caller decides.
///
/// `command_name` is used in error messages to identify the calling command.
///
/// Resolution priority:
/// 1. Explicit path argument (directory or project name)
/// 2. Current working directory
/// 3. Git root walk-up from cwd
/// 4. Single available project fallback
pub fn resolve_project(
    explicit_path: Option<&Path>,
    command_name: &str,
) -> Result<ResolvedProject, CliError> {
    let repos_dir = xdg_repos_dir()?;

    // Priority 1: explicit path argument
    if let Some(repo_arg) = explicit_path {
        if repo_arg.is_dir() {
            let name = project_name(repo_arg);
            let db = repos_dir.join(format!("{name}.db"));
            return Ok(ResolvedProject {
                project_root: repo_arg.to_path_buf(),
                db_path: db,
            });
        }

        // Treat as project name or non-existent directory.
        let name = repo_arg.to_string_lossy();
        let db = repos_dir.join(format!("{name}.db"));
        if db.exists() && db.is_file() {
            return Ok(ResolvedProject {
                project_root: read_project_root_from_db(&db)
                    .or_else(|| db.parent().map(PathBuf::from))
                    .unwrap_or(repos_dir.clone()),
                db_path: db,
            });
        }

        // Maybe it's a path — extract last component as project name.
        let name_from_path = project_name(repo_arg);
        let db_from_path = repos_dir.join(format!("{name_from_path}.db"));
        if db_from_path.exists() && db_from_path.is_file() {
            return Ok(ResolvedProject {
                project_root: read_project_root_from_db(&db_from_path)
                    .or_else(|| db_from_path.parent().map(PathBuf::from))
                    .unwrap_or(repos_dir.clone()),
                db_path: db_from_path,
            });
        }

        // No DB found and arg wasn't a valid directory — error.
        return Err(CliError::CommandFailed {
            command: command_name.to_owned(),
            reason: format!(
                "project '{}' has not been found.\n\
                  hint: run `seshat scan {}` first",
                name,
                repo_arg.display()
            ),
        });
    }

    // Priority 2: current working directory
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_name = project_name(&cwd);
        let cwd_db = repos_dir.join(format!("{cwd_name}.db"));
        if cwd_db.exists() && cwd_db.is_file() {
            tracing::info!(project = %cwd_name, "Auto-detected project from working directory");
            let project_root = read_project_root_from_db(&cwd_db).unwrap_or_else(|| cwd.clone());
            return Ok(ResolvedProject {
                project_root,
                db_path: cwd_db,
            });
        }

        // Priority 3: walk up to git root
        if let Some(git_root) = find_git_root(&cwd) {
            let repo_name = project_name(&git_root);
            let repo_db = repos_dir.join(format!("{repo_name}.db"));
            if repo_db.exists() && repo_db.is_file() {
                tracing::info!(
                  project = %repo_name,
                  git_root = %git_root.display(),
                    "Auto-detected project from git root"
                );
                let project_root =
                    read_project_root_from_db(&repo_db).unwrap_or_else(|| git_root.clone());
                return Ok(ResolvedProject {
                    project_root,
                    db_path: repo_db,
                });
            }

            // Git root found but no DB.
            return Ok(ResolvedProject {
                project_root: git_root,
                db_path: repo_db,
            });
        }

        // No git root — use cwd.
        return Ok(ResolvedProject {
            project_root: cwd,
            db_path: cwd_db,
        });
    }

    // Priority 4/5: check available projects
    let projects = list_available_projects(&repos_dir)?;

    match projects.len() {
        0 => Err(CliError::CommandFailed {
            command: command_name.to_owned(),
            reason: "no scanned projects found.\n\
                   hint: run `seshat scan <path>` first to index a project"
                .to_string(),
        }),
        1 => {
            let (ref path, ref name) = projects[0];
            tracing::info!(project = %name, "Auto-selected only available project");
            let project_root = read_project_root_from_db(path)
                .or_else(|| path.parent().map(PathBuf::from))
                .unwrap_or(repos_dir.clone());
            Ok(ResolvedProject {
                project_root,
                db_path: path.clone(),
            })
        }
        _ => {
            let project_list = projects
                .iter()
                .map(|(_, name)| format!("    ‣ {name}"))
                .collect::<Vec<_>>()
                .join("\n");

            Err(CliError::CommandFailed {
                command: command_name.to_owned(),
                reason: format!(
                    "could not determine which project to use.\n\n\
                      Available scanned projects:\n\
                        {project_list}\n\n\
                      hint: run from the project directory, or specify:\n\
                        \x20     seshat <command> <project-name>\n\
                        \x20     seshat <command> <path-to-project>"
                ),
            })
        }
    }
}

/// Resolves what to serve — either an existing database or a project root that
/// needs auto-scanning.
///
/// When no `.db` file is found, instead of erroring, this function determines
/// the project root and returns `ServeTarget::AutoScan`. The caller can then
/// create an empty DB and launch a background scan.
pub(crate) fn resolve_serve_db_or_project_root(
    explicit_repo: Option<&Path>,
) -> Result<ServeTarget, CliError> {
    let resolved = resolve_project(explicit_repo, "serve")?;
    if resolved.db_path.exists() {
        Ok(ServeTarget::ExistingDb {
            db_path: resolved.db_path,
            project_root: resolved.project_root,
        })
    } else {
        Ok(ServeTarget::AutoScan {
            project_root: resolved.project_root,
            db_path: resolved.db_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct CleanupDir(PathBuf);
    impl Drop for CleanupDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn setup_repos_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let repos = tmp.path().join("seshat").join("repos");
        fs::create_dir_all(&repos).expect("create repos dir");
        (tmp, repos)
    }

    #[test]
    fn project_name_extracts_last_component() {
        assert_eq!(
            project_name(Path::new("/Users/me/Projects/my-app")),
            "my-app"
        );
        assert_eq!(project_name(Path::new("my-app")), "my-app");
        // "." has no file_name() component — falls back to "unknown"
        assert_eq!(project_name(Path::new(".")), "unknown");
    }

    #[test]
    fn find_git_root_finds_parent_with_dotgit() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let project = tmp.path().join("my-project");
        let subdir = project.join("src").join("api");
        fs::create_dir_all(&subdir).expect("create subdirs");
        fs::create_dir(project.join(".git")).expect("create .git");

        let root = find_git_root(&subdir);
        assert_eq!(root, Some(project));
    }

    #[test]
    fn find_git_root_returns_none_without_dotgit() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let subdir = tmp.path().join("no-git").join("src");
        fs::create_dir_all(&subdir).expect("create subdirs");

        assert!(find_git_root(&subdir).is_none());
    }

    #[test]
    fn list_available_projects_returns_sorted() {
        let (_tmp, repos) = setup_repos_dir();
        fs::write(repos.join("zebra.db"), "").unwrap();
        fs::write(repos.join("alpha.db"), "").unwrap();
        fs::write(repos.join("middle.db"), "").unwrap();
        // Non-db file should be ignored
        fs::write(repos.join("notes.txt"), "").unwrap();

        let projects = list_available_projects(&repos).unwrap();
        let names: Vec<&str> = projects.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn list_available_projects_empty_dir() {
        let (_tmp, repos) = setup_repos_dir();
        let projects = list_available_projects(&repos).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn list_available_projects_nonexistent_dir() {
        let projects = list_available_projects(Path::new("/nonexistent/path")).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn submodule_ir_schema_is_current_empty_db_returns_true() {
        // Empty DB (no rows in files_ir) → no stale rows → schema is current.
        let tmp = tempfile::tempdir().expect("create temp dir");
        let db_path = tmp.path().join("sub.db");
        let db = Database::open(&db_path).expect("open");
        assert!(submodule_ir_schema_is_current(&db, "main"));
    }

    #[test]
    fn submodule_ir_schema_is_current_detects_stale_rows() {
        use seshat_core::test_helpers::make_project_file;
        use seshat_storage::{FileIRRepository, SqliteFileIRRepository};

        let tmp = tempfile::tempdir().expect("create temp dir");
        let db_path = tmp.path().join("sub.db");
        let db = Database::open(&db_path).expect("open");

        let branch = BranchId::from("main");
        // Insert a row via the normal upsert path (writes current IR_SCHEMA_VERSION).
        let file = make_project_file(seshat_core::Language::Rust);
        SqliteFileIRRepository::new(db.connection().clone())
            .upsert(&branch, &file, None)
            .expect("upsert");

        // Verify current schema is detected as current.
        assert!(submodule_ir_schema_is_current(&db, "main"));

        // Now manually corrupt the ir_schema_version to simulate an old scan.
        {
            let guard = db.connection().lock().expect("lock");
            guard
                .execute(
                    "UPDATE files_ir SET ir_schema_version = 0 WHERE branch_id = 'main'",
                    [],
                )
                .expect("update");
        }

        // Should now report schema as stale.
        assert!(!submodule_ir_schema_is_current(&db, "main"));
    }

    #[test]
    fn resolve_submodule_db_path_creates_parent_dirs() {
        let project = "db-test-submod-nested";
        let result = resolve_submodule_db_path(project, "libs/shared");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(
            path.ends_with(format!("{project}/libs/shared.db")),
            "Expected path ending with {project}/libs/shared.db, got: {}",
            path.display()
        );
        // Clean up the directories created by resolve_submodule_db_path.
        if let Ok(repos) = xdg_repos_dir() {
            let _ = fs::remove_dir_all(repos.join(project));
        }
    }

    #[test]
    fn resolve_serve_db_or_project_root_returns_auto_scan_when_no_db() {
        let tmp_dir = tempfile::tempdir().expect("create temp dir");
        let project_dir = tmp_dir.path().join("new-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Explicit directory with no existing DB → AutoScan.
        let result = resolve_serve_db_or_project_root(Some(&project_dir));
        assert!(result.is_ok());
        match result.unwrap() {
            ServeTarget::AutoScan {
                project_root,
                db_path,
            } => {
                assert_eq!(project_root, project_dir);
                assert!(db_path.to_string_lossy().ends_with("new-project.db"));
            }
            ServeTarget::ExistingDb { .. } => {
                panic!("Expected AutoScan, got ExistingDb");
            }
        }
    }

    #[test]
    fn resolve_serve_db_or_project_root_returns_existing_db_when_present() {
        // Create a temp project directory and its DB in the real XDG repos dir.
        let repos_dir = xdg_repos_dir().expect("repos dir");
        let _cleanup = CleanupDir(repos_dir.join("_test_serve_existing"));

        let project_name = "_test_serve_existing";
        let db_path = repos_dir.join(format!("{project_name}.db"));
        fs::write(&db_path, "").unwrap();

        let project_dir = tempfile::tempdir().expect("temp dir");

        let result =
            resolve_serve_db_or_project_root(Some(project_dir.path().join(project_name).as_path()));
        // The explicit repo arg is a path that doesn't exist as a directory,
        // so it's treated as a project name. With the DB existing, it should
        // return ExistingDb.
        if let Ok(ServeTarget::ExistingDb {
            db_path: resolved,
            project_root,
        }) = result
        {
            assert!(
                resolved
                    .to_string_lossy()
                    .ends_with("_test_serve_existing.db")
            );
            // project_root should be read from repo_metadata, not db_path.parent()
            // Since the DB was just created empty, project_root defaults to repos_dir
            assert_eq!(project_root, repos_dir);
        }
    }

    #[test]
    fn resolve_serve_db_or_project_root_uses_cwd_when_no_git() {
        let tmp_dir = tempfile::tempdir().expect("create temp dir");
        let project_dir = tmp_dir.path().join("no-git-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Explicit directory path with no DB and no git → AutoScan with cwd.
        let result = resolve_serve_db_or_project_root(Some(&project_dir));
        assert!(result.is_ok());
        match result.unwrap() {
            ServeTarget::AutoScan { project_root, .. } => {
                assert_eq!(project_root, project_dir);
            }
            ServeTarget::ExistingDb { .. } => {
                panic!("Expected AutoScan, got ExistingDb");
            }
        }
    }

    #[test]
    fn existing_db_project_root_is_used_for_branch_detection() {
        let tmp_dir = tempfile::tempdir().expect("create temp dir");
        let project_dir = tmp_dir.path().join("my-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Initialize a git repo with a specific branch
        let git_output = std::process::Command::new("git")
            .arg("init")
            .arg("-b")
            .arg("feature-x")
            .current_dir(&project_dir)
            .output()
            .expect("git init");
        assert!(git_output.status.success(), "git init failed");

        // Create a DB in the XDG repos dir to make it ExistingDb
        let repos_dir = xdg_repos_dir().expect("repos dir");
        let db_path = repos_dir.join("my-project.db");
        let _cleanup = CleanupDir(db_path.clone());
        fs::write(&db_path, "").unwrap();

        // Resolve — should be ExistingDb with project_root = the actual project dir
        let result = resolve_serve_db_or_project_root(Some(&project_dir));
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());

        let (resolved_root, db_file) = match result.unwrap() {
            ServeTarget::ExistingDb {
                project_root,
                db_path,
            } => (project_root, db_path),
            _ => panic!("Expected ExistingDb"),
        };

        // project_root should be the project directory, not the repos dir
        assert_eq!(resolved_root, project_dir);
        assert!(db_file.to_string_lossy().ends_with("my-project.db"));

        // detect_branch on the resolved project_root should return the actual branch
        let branch = detect_branch(&resolved_root);
        assert_eq!(branch.as_str(), "feature-x");
    }

    #[test]
    fn find_git_root_handles_worktree_gitfile() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let main_project = tmp.path().join("main-repo");
        fs::create_dir_all(&main_project).expect("create main project");
        // Create HEAD in main repo so walk-up can find it.
        fs::write(main_project.join("HEAD"), "ref: refs/heads/main").expect("write HEAD");

        let worktree = tmp.path().join("worktree");
        fs::create_dir_all(&worktree).expect("create worktree");

        let main_git = main_project.join(".git");
        let rel = main_git.strip_prefix(worktree.parent().unwrap()).unwrap();
        let gitdir_rel = PathBuf::from("../").join(rel);
        let gitdir_content = format!("gitdir: {}\n", gitdir_rel.display());
        fs::write(worktree.join(".git"), gitdir_content).expect("write .git file");

        let result = find_git_root(&worktree);
        assert_eq!(result, Some(main_project));
    }

    #[test]
    fn find_git_root_handles_nested_worktree() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let main_project = tmp.path().join("main-project");
        fs::create_dir_all(&main_project).expect("create main project");
        fs::create_dir(main_project.join(".git")).expect("create .git dir");

        let worktree = main_project.join("worktree");
        fs::create_dir_all(&worktree).expect("create worktree");

        let rel = main_project
            .strip_prefix(worktree.parent().unwrap())
            .unwrap();
        let gitdir_content = format!("gitdir: {}\n", rel.display());
        fs::write(worktree.join(".git"), gitdir_content).expect("write .git file");

        let subdir = worktree.join("src").join("api");
        fs::create_dir_all(&subdir).expect("create subdir");

        let root = find_git_root(&subdir);
        assert_eq!(root, Some(main_project));
    }

    #[test]
    fn get_current_branch_from_git_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");

        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");

        fs::write(repo.join("README.md"), "# Test").expect("write file");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");

        let branch = get_current_branch(&repo);
        assert_eq!(branch, Some("main".to_string()));
    }

    #[test]
    fn get_current_branch_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main_repo = dir.path().join("main-repo");
        fs::create_dir_all(&main_repo).expect("create main repo");

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&main_repo)
            .output()
            .expect("git init");

        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&main_repo)
            .output()
            .expect("git config email");

        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&main_repo)
            .output()
            .expect("git config name");

        fs::write(main_repo.join("README.md"), "# Main").expect("write");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&main_repo)
            .output()
            .expect("git commit");

        // Create a worktree using git worktree command
        let worktree = main_repo.join("worktree");
        let status = std::process::Command::new("git")
            .args(["worktree", "add", "../worktree"])
            .current_dir(&main_repo)
            .status()
            .expect("git worktree add");
        assert!(status.success(), "git worktree add failed");

        let branch = get_current_branch(&worktree);
        assert_eq!(branch, Some("main".to_string()));
    }

    #[test]
    fn get_current_branch_detached_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");

        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");

        fs::write(repo.join("file.txt"), "content").expect("write");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");

        // Detach HEAD
        std::process::Command::new("git")
            .args(["checkout", "--detach", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git checkout detach");

        let branch = get_current_branch(&repo);
        assert!(
            branch
                .as_deref()
                .is_some_and(|b| b.len() == 40 && b.chars().all(|c| c.is_ascii_hexdigit())),
            "detached HEAD should return commit hash, got: {:?}",
            branch
        );
    }

    #[test]
    fn gc_deletes_orphan_branches() {
        // Create a temp git repo with main and a feature branch.
        let git_dir = tempfile::tempdir().expect("tempdir");
        let repo = git_dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");
        fs::write(repo.join("README.md"), "# Test").expect("write file");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");
        // Create a feature branch in git.
        std::process::Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(&repo)
            .output()
            .expect("git checkout feature");
        fs::write(repo.join("feature.txt"), "feat").expect("write");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "feature"])
            .current_dir(&repo)
            .output()
            .expect("git commit");
        std::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&repo)
            .output()
            .expect("git checkout main");

        // Create a DB with main, feature, and orphan branches.
        // First insert data for main to register it, then snapshot to others.
        let db_dir = tempfile::tempdir().expect("tempdir");
        let db_path = db_dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open db");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let file_repo = SqliteFileIRRepository::new(db.connection().clone());

        branch_repo
            .switch_branch(&BranchId::from("main"))
            .expect("switch to main");

        // Insert a file to register the main branch
        use seshat_core::test_helpers::make_project_file;
        let file = make_project_file(seshat_core::Language::Rust);
        file_repo
            .upsert(&BranchId::from("main"), &file, None)
            .expect("upsert file");

        // Snapshot main to feature and orphan-branch
        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("feature"))
            .expect("snapshot feature");
        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("orphan-branch"))
            .expect("snapshot orphan");

        // Run GC — orphan-branch should be deleted, main and feature preserved.
        let deleted = gc_branch_snapshots(&db, &repo).expect("gc");
        assert_eq!(deleted, vec!["orphan-branch"]);

        // Verify remaining branches.
        let remaining = branch_repo.list_branches().expect("list branches");
        let names: Vec<&str> = remaining.iter().map(|b| b.0.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"feature"));
        assert!(!names.contains(&"orphan-branch"));
    }

    #[test]
    fn gc_preserves_current_branch() {
        // Create a temp git repo with NO commits (so no branches in git).
        let git_dir = tempfile::tempdir().expect("tempdir");
        let repo = git_dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        // Create a DB with main and some-branch.
        // Current git branch is "main" (default after git init) even though
        // git has no branches yet. "main" should be preserved.
        let db_dir = tempfile::tempdir().expect("tempdir");
        let db_path = db_dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open db");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let file_repo = SqliteFileIRRepository::new(db.connection().clone());

        branch_repo
            .switch_branch(&BranchId::from("main"))
            .expect("switch to main");

        // Insert data for main
        use seshat_core::test_helpers::make_project_file;
        let file = make_project_file(seshat_core::Language::Rust);
        file_repo
            .upsert(&BranchId::from("main"), &file, None)
            .expect("upsert file");

        // Snapshot main to some-branch
        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("some-branch"))
            .expect("snapshot some-branch");

        // Run GC — main should be preserved as current branch even though
        // git has no branches (get_git_branches returns empty).
        let deleted = gc_branch_snapshots(&db, &repo).expect("gc");
        assert!(!deleted.contains(&"main".to_string()));

        // Verify some-branch was deleted (it's not protected, not current, not in git).
        assert!(deleted.contains(&"some-branch".to_string()));

        // Verify main is still there.
        let remaining = branch_repo.list_branches().expect("list branches");
        let names: Vec<&str> = remaining.iter().map(|b| b.0.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(!names.contains(&"some-branch"));
    }

    #[test]
    fn gc_preserves_main() {
        // Create a temp git repo with no branches (just init).
        let git_dir = tempfile::tempdir().expect("tempdir");
        let repo = git_dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        // Create a DB with main and some other branches.
        let db_dir = tempfile::tempdir().expect("tempdir");
        let db_path = db_dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open db");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let file_repo = SqliteFileIRRepository::new(db.connection().clone());

        branch_repo
            .switch_branch(&BranchId::from("main"))
            .expect("switch to main");

        // Insert data for main
        use seshat_core::test_helpers::make_project_file;
        let file = make_project_file(seshat_core::Language::Rust);
        file_repo
            .upsert(&BranchId::from("main"), &file, None)
            .expect("upsert file");

        // Snapshot main to some-branch
        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("some-branch"))
            .expect("snapshot some-branch");

        // Run GC — main should NEVER be deleted.
        let deleted = gc_branch_snapshots(&db, &repo).expect("gc");
        assert!(!deleted.contains(&"main".to_string()));

        // Verify main is still there.
        let remaining = branch_repo.list_branches().expect("list branches");
        let names: Vec<&str> = remaining.iter().map(|b| b.0.as_str()).collect();
        assert!(names.contains(&"main"));
    }

    #[test]
    fn gc_preserves_master() {
        // Create a temp git repo with no branches (just init).
        let git_dir = tempfile::tempdir().expect("tempdir");
        let repo = git_dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        // Create a DB with master and some other branches.
        let db_dir = tempfile::tempdir().expect("tempdir");
        let db_path = db_dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open db");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let file_repo = SqliteFileIRRepository::new(db.connection().clone());

        branch_repo
            .switch_branch(&BranchId::from("master"))
            .expect("switch to master");

        // Insert data for master
        use seshat_core::test_helpers::make_project_file;
        let file = make_project_file(seshat_core::Language::Rust);
        file_repo
            .upsert(&BranchId::from("master"), &file, None)
            .expect("upsert file");

        // Snapshot master to some-branch
        branch_repo
            .create_snapshot(&BranchId::from("master"), &BranchId::from("some-branch"))
            .expect("snapshot some-branch");

        // Run GC — master should NEVER be deleted.
        let deleted = gc_branch_snapshots(&db, &repo).expect("gc");
        assert!(!deleted.contains(&"master".to_string()));

        // Verify master is still there.
        let remaining = branch_repo.list_branches().expect("list branches");
        let names: Vec<&str> = remaining.iter().map(|b| b.0.as_str()).collect();
        assert!(names.contains(&"master"));
    }

    #[test]
    fn gc_preserves_current_branch_not_in_git() {
        // Create a temp git repo with a feature branch.
        let git_dir = tempfile::tempdir().expect("tempdir");
        let repo = git_dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");
        fs::write(repo.join("README.md"), "# Test").expect("write file");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");
        std::process::Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(&repo)
            .output()
            .expect("git checkout feature");
        // Delete the feature branch in git so it doesn't exist in git anymore.
        std::process::Command::new("git")
            .args(["branch", "-D", "feature"])
            .current_dir(&repo)
            .output()
            .expect("git branch -D feature");
        // Checkout main so HEAD points to main.
        std::process::Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&repo)
            .output()
            .expect("git checkout main");

        // Create a DB with main and feature-branch.
        let db_dir = tempfile::tempdir().expect("tempdir");
        let db_path = db_dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open db");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let file_repo = SqliteFileIRRepository::new(db.connection().clone());

        branch_repo
            .switch_branch(&BranchId::from("main"))
            .expect("switch to main");

        use seshat_core::test_helpers::make_project_file;
        let file = make_project_file(seshat_core::Language::Rust);
        file_repo
            .upsert(&BranchId::from("main"), &file, None)
            .expect("upsert file");

        // Snapshot main to feature-branch
        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("feature-branch"))
            .expect("snapshot feature-branch");

        // The current git branch is "main", so feature-branch should be deleted.
        // But we need to verify that the CURRENT branch (main) is preserved.
        let deleted = gc_branch_snapshots(&db, &repo).expect("gc");
        assert!(
            !deleted.contains(&"main".to_string()),
            "main should be preserved as current branch"
        );
        assert!(
            deleted.contains(&"feature-branch".to_string()),
            "feature-branch should be deleted (not current, not in git, not protected)"
        );

        let remaining = branch_repo.list_branches().expect("list branches");
        let names: Vec<&str> = remaining.iter().map(|b| b.0.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(!names.contains(&"feature-branch"));
    }

    #[test]
    fn gc_handles_detached_head() {
        // Create a temp git repo, make a commit, then detach HEAD.
        let git_dir = tempfile::tempdir().expect("tempdir");
        let repo = git_dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");
        fs::write(repo.join("README.md"), "# Test").expect("write file");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");
        // Detach HEAD
        std::process::Command::new("git")
            .args(["checkout", "--detach", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git checkout detach");

        // Create a DB with main and some-branch.
        let db_dir = tempfile::tempdir().expect("tempdir");
        let db_path = db_dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open db");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let file_repo = SqliteFileIRRepository::new(db.connection().clone());

        branch_repo
            .switch_branch(&BranchId::from("main"))
            .expect("switch to main");

        use seshat_core::test_helpers::make_project_file;
        let file = make_project_file(seshat_core::Language::Rust);
        file_repo
            .upsert(&BranchId::from("main"), &file, None)
            .expect("upsert file");

        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("some-branch"))
            .expect("snapshot some-branch");

        // In detached HEAD state, get_current_branch returns a commit hash.
        // main should still be preserved as a protected branch.
        let deleted = gc_branch_snapshots(&db, &repo).expect("gc");
        assert!(
            !deleted.contains(&"main".to_string()),
            "main should be preserved even in detached HEAD"
        );
        assert!(
            deleted.contains(&"some-branch".to_string()),
            "some-branch should be deleted"
        );

        let remaining = branch_repo.list_branches().expect("list branches");
        let names: Vec<&str> = remaining.iter().map(|b| b.0.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(!names.contains(&"some-branch"));
    }

    #[test]
    fn gc_deletes_all_orphans() {
        // Create a temp git repo with only main.
        let git_dir = tempfile::tempdir().expect("tempdir");
        let repo = git_dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");
        fs::write(repo.join("README.md"), "# Test").expect("write file");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");

        // Create a DB with main and multiple orphan branches.
        let db_dir = tempfile::tempdir().expect("tempdir");
        let db_path = db_dir.path().join("test.db");
        let db = Database::open(&db_path).expect("open db");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let file_repo = SqliteFileIRRepository::new(db.connection().clone());

        branch_repo
            .switch_branch(&BranchId::from("main"))
            .expect("switch to main");

        use seshat_core::test_helpers::make_project_file;
        let file = make_project_file(seshat_core::Language::Rust);
        file_repo
            .upsert(&BranchId::from("main"), &file, None)
            .expect("upsert file");

        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("orphan-1"))
            .expect("snapshot orphan-1");
        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("orphan-2"))
            .expect("snapshot orphan-2");
        branch_repo
            .create_snapshot(&BranchId::from("main"), &BranchId::from("orphan-3"))
            .expect("snapshot orphan-3");

        // Run GC — all orphans should be deleted, main preserved.
        let deleted = gc_branch_snapshots(&db, &repo).expect("gc");
        assert_eq!(deleted.len(), 3, "should delete all 3 orphans");
        assert!(deleted.contains(&"orphan-1".to_string()));
        assert!(deleted.contains(&"orphan-2".to_string()));
        assert!(deleted.contains(&"orphan-3".to_string()));
        assert!(!deleted.contains(&"main".to_string()));

        let remaining = branch_repo.list_branches().expect("list branches");
        let names: Vec<&str> = remaining.iter().map(|b| b.0.as_str()).collect();
        assert_eq!(names.len(), 1);
        assert!(names.contains(&"main"));
        assert!(!names.contains(&"orphan-1"));
        assert!(!names.contains(&"orphan-2"));
        assert!(!names.contains(&"orphan-3"));
    }

    #[test]
    fn detect_branch_normal_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");
        fs::write(repo.join("README.md"), "# Test").expect("write file");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");

        let branch = detect_branch(&repo);
        assert_eq!(branch, "main");
    }

    #[test]
    fn detect_branch_worktree_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main_repo = dir.path().join("main-repo");
        fs::create_dir_all(&main_repo).expect("create main repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&main_repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&main_repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&main_repo)
            .output()
            .expect("git config name");
        fs::write(main_repo.join("README.md"), "# Main").expect("write");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&main_repo)
            .output()
            .expect("git commit");
        // Create a test branch to base the worktree on.
        std::process::Command::new("git")
            .args(["branch", "wt-test-branch-1"])
            .current_dir(&main_repo)
            .output()
            .expect("git branch wt-test-branch-1");

        let worktree = dir.path().join("wt-on-test");
        let status = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                worktree.to_str().unwrap(),
                "wt-test-branch-1",
            ])
            .current_dir(&main_repo)
            .status()
            .expect("git worktree add wt-test-branch-1");
        assert!(status.success(), "git worktree add wt-test-branch-1 failed");

        let branch = detect_branch(&worktree);
        assert_eq!(branch, "wt-test-branch-1");
    }

    #[test]
    fn detect_branch_worktree_nested() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main_repo = dir.path().join("main-repo");
        fs::create_dir_all(&main_repo).expect("create main repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&main_repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&main_repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&main_repo)
            .output()
            .expect("git config name");
        fs::write(main_repo.join("README.md"), "# Main").expect("write");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&main_repo)
            .output()
            .expect("git commit");
        std::process::Command::new("git")
            .args(["branch", "wt-test-branch-2"])
            .current_dir(&main_repo)
            .output()
            .expect("git branch wt-test-branch-2");

        let worktree = dir.path().join("wt-nested-on-test");
        let status = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                worktree.to_str().unwrap(),
                "wt-test-branch-2",
            ])
            .current_dir(&main_repo)
            .status()
            .expect("git worktree add wt-test-branch-2");
        assert!(status.success(), "git worktree add wt-test-branch-2 failed");

        let subdir = worktree.join("src").join("api");
        fs::create_dir_all(&subdir).expect("create subdir");

        let branch = detect_branch(&subdir);
        assert_eq!(branch, "wt-test-branch-2");
    }

    #[test]
    fn detect_branch_detached_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("test-repo");
        fs::create_dir_all(&repo).expect("create repo");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .output()
            .expect("git config email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .expect("git config name");
        fs::write(repo.join("file.txt"), "content").expect("write");
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&repo)
            .output()
            .expect("git commit");
        std::process::Command::new("git")
            .args(["checkout", "--detach", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git checkout detach");

        let branch = detect_branch(&repo);
        assert_eq!(branch.len(), 40);
        assert!(branch.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn detect_branch_no_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        let no_git = dir.path().join("no-git-project");
        fs::create_dir_all(&no_git).expect("create dir");

        let branch = detect_branch(&no_git);
        assert_eq!(branch, "main");
    }
}
