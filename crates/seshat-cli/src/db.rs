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

/// Resolved project information returned by the single shared resolver.
///
/// Used by every CLI command that needs to locate a project (scan, serve,
/// review, status, decisions, uninstall, debug). Whether the DB file exists
/// on disk is NOT checked here — the caller decides how to handle a missing
/// database.
///
/// **Worktree-stable identity.** [`Self::project_name`] is derived from the
/// git common-dir's basename (when available), so all worktrees of one git
/// repository resolve to the same [`Self::db_path`]. [`Self::project_root`]
/// remains the working tree directory the caller is operating on, so file
/// reads still happen in the worktree the user is sitting in.
pub struct ResolvedProject {
    /// Working tree directory — where source files are read from.
    /// For git worktrees, this is the worktree directory (NOT the main repo
    /// root). For non-git directories, this is the canonicalised input path.
    pub project_root: PathBuf,
    /// Main repository root, where `.git` lives. Same for every worktree of
    /// a single repository. `None` for non-git directories.
    pub git_root: Option<PathBuf>,
    /// Stable DB filename stem. Derived from `git_root.file_name()` when
    /// available, otherwise from `project_root.file_name()`.
    pub project_name: String,
    /// Full DB path: `xdg_repos_dir / "{project_name}.db"`. May or may not
    /// exist on disk.
    pub db_path: PathBuf,
}

impl ResolvedProject {
    /// Root used for git operations (gix open, tree-diff, ref resolution,
    /// branch GC). Falls back to [`Self::project_root`] for non-git
    /// directories so callers get a usable path either way.
    pub fn sync_root(&self) -> &Path {
        self.git_root.as_deref().unwrap_or(&self.project_root)
    }
}

/// Walk up from `path` to the git common-dir parent, falling back to `path`
/// itself when no git root is found.
///
/// Single helper for callers that have a plain `&Path` instead of a
/// [`ResolvedProject`] (test fixtures, watcher callbacks, freshness gate
/// helpers). Production CLI paths should prefer
/// [`ResolvedProject::sync_root`] which avoids the redundant walk.
pub fn sync_root_for(path: &Path) -> PathBuf {
    find_git_root(path).unwrap_or_else(|| path.to_path_buf())
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

/// Read the HEAD file directly and extract branch name (or commit hash for
/// detached HEAD).
///
/// Handles both normal repos (`.git` is a directory) and worktrees
/// (`.git` is a file containing `gitdir: <path>`).
fn read_head_file(path: &Path) -> Option<String> {
    let gitdir = resolve_gitdir(path)?;
    read_head_in_gitdir(&gitdir)
}

/// Resolve the on-disk gitdir for `path`, following the worktree `.git`-file
/// indirection when needed.
fn resolve_gitdir(path: &Path) -> Option<PathBuf> {
    let git_dir = find_git_dir(path)?;
    match git_dir {
        GitDir::Dir(dir) => Some(dir),
        GitDir::File(file) => {
            let content = std::fs::read_to_string(&file).ok()?;
            let gitdir = content.strip_prefix("gitdir: ")?.trim();
            let gitdir_path = PathBuf::from(gitdir);
            if gitdir_path.is_absolute() {
                Some(gitdir_path)
            } else {
                Some(file.parent()?.join(gitdir_path))
            }
        }
    }
}

/// Locate the `.git` directory or file, walking up from `path`.
pub(crate) enum GitDir {
    Dir(PathBuf),
    File(PathBuf),
}

pub(crate) fn find_git_dir(path: &Path) -> Option<GitDir> {
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };

    for _ in 0..GIT_ROOT_MAX_ITERATIONS {
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

    tracing::warn!(
        path = %path.display(),
        "find_git_dir reached iteration limit; possible symlink cycle"
    );
    None
}

/// Read the HEAD file at `<gitdir>/HEAD` and parse it into either a branch
/// name (`refs/heads/X` → `X`) or a commit hash (detached HEAD).
///
/// Single source of truth for HEAD parsing — used by [`read_head_file`]
/// (which resolves the gitdir via worktree-aware indirection first).
fn read_head_in_gitdir(gitdir: &Path) -> Option<String> {
    let content = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;

    if let Some(rest) = content.strip_prefix("ref: ") {
        if let Some(branch) = rest.trim().strip_prefix("refs/heads/") {
            return Some(branch.to_string());
        }
    }

    // Detached HEAD — content is a commit hash. Accept both full (40-char)
    // and abbreviated hashes (>= 7 chars).
    let trimmed = content.trim();
    if trimmed.len() >= 7 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
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

/// Build a [`ResolvedProject`] for a directory on disk.
///
/// Walks up to the git common-dir parent via [`find_git_root`]
/// (worktree-aware) and uses ITS basename as the project name, so all
/// worktrees of one repository resolve to the same DB. For non-git
/// directories, falls back to the canonicalised input directory's basename.
fn identity_from_dir(input: &Path) -> Result<ResolvedProject, CliError> {
    let canonical = input.canonicalize().unwrap_or_else(|_| input.to_path_buf());
    let git_root = find_git_root(&canonical);
    let name_source = git_root.as_deref().unwrap_or(&canonical);
    let project_name = project_name(name_source);
    let repos_dir = xdg_repos_dir()?;
    let db_path = repos_dir.join(format!("{project_name}.db"));
    Ok(ResolvedProject {
        project_root: canonical,
        git_root,
        project_name,
        db_path,
    })
}

/// Build a [`ResolvedProject`] from a stored DB plus its scan-time
/// `project_root` metadata. Used by name-based and auto-select fallbacks
/// where the on-disk DB is the source of truth for "where this project
/// lives". Re-derives `git_root` from the stored root so sync works even
/// when the DB was originally scanned from a worktree directory.
fn identity_from_db(
    project_name: String,
    db_path: PathBuf,
    stored_root: Option<PathBuf>,
) -> ResolvedProject {
    let project_root = stored_root.unwrap_or_else(|| {
        db_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let git_root = find_git_root(&project_root);
    ResolvedProject {
        project_root,
        git_root,
        project_name,
        db_path,
    }
}

/// Common project resolution logic used by every CLI command.
///
/// The resolver walks to the git common-dir parent BEFORE deriving the
/// project name, so all worktrees of a single repository share one DB. The
/// caller decides whether a missing DB is an error.
///
/// `command_name` is used in error messages to identify the calling command.
///
/// Resolution priority:
/// 1. Explicit argument that names an existing directory → resolve from it.
/// 2. Explicit argument that names a known project (DB exists in repos_dir)
///    → recover scan-time root from DB metadata.
/// 3. No argument: derive from cwd. The DB is keyed by the cwd's git-root
///    basename so worktrees collapse to one DB.
/// 4. cwd is not in a git repo and has no DB → fall back to listing
///    available projects (auto-select when exactly one exists).
pub fn resolve_project(
    explicit_path: Option<&Path>,
    command_name: &str,
) -> Result<ResolvedProject, CliError> {
    // Priority 1: explicit argument.
    if let Some(arg) = explicit_path {
        // 1a. Existing directory — resolve from disk.
        if arg.is_dir() {
            return identity_from_dir(arg);
        }

        // 1b. Project NAME lookup — resolve via stored root in DB metadata.
        let repos_dir = xdg_repos_dir()?;
        let name = arg.to_string_lossy().to_string();
        let by_name = repos_dir.join(format!("{name}.db"));
        if by_name.is_file() {
            let stored = read_project_root_from_db(&by_name);
            return Ok(identity_from_db(name, by_name, stored));
        }

        // 1c. Maybe a path-like that doesn't exist; try its basename as a
        //     project name (consistent with how scan would have stored it).
        let name_from_path = project_name(arg);
        let by_path_name = repos_dir.join(format!("{name_from_path}.db"));
        if by_path_name.is_file() {
            let stored = read_project_root_from_db(&by_path_name);
            return Ok(identity_from_db(name_from_path, by_path_name, stored));
        }

        // 1d. Unknown — surface a hint to scan first.
        return Err(CliError::CommandFailed {
            command: command_name.to_owned(),
            reason: format!(
                "project '{}' has not been found.\n\
                 hint: run `seshat scan {}` first",
                name,
                arg.display()
            ),
        });
    }

    // Priority 2: derive from cwd. Whether the DB exists or not, return
    // the cwd-derived identity — the caller decides how to handle a missing
    // DB (e.g. `scan` creates it, `review` errors with "No database found").
    if let Ok(cwd) = std::env::current_dir() {
        let identity = identity_from_dir(&cwd)?;
        if identity.db_path.is_file() {
            tracing::info!(
                project = %identity.project_name,
                "Auto-detected project from cwd"
            );
        }
        return Ok(identity);
    }

    // Priority 3: cwd is unreadable (deleted-while-running, EACCES, …).
    // Last-resort fallback — list whatever's in repos_dir and auto-select
    // when there's exactly one. Otherwise surface a helpful list.
    let repos_dir = xdg_repos_dir()?;
    let projects = list_available_projects(&repos_dir)?;
    match projects.len() {
        0 => Err(CliError::CommandFailed {
            command: command_name.to_owned(),
            reason: "no scanned projects found.\n\
                   hint: run `seshat scan <path>` first to index a project"
                .to_string(),
        }),
        1 => {
            let (path, name) = &projects[0];
            tracing::info!(project = %name, "Auto-selected only available project");
            let stored = read_project_root_from_db(path);
            Ok(identity_from_db(name.clone(), path.clone(), stored))
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

/// Build the multi-line user-facing hint embedded in
/// [`CliError::DangerousCwd`] errors. Lists three concrete next steps.
///
/// `seshat serve` accepts an optional positional `<repo>` argument (see
/// `args.rs`), so the override hint shows the positional form rather than a
/// `--repo` flag.
pub(crate) fn build_dangerous_cwd_hint() -> String {
    concat!(
        "Suggestions:\n",
        "  • Change to a real project directory: cd /path/to/your/project\n",
        "  • Index a specific path: seshat scan /path/to/project\n",
        "  • Bypass this guardrail by passing the path explicitly: seshat serve /path/to/project",
    )
    .to_owned()
}

/// Build the multi-line stderr warning emitted when the user passed an
/// explicit positional `<repo>` pointing at a dangerous, non-git location.
///
/// The override is non-fatal: the user opted in by being explicit, so we
/// only warn and continue.
pub(crate) fn build_repo_override_warning(project_root: &Path) -> String {
    format!(
        concat!(
            "⚠️  Serving from a dangerous location: {}\n",
            "   This path is on the dangerous-cwd denylist (e.g. $HOME, ~/Library, /, drive roots).\n",
            "   Proceeding because an explicit repo path was passed. Watch memory usage on large trees.",
        ),
        project_root.display()
    )
}

/// Pure decision: should `serve` refuse to run because `cwd` is dangerous and
/// not inside a git repository?
///
/// Refuses only when `explicit_repo.is_none()`. When the user passed `--repo`,
/// the caller should instead use [`check_repo_override_dangerous`] to decide
/// whether to emit a warning.
pub(crate) fn check_serve_dangerous_cwd(
    explicit_repo: Option<&Path>,
    additional: &[String],
    cwd: &Path,
    home: Option<&Path>,
) -> Result<(), CliError> {
    if explicit_repo.is_some() {
        return Ok(());
    }
    if !crate::dangerous_path::is_dangerous_cwd_with_home(cwd, additional, home) {
        return Ok(());
    }
    // Even in a dangerous cwd, allow proceeding when there is a git
    // repository at-or-above us — UNLESS the resolved git root IS itself
    // a denylist entry (e.g. a stray `~/.git` from a dotfiles repo, where
    // `find_git_root($HOME/scratch)` walks up and lands on `$HOME`). A
    // legitimate project nested inside `$HOME` (e.g. `~/work/myproj`)
    // resolves to a git root that is a *descendant* of the denylist
    // entry, not equal to one — so it correctly stays allowed.
    if let Some(git_root) = find_git_root(cwd) {
        if !crate::dangerous_path::is_exact_denylist_entry(&git_root, additional, home) {
            return Ok(());
        }
        tracing::warn!(
            cwd = %cwd.display(),
            git_root = %git_root.display(),
            "found .git exactly at a denylist root; ignoring it for guard purposes"
        );
    }
    Err(CliError::DangerousCwd {
        path: cwd.to_path_buf(),
        hint: build_dangerous_cwd_hint(),
    })
}

/// Pure decision: when `--repo` was passed and the resolved `project_root`
/// is on the dangerous denylist with no nearby git repository, return a
/// warning string. Otherwise return `None`.
///
/// Mirrors the "dangerous && no-git" condition used by
/// [`check_serve_dangerous_cwd`]; the difference is that this case is
/// non-fatal — the user explicitly opted in via `--repo`.
pub(crate) fn check_repo_override_dangerous(
    explicit_repo: Option<&Path>,
    additional: &[String],
    project_root: &Path,
    home: Option<&Path>,
) -> Option<String> {
    explicit_repo?;
    if !crate::dangerous_path::is_dangerous_cwd_with_home(project_root, additional, home) {
        return None;
    }
    // A real git repo at-or-above `project_root` means the user pointed
    // their explicit `<repo>` at a project, not at a dangerous tree root —
    // no warn needed. We require the resolved git root to NOT be exactly
    // a denylist entry (mirrors the logic in [`check_serve_dangerous_cwd`]:
    // a stray `.git` at `$HOME` does not retroactively make `$HOME` safe).
    if let Some(git_root) = find_git_root(project_root) {
        if !crate::dangerous_path::is_exact_denylist_entry(&git_root, additional, home) {
            return None;
        }
    }
    Some(build_repo_override_warning(project_root))
}

/// Resolves what to serve — either an existing database or a project root that
/// needs auto-scanning.
///
/// When no `.db` file is found, instead of erroring, this function determines
/// the project root and returns `ServeTarget::AutoScan`. The caller can then
/// create an empty DB and launch a background scan.
///
/// `additional_denylist_paths` extends the per-OS dangerous-cwd denylist used
/// to gate auto-scan in unsafe locations (see [`check_serve_dangerous_cwd`]).
pub(crate) fn resolve_serve_db_or_project_root(
    explicit_repo: Option<&Path>,
    additional_denylist_paths: &[String],
) -> Result<ServeTarget, CliError> {
    // Refuse early when invoked from a dangerous cwd with no nearby git repo.
    //
    // Fail closed if the cwd cannot be read (deleted-while-running, EACCES,
    // etc.): silently skipping the guard would let a process with an
    // unreadable cwd evade the entire P1 protection.
    if explicit_repo.is_none() {
        let cwd = std::env::current_dir().map_err(|e| CliError::IoWithPath {
            message: format!("could not read current working directory: {e}"),
            path: PathBuf::from("."),
        })?;
        check_serve_dangerous_cwd(
            explicit_repo,
            additional_denylist_paths,
            &cwd,
            dirs::home_dir().as_deref(),
        )?;
    }

    let resolved = resolve_project(explicit_repo, "serve")?;

    // Warn (but proceed) when an explicit `<repo>` arg points at a
    // dangerous, non-git path. We use `tracing::warn!` rather than `eprintln!`
    // so the warning flows through the normal logging pipeline (JSON
    // subscribers, log aggregators, level filtering all work). The default
    // tracing-subscriber writes WARN to stderr, so user-visible behaviour
    // is unchanged for plain CLI invocations.
    if let Some(warning) = check_repo_override_dangerous(
        explicit_repo,
        additional_denylist_paths,
        &resolved.project_root,
        dirs::home_dir().as_deref(),
    ) {
        tracing::warn!("{warning}");
    }

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

        // The unified resolver canonicalises the input path so worktree
        // resolution is symlink-stable; tests must compare against the
        // canonical form (on macOS `/var/folders/...` → `/private/var/...`).
        let expected_root = std::fs::canonicalize(&project_dir).unwrap();

        // Explicit directory with no existing DB → AutoScan.
        let result = resolve_serve_db_or_project_root(Some(&project_dir), &[]);
        assert!(result.is_ok());
        match result.unwrap() {
            ServeTarget::AutoScan {
                project_root,
                db_path,
            } => {
                assert_eq!(project_root, expected_root);
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
        // Make sure the XDG repos dir exists — on a fresh CI runner it may
        // not have been created yet.
        fs::create_dir_all(&repos_dir).expect("create repos dir");
        let _cleanup = CleanupDir(repos_dir.join("_test_serve_existing"));

        let project_name = "_test_serve_existing";
        let db_path = repos_dir.join(format!("{project_name}.db"));
        fs::write(&db_path, "").unwrap();

        let project_dir = tempfile::tempdir().expect("temp dir");

        let result = resolve_serve_db_or_project_root(
            Some(project_dir.path().join(project_name).as_path()),
            &[],
        );
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

        let expected_root = std::fs::canonicalize(&project_dir).unwrap();

        // Explicit directory path with no DB and no git → AutoScan with cwd.
        let result = resolve_serve_db_or_project_root(Some(&project_dir), &[]);
        assert!(result.is_ok());
        match result.unwrap() {
            ServeTarget::AutoScan { project_root, .. } => {
                assert_eq!(project_root, expected_root);
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
        // Ensure the XDG repos dir exists on fresh CI runners.
        fs::create_dir_all(&repos_dir).expect("create repos dir");
        let db_path = repos_dir.join("my-project.db");
        let _cleanup = CleanupDir(db_path.clone());
        fs::write(&db_path, "").unwrap();

        // Resolve — should be ExistingDb with project_root = the actual project dir
        let result = resolve_serve_db_or_project_root(Some(&project_dir), &[]);
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());

        let (resolved_root, db_file) = match result.unwrap() {
            ServeTarget::ExistingDb {
                project_root,
                db_path,
            } => (project_root, db_path),
            _ => panic!("Expected ExistingDb"),
        };

        // project_root should be the canonical project directory (worktree
        // resolution canonicalises the input).
        let expected_root = std::fs::canonicalize(&project_dir).unwrap();
        assert_eq!(resolved_root, expected_root);
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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
            .args(["init", "-b", "main"])
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

    // ── unix_now / xdg_repos_dir / path resolvers ───────────────────

    #[test]
    fn unix_now_returns_recent_timestamp() {
        // Sanity check: should be a positive value and >= a known baseline
        // (year 2025-01-01 UTC = 1735689600). We bumped well past that.
        let now = unix_now();
        assert!(
            now > 1_735_689_600,
            "expected post-2025 unix time, got {now}"
        );
    }

    #[test]
    fn xdg_repos_dir_path_shape() {
        let dir = xdg_repos_dir().expect("should resolve");
        assert!(dir.ends_with("repos"));
        assert!(dir.parent().unwrap().ends_with("seshat"));
    }

    #[test]
    fn resolved_project_uses_project_filename_for_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("my-app");
        fs::create_dir_all(&project).unwrap();
        // Non-git directory → project_name = file_name, db_path =
        // <repos_dir>/my-app.db. git_root is None.
        let resolved = resolve_project(Some(&project), "test").expect("resolve");
        assert_eq!(resolved.project_name, "my-app");
        assert_eq!(
            resolved.db_path.file_name().unwrap().to_string_lossy(),
            "my-app.db"
        );
        assert!(resolved.db_path.parent().unwrap().ends_with("repos"));
        assert!(resolved.git_root.is_none());
    }

    #[test]
    fn resolve_submodule_db_path_creates_parent_and_uses_mount() {
        // Use a unique name to avoid colliding with user's real seshat data dir.
        let unique = format!("seshat-test-{}", unix_now());
        let result = resolve_submodule_db_path(&unique, "libs/shared").expect("resolve");
        assert!(result.ends_with("libs/shared.db"));
        // Parent dir must exist now (resolve_submodule_db_path creates it).
        let parent = result.parent().unwrap();
        assert!(parent.is_dir(), "parent dir should be created: {parent:?}");
        // Cleanup so we don't leak per-test directories under the user's data dir.
        if let Some(repos) = parent.parent() {
            if repos.file_name().and_then(|s| s.to_str()) == Some(&unique) {
                let _ = fs::remove_dir_all(repos);
            }
        }
    }

    // ── count_files_any_schema / count_conventions / load_project_info ──

    #[test]
    fn count_files_any_schema_empty_db_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("c.db")).unwrap();
        assert_eq!(count_files_any_schema(&db, "main"), 0);
    }

    #[test]
    fn count_conventions_empty_db_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("c.db")).unwrap();
        assert_eq!(count_conventions(&db, "main"), 0);
    }

    #[test]
    fn count_conventions_seeded_returns_count() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("c.db")).unwrap();
        {
            let g = db.connection().lock().unwrap();
            for desc in &["a", "b", "c"] {
                g.execute(
                    "INSERT INTO nodes (branch_id, nature, weight, confidence,
                       adoption_count, total_count, description, ext_data)
                     VALUES ('main', 'convention', 'strong', 0.9, 1, 1, ?1, NULL)",
                    params![*desc],
                )
                .unwrap();
            }
        }
        assert_eq!(count_conventions(&db, "main"), 3);
        assert_eq!(count_conventions(&db, "other"), 0);
    }

    #[test]
    fn load_project_info_defaults_for_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("c.db")).unwrap();
        let info = load_project_info(&db);
        // No git repo, no data — branch should default to "main".
        assert_eq!(info.branch.0, "main");
        assert_eq!(info.file_count, 0);
        assert_eq!(info.convention_count, 0);
    }

    // ── read_head_in_gitdir ───────────────────────────────────────

    #[test]
    fn read_head_in_gitdir_ref_form() {
        let dir = tempfile::tempdir().unwrap();
        let gitdir = dir.path();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature/my-branch\n").unwrap();
        let result = read_head_in_gitdir(gitdir);
        assert_eq!(result.as_deref(), Some("feature/my-branch"));
    }

    #[test]
    fn read_head_in_gitdir_detached_full_hash() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        let result = read_head_in_gitdir(dir.path());
        assert_eq!(
            result.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
    }

    #[test]
    fn read_head_in_gitdir_detached_abbreviated_hash() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("HEAD"), "deadbee\n").unwrap();
        let result = read_head_in_gitdir(dir.path());
        assert_eq!(result.as_deref(), Some("deadbee"));
    }

    #[test]
    fn read_head_in_gitdir_unknown_ref_namespace_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        // Refs outside refs/heads/ (e.g. tag refs) are not branches.
        fs::write(dir.path().join("HEAD"), "ref: refs/tags/v1.0\n").unwrap();
        assert!(read_head_in_gitdir(dir.path()).is_none());
    }

    #[test]
    fn read_head_in_gitdir_garbage_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("HEAD"), "not a hash and not a ref").unwrap();
        assert!(read_head_in_gitdir(dir.path()).is_none());
    }

    #[test]
    fn read_head_in_gitdir_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        // No HEAD file at all.
        assert!(read_head_in_gitdir(dir.path()).is_none());
    }

    // ── find_git_dir ────────────────────────────────────────────────

    #[test]
    fn find_git_dir_returns_dir_variant_when_dotgit_is_directory() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        fs::create_dir_all(project.join(".git").join("subdir")).unwrap();
        match find_git_dir(&project) {
            Some(GitDir::Dir(p)) => assert!(p.ends_with(".git")),
            Some(GitDir::File(_)) => panic!("expected GitDir::Dir, got File"),
            None => panic!("expected GitDir::Dir, got None"),
        }
    }

    #[test]
    fn find_git_dir_returns_file_variant_when_dotgit_is_file() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("wt");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: /tmp/some-elsewhere").unwrap();
        match find_git_dir(&worktree) {
            Some(GitDir::File(p)) => assert!(p.ends_with(".git")),
            Some(GitDir::Dir(_)) => panic!("expected GitDir::File, got Dir"),
            None => panic!("expected GitDir::File, got None"),
        }
    }

    #[test]
    fn find_git_dir_walks_up_from_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        let nested = project.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(project.join(".git")).unwrap();
        let result = find_git_dir(&nested);
        assert!(matches!(result, Some(GitDir::Dir(_))));
    }

    #[test]
    fn find_git_dir_returns_none_when_no_dotgit() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("no-git");
        fs::create_dir_all(&project).unwrap();
        // We can't actually walk up to / and find no .git in CI tempdirs,
        // but at least we can verify it doesn't panic.
        let _ = find_git_dir(&project);
    }

    // ── gc_branch_snapshots ─────────────────────────────────────────

    #[test]
    fn gc_branch_snapshots_empty_db_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("c.db")).unwrap();
        let deleted = gc_branch_snapshots(&db, dir.path()).unwrap();
        assert!(deleted.is_empty());
    }

    // ── dangerous-cwd guardrail (US-003) ────────────────────────────

    /// `home`/`cwd` setup used to simulate "the user invoked seshat from
    /// inside `$HOME` (or a subdir) without a git repo nearby". A directory
    /// is created under the fake home and returned along with the home dir.
    fn fake_home_with_subdir(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let home = tmp.path().to_path_buf();
        let cwd = home.join(name);
        fs::create_dir_all(&cwd).expect("create cwd subdir");
        (tmp, home, cwd)
    }

    #[test]
    fn check_serve_dangerous_cwd_refuses_when_in_home_with_no_git() {
        let (_tmp, home, cwd) = fake_home_with_subdir("scratchpad");
        let result = check_serve_dangerous_cwd(None, &[], &cwd, Some(&home));
        match result {
            Err(CliError::DangerousCwd { path, hint }) => {
                // canonicalize for macOS where /var → /private/var etc.
                let expected = std::fs::canonicalize(&cwd).unwrap_or(cwd.clone());
                let got = std::fs::canonicalize(&path).unwrap_or(path.clone());
                assert_eq!(got, expected, "path should reflect offending cwd");
                assert!(
                    hint.contains("seshat scan"),
                    "hint missing scan suggestion: {hint}"
                );
                assert!(
                    hint.contains("seshat serve /"),
                    "hint missing positional-repo override suggestion: {hint}"
                );
                assert!(hint.contains("cd "), "hint missing cd suggestion: {hint}");
            }
            other => panic!("expected DangerousCwd, got {other:?}"),
        }
    }

    #[test]
    fn check_serve_dangerous_cwd_proceeds_when_inside_git_repo() {
        // cwd is dangerous (under fake home) but ALSO inside a git repo;
        // the gate should allow the caller to proceed (Ok(())).
        let (_tmp, home, cwd) = fake_home_with_subdir("real-project");
        fs::create_dir(cwd.join(".git")).expect("create .git dir");
        let result = check_serve_dangerous_cwd(None, &[], &cwd, Some(&home));
        assert!(
            result.is_ok(),
            "expected Ok when cwd is inside a git repo, got {result:?}"
        );
    }

    #[test]
    fn check_serve_dangerous_cwd_refuses_when_stray_git_lives_at_dangerous_root() {
        // A stray `.git` directory at $HOME (e.g. dotfiles repo) must not
        // retroactively make every $HOME subdir look "safe" — the guard
        // would otherwise be trivially bypassed by anyone with such a setup.
        let (_tmp, home, cwd) = fake_home_with_subdir("scratchpad");
        fs::create_dir(home.join(".git")).expect("create stray .git at home");
        let result = check_serve_dangerous_cwd(None, &[], &cwd, Some(&home));
        match result {
            Err(CliError::DangerousCwd { .. }) => {}
            other => panic!("expected DangerousCwd despite stray ~/.git, got {other:?}"),
        }
    }

    #[test]
    fn check_serve_dangerous_cwd_skipped_when_explicit_repo_provided() {
        // explicit_repo is Some, so the gate must not refuse — even if cwd is
        // both dangerous and not in a git repo. Caller is opting in.
        let (_tmp, home, cwd) = fake_home_with_subdir("scratchpad");
        let safe_repo = PathBuf::from("/totally/unrelated/path");
        let result = check_serve_dangerous_cwd(Some(&safe_repo), &[], &cwd, Some(&home));
        assert!(
            result.is_ok(),
            "explicit --repo must bypass the cwd gate, got {result:?}"
        );
    }

    #[test]
    fn check_repo_override_dangerous_returns_warn_for_dangerous_path_no_git() {
        // explicit_repo=Some(dangerous-no-git): pure decision returns a warn.
        let (_tmp, home, project_root) = fake_home_with_subdir("inside-home");
        let warn =
            check_repo_override_dangerous(Some(&project_root), &[], &project_root, Some(&home));
        let msg = warn.expect("expected warn message for dangerous explicit repo");
        assert!(msg.contains("⚠️"), "warn message missing ⚠️ prefix: {msg}");
        assert!(
            msg.contains("explicit repo path"),
            "warn message must explain the explicit-repo override: {msg}"
        );
        assert!(msg.lines().count() >= 2, "warn must be multi-line: {msg}");
    }

    #[test]
    fn check_repo_override_dangerous_silent_when_project_root_is_git_repo() {
        // cwd is dangerous AND --repo points at a path that has its own .git;
        // because the override-warn helper requires "no git root", we should
        // get None — i.e. proceed silently (PRD: "explicit_repo=Some(safe) →
        // proceeds with no warn", where 'safe' = a real project).
        let (_tmp, home, project_root) = fake_home_with_subdir("real-project");
        fs::create_dir(project_root.join(".git")).expect("create .git");
        let warn =
            check_repo_override_dangerous(Some(&project_root), &[], &project_root, Some(&home));
        assert!(
            warn.is_none(),
            "git-rooted --repo path must not warn, got {warn:?}"
        );
    }

    #[test]
    fn check_repo_override_dangerous_skipped_when_no_explicit_repo() {
        // explicit_repo=None: the override-warning helper must always return
        // None (refusal handling is `check_serve_dangerous_cwd`'s job).
        let (_tmp, home, project_root) = fake_home_with_subdir("inside-home");
        let warn = check_repo_override_dangerous(None, &[], &project_root, Some(&home));
        assert!(warn.is_none(), "no explicit_repo → no override warn");
    }
}
