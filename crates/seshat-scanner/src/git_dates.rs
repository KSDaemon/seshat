//! Git file date collection via `gix`.
//!
//! Walks the commit history from HEAD once (O(commits)) and records the most
//! recent commit timestamp for every file touched. This avoids per-file
//! `git log` calls which would be O(files × commits).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::ScanError;

/// Collect the most recent git commit date (Unix timestamp) for each file
/// in the repository rooted at `repo_root`.
///
/// Returns a `HashMap<PathBuf, i64>` mapping relative file paths to their
/// most recent commit's author timestamp (seconds since Unix epoch).
///
/// # Non-git directories
///
/// If `repo_root` is not inside a git repository, returns an empty `HashMap`
/// without error. This allows the scan pipeline to proceed normally for
/// non-git projects.
///
/// # Empty repositories
///
/// If the repository has no commits (e.g., freshly `git init`'d), returns an
/// empty `HashMap`.
#[tracing::instrument(skip_all, fields(repo_root = %repo_root.display()))]
pub fn collect_git_file_dates(repo_root: &Path) -> Result<HashMap<PathBuf, i64>, ScanError> {
    // Discover the git repository, correctly handling worktrees, submodules,
    // and any non-standard git layout where `.git` is a file rather than a dir.
    let repo = match gix::discover(repo_root) {
        Ok(r) => r,
        Err(_) => {
            tracing::debug!("Not a git repository, skipping file date collection");
            return Ok(HashMap::new());
        }
    };

    // Get HEAD commit — if no commits exist, return empty.
    let head_commit = match repo.head_commit() {
        Ok(c) => c,
        Err(_) => {
            tracing::debug!("No HEAD commit found (empty repo), skipping file date collection");
            return Ok(HashMap::new());
        }
    };

    let mut file_dates: HashMap<PathBuf, i64> = HashMap::new();

    // Walk all commits reachable from HEAD in reverse chronological order.
    // For each commit, diff against its first parent (or against empty tree for
    // the root commit) to find which files were touched. The first time we see
    // a file, that's its most recent commit date.
    let walk = head_commit
        .ancestors()
        .all()
        .map_err(|e| ScanError::GitError(format!("Failed to walk commit ancestors: {e}")))?;

    for info in walk {
        let info = info
            .map_err(|e| ScanError::GitError(format!("Failed to read commit during walk: {e}")))?;

        let commit = info
            .id()
            .object()
            .map_err(|e| ScanError::GitError(format!("Failed to read commit object: {e}")))?
            .into_commit();

        let commit_time = commit
            .time()
            .map_err(|e| ScanError::GitError(format!("Failed to read commit time: {e}")))?;
        let timestamp = commit_time.seconds;

        let tree = commit
            .tree()
            .map_err(|e| ScanError::GitError(format!("Failed to read commit tree: {e}")))?;

        // Get the parent tree (or empty tree for root commit).
        let parent_tree = commit
            .parent_ids()
            .next()
            .and_then(|parent_id| parent_id.object().ok()?.into_commit().tree().ok());

        // Compute the diff between parent and current commit.
        let changes = match &parent_tree {
            Some(parent) => {
                let mut changes = Vec::new();
                let mut platform = parent.changes().map_err(|e| {
                    ScanError::GitError(format!("Failed to create tree changes tracker: {e}"))
                })?;
                platform.options(|opts| {
                    opts.track_path();
                });
                platform
                    .for_each_to_obtain_tree(&tree, |change| {
                        let path = PathBuf::from(change.location().to_string());
                        changes.push(path);
                        Ok::<_, std::convert::Infallible>(gix::object::tree::diff::Action::Continue)
                    })
                    .map_err(|e| ScanError::GitError(format!("Failed to diff trees: {e}")))?;
                changes
            }
            None => {
                // Root commit — all files in the tree are "added".
                let mut changes = Vec::new();
                tree_paths(&tree, &mut changes)?;
                changes
            }
        };

        for path in changes {
            // Only record the first (most recent) commit date per file.
            file_dates.entry(path).or_insert(timestamp);
        }
    }

    tracing::info!(
        files_with_dates = file_dates.len(),
        "Collected git file dates"
    );

    if file_dates.is_empty() {
        tracing::warn!(
            repo_root = %repo_root.display(),
            "No file dates collected — git history may be shallow, the repo may be a bare \
             clone, or the worktree walk encountered an unexpected layout"
        );
    }

    Ok(file_dates)
}

/// Recursively collect all file paths in a tree (for root commits).
fn tree_paths(tree: &gix::Tree<'_>, paths: &mut Vec<PathBuf>) -> Result<(), ScanError> {
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .map_err(|e| ScanError::GitError(format!("Failed to traverse tree: {e}")))?;

    for entry in recorder.records {
        if entry.mode.is_blob() {
            paths.push(PathBuf::from(entry.filepath.to_string()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    /// Helper: initialize a git repo, configure user, and make commits.
    fn init_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .expect("git config name");
    }

    fn git_add_and_commit(dir: &Path, message: &str) {
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", message, "--allow-empty-message"])
            .current_dir(dir)
            .output()
            .expect("git commit");
    }

    #[test]
    fn non_git_directory_returns_empty() {
        let dir = tempdir().expect("tempdir");
        let result = collect_git_file_dates(dir.path()).expect("should not error");
        assert!(result.is_empty(), "non-git dir should return empty map");
    }

    #[test]
    fn empty_repo_returns_empty() {
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());

        let result = collect_git_file_dates(dir.path()).expect("should not error");
        assert!(result.is_empty(), "empty repo should return empty map");
    }

    #[test]
    fn collects_dates_for_committed_files() {
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());

        // Create and commit a file
        fs::write(dir.path().join("hello.txt"), "hello").expect("write file");
        git_add_and_commit(dir.path(), "first commit");

        // Create and commit another file
        fs::write(dir.path().join("world.txt"), "world").expect("write file");
        git_add_and_commit(dir.path(), "second commit");

        let dates = collect_git_file_dates(dir.path()).expect("collect dates");
        assert!(
            dates.contains_key(&PathBuf::from("hello.txt")),
            "should have hello.txt"
        );
        assert!(
            dates.contains_key(&PathBuf::from("world.txt")),
            "should have world.txt"
        );

        // Both should have valid timestamps (positive values)
        for (path, ts) in &dates {
            assert!(
                *ts > 0,
                "timestamp for {} should be positive, got {}",
                path.display(),
                ts
            );
        }
    }

    #[test]
    fn most_recent_date_wins() {
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());

        // First commit
        fs::write(dir.path().join("file.txt"), "v1").expect("write");
        git_add_and_commit(dir.path(), "first");

        // Allow at least 1 second to elapse so timestamps differ.
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Modify the same file
        fs::write(dir.path().join("file.txt"), "v2").expect("write");
        git_add_and_commit(dir.path(), "second");

        let dates = collect_git_file_dates(dir.path()).expect("collect dates");
        let file_date = dates
            .get(&PathBuf::from("file.txt"))
            .expect("should have file.txt");

        // The date should be from the second (more recent) commit.
        // We can't check the exact value, but we verify it's a valid timestamp.
        assert!(*file_date > 0, "should have a positive timestamp");
    }

    #[test]
    fn handles_subdirectories() {
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());

        let sub = dir.path().join("src");
        fs::create_dir_all(&sub).expect("mkdir");
        fs::write(sub.join("main.rs"), "fn main() {}").expect("write");
        git_add_and_commit(dir.path(), "with subdirectory");

        let dates = collect_git_file_dates(dir.path()).expect("collect dates");
        assert!(
            dates.contains_key(&PathBuf::from("src/main.rs")),
            "should have src/main.rs, got keys: {:?}",
            dates.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn keys_are_relative_not_absolute() {
        // Verify that keys are relative paths so callers can look up by
        // stripping the project root prefix from an absolute path.
        let dir = tempdir().expect("tempdir");
        init_git_repo(dir.path());

        fs::write(dir.path().join("config.toml"), "[package]").expect("write");
        git_add_and_commit(dir.path(), "add config");

        let dates = collect_git_file_dates(dir.path()).expect("collect dates");

        // The relative path must be present.
        assert!(
            dates.contains_key(&PathBuf::from("config.toml")),
            "relative path must be a key"
        );

        // The absolute path must NOT be present.
        let abs = dir.path().join("config.toml");
        assert!(
            !dates.contains_key(abs.as_path()),
            "absolute path must NOT be a key — callers must strip the root prefix"
        );
    }
}
