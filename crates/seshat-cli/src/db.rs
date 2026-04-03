//! Shared database path utilities used by both `scan` and `serve` commands.
//!
//! All Seshat databases live in `$XDG_DATA_HOME/seshat/repos/{project_name}.db`
//! (typically `~/.local/share/seshat/repos/` on Linux/macOS).

use std::path::{Path, PathBuf};

use crate::error::CliError;

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
#[allow(dead_code)] // Will be used in US-004 (submodule scan flow)
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

/// Walk up from `from` to find the nearest `.git` directory.
///
/// Returns the parent of `.git` (the repository root).
/// Returns `None` if no `.git` is found before reaching the filesystem root.
pub(crate) fn find_git_root(from: &Path) -> Option<PathBuf> {
    let mut current = if from.is_absolute() {
        from.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(from)
    };

    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
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

/// Resolve the database path for `seshat serve`.
///
/// Priority chain:
/// 1. Explicit `repo` argument (path to directory or project name)
/// 2. Current working directory name → `{name}.db`
/// 3. Walk up to git root → `{repo_name}.db`
/// 4. Single DB in repos dir → use unambiguously
/// 5. Multiple DBs / no match → error with list
pub(crate) fn resolve_serve_db(explicit_repo: Option<&Path>) -> Result<PathBuf, CliError> {
    let repos_dir = xdg_repos_dir()?;

    // Priority 1: explicit repo argument
    if let Some(repo_arg) = explicit_repo {
        return resolve_explicit_repo(&repos_dir, repo_arg);
    }

    // Priority 2: current working directory
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_name = project_name(&cwd);
        let cwd_db = repos_dir.join(format!("{cwd_name}.db"));
        if cwd_db.exists() {
            tracing::info!(project = %cwd_name, "Auto-detected project from working directory");
            return Ok(cwd_db);
        }

        // Priority 3: walk up to git root
        if let Some(git_root) = find_git_root(&cwd) {
            let repo_name = project_name(&git_root);
            let repo_db = repos_dir.join(format!("{repo_name}.db"));
            if repo_db.exists() {
                tracing::info!(
                    project = %repo_name,
                    git_root = %git_root.display(),
                    "Auto-detected project from git root"
                );
                return Ok(repo_db);
            }
        }
    }

    // Priority 4/5: check available projects
    let projects = list_available_projects(&repos_dir)?;

    match projects.len() {
        0 => Err(CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: "no scanned projects found.\n\
                     hint: run `seshat scan <path>` first to index a project"
                .to_owned(),
        }),
        1 => {
            let (ref path, ref name) = projects[0];
            tracing::info!(project = %name, "Auto-selected only available project");
            Ok(path.clone())
        }
        _ => {
            let project_list = projects
                .iter()
                .map(|(_, name)| format!("    \u{2022} {name}"))
                .collect::<Vec<_>>()
                .join("\n");

            Err(CliError::CommandFailed {
                command: "serve".to_owned(),
                reason: format!(
                    "could not determine which project to serve.\n\n\
                     Available scanned projects:\n\
                     {project_list}\n\n\
                     hint: run from the project directory, or specify:\n\
                     \x20     seshat serve <project-name>\n\
                     \x20     seshat serve <path-to-project>"
                ),
            })
        }
    }
}

/// Resolve an explicit repo argument — either a directory path or a project name.
fn resolve_explicit_repo(repos_dir: &Path, repo_arg: &Path) -> Result<PathBuf, CliError> {
    // If it's an existing directory, extract the project name from it
    if repo_arg.is_dir() {
        let name = project_name(repo_arg);
        let db = repos_dir.join(format!("{name}.db"));
        if db.exists() {
            return Ok(db);
        }
        return Err(CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!(
                "project '{}' has not been scanned.\n\
                 hint: run `seshat scan {}` first",
                name,
                repo_arg.display()
            ),
        });
    }

    // Otherwise, treat as a project name
    let name = repo_arg.to_string_lossy();
    let db = repos_dir.join(format!("{name}.db"));
    if db.exists() {
        return Ok(db);
    }

    // Maybe it's a path that doesn't exist as a directory
    // (e.g., ~/Projects/deleted-project) — extract name and try
    let name_from_path = project_name(repo_arg);
    let db_from_path = repos_dir.join(format!("{name_from_path}.db"));
    if db_from_path.exists() {
        return Ok(db_from_path);
    }

    Err(CliError::CommandFailed {
        command: "serve".to_owned(),
        reason: format!(
            "project '{name}' has not been scanned.\n\
             hint: run `seshat scan <path>` first to index it"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    fn resolve_explicit_repo_by_name() {
        let (_tmp, repos) = setup_repos_dir();
        fs::write(repos.join("my-project.db"), "").unwrap();

        let result = resolve_explicit_repo(&repos, Path::new("my-project"));
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("my-project.db"));
    }

    #[test]
    fn resolve_explicit_repo_not_scanned() {
        let (_tmp, repos) = setup_repos_dir();

        let result = resolve_explicit_repo(&repos, Path::new("nonexistent"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not been scanned"));
    }

    #[test]
    fn resolve_explicit_repo_by_directory() {
        let (tmp, repos) = setup_repos_dir();
        let project_dir = tmp.path().join("my-project");
        fs::create_dir(&project_dir).unwrap();
        fs::write(repos.join("my-project.db"), "").unwrap();

        let result = resolve_explicit_repo(&repos, &project_dir);
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("my-project.db"));
    }

    #[test]
    fn resolve_submodule_db_path_creates_parent_dirs() {
        // We can't easily test the real XDG path without mocking,
        // but we can verify the function returns the expected structure
        // by checking the path format.
        let result = resolve_submodule_db_path("my-app", "libs/shared");
        assert!(result.is_ok());
        let path = result.unwrap();
        // Path should end with my-app/libs/shared.db
        assert!(
            path.ends_with("my-app/libs/shared.db"),
            "Expected path ending with my-app/libs/shared.db, got: {}",
            path.display()
        );
    }

    #[test]
    fn resolve_submodule_db_path_simple_mount() {
        let result = resolve_submodule_db_path("my-app", "frontend");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(
            path.ends_with("my-app/frontend.db"),
            "Expected path ending with my-app/frontend.db, got: {}",
            path.display()
        );
    }
}
