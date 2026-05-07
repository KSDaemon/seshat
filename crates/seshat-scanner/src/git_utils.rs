//! Git utility functions for submodule operations and project freshness.
//!
//! Provides helpers for extracting git metadata (HEAD commit hash) from any
//! git working tree — submodule, project root, worktree — and recording the
//! commit SHA reached at scan-completion time so subsequent startups can
//! detect divergence (US-009).

use std::path::Path;

use seshat_core::BranchId;
use seshat_storage::BranchRepository;

/// Get the current commit hash (HEAD) of the git repository at `path`.
///
/// Opens `path` as a git repository (works with normal repos, worktrees, and
/// submodules — anywhere a `.git` file or directory is reachable) and reads
/// the HEAD commit object ID.
///
/// Returns `None` if:
/// - The path is not a git repository
/// - The repository has no commits (freshly init'd)
/// - Any git operation fails
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use seshat_scanner::get_head_commit;
///
/// if let Some(head) = get_head_commit(Path::new(".")) {
///     println!("HEAD: {head}");
/// }
/// ```
pub fn get_head_commit(path: &Path) -> Option<String> {
    let repo = gix::open(path).ok()?;
    let head = repo.head_commit().ok()?;
    Some(head.id().to_string())
}

/// Alias of [`get_head_commit`] retained for callers that semantically want
/// a submodule's HEAD (the implementation is identical — any git working
/// tree works).
pub fn get_submodule_commit_hash(submodule_path: &Path) -> Option<String> {
    get_head_commit(submodule_path)
}

/// Record `branch_id`'s `last_scanned_commit` to the current `git rev-parse
/// HEAD` of `root`, after a successful scan/sync.
///
/// Behaviour:
/// - When `root` resolves to a real git repo, calls
///   [`BranchRepository::set_last_scanned_commit`] with the HEAD commit hash.
/// - When git is unavailable (no `.git`, empty repo, gix open failure), this
///   is a silent no-op with a `debug!` trace — the `branches.last_scanned_commit`
///   column simply stays `NULL` for that branch (US-009 git-unavailable case).
/// - Storage errors during the write are logged at `warn!` and swallowed:
///   the scan/sync that just succeeded should not regress because the
///   freshness sentinel could not be persisted.
pub fn record_branch_scan_complete<R: BranchRepository>(
    branch_repo: &R,
    root: &Path,
    branch_id: &BranchId,
) {
    match get_head_commit(root) {
        Some(head) => {
            if let Err(e) = branch_repo.set_last_scanned_commit(branch_id, &head) {
                tracing::warn!(
                    error = %e,
                    branch = %branch_id.0,
                    "failed to record last_scanned_commit; freshness check may be stale"
                );
            } else {
                tracing::debug!(
                    branch = %branch_id.0,
                    head = %head,
                    "recorded last_scanned_commit"
                );
            }
        }
        None => {
            tracing::debug!(
                root = %root.display(),
                branch = %branch_id.0,
                "git unavailable; skipping last_scanned_commit update"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::{Command, Stdio};
    use tempfile::tempdir;

    use seshat_storage::{BranchRepository, Database, SqliteBranchRepository};

    /// Initialise a git repo at `path` with one commit and return the HEAD SHA.
    fn init_git_repo_with_commit(path: &Path) -> String {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@seshat.dev"])
            .current_dir(path)
            .stdout(Stdio::null())
            .status()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Seshat Test"])
            .current_dir(path)
            .stdout(Stdio::null())
            .status()
            .expect("git config name");
        fs::write(path.join("README.md"), "# fixture").expect("write readme");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .stdout(Stdio::null())
            .status()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git commit");

        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .expect("git rev-parse HEAD");
        String::from_utf8(out.stdout)
            .expect("rev-parse output utf8")
            .trim()
            .to_owned()
    }

    #[test]
    fn returns_none_for_non_git_directory() {
        let dir = tempdir().expect("create temp dir");
        assert!(get_submodule_commit_hash(dir.path()).is_none());
        assert!(get_head_commit(dir.path()).is_none());
    }

    #[test]
    fn returns_none_for_nonexistent_path() {
        assert!(get_submodule_commit_hash(Path::new("/tmp/does-not-exist-seshat-test")).is_none());
        assert!(get_head_commit(Path::new("/tmp/does-not-exist-seshat-test")).is_none());
    }

    #[test]
    fn returns_none_for_empty_git_repo() {
        let dir = tempdir().expect("create temp dir");
        // Create .git dir but no commits
        fs::create_dir(dir.path().join(".git")).expect("create .git");
        assert!(get_submodule_commit_hash(dir.path()).is_none());
        assert!(get_head_commit(dir.path()).is_none());
    }

    #[test]
    fn get_head_commit_returns_hash_for_real_git_repo() {
        let dir = tempdir().expect("create temp dir");
        let expected = init_git_repo_with_commit(dir.path());
        let hash = get_head_commit(dir.path()).expect("HEAD commit hash");
        assert_eq!(hash, expected, "gix HEAD should match git rev-parse HEAD");
        assert_eq!(hash.len(), 40, "SHA-1 hash should be 40 hex chars");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "hash should be hex: {hash}"
        );
    }

    #[test]
    fn record_branch_scan_complete_writes_head_to_branches_table() {
        let dir = tempdir().expect("create temp dir");
        let expected_head = init_git_repo_with_commit(dir.path());

        let db = Database::open(":memory:").expect("open DB");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = BranchId::from("main");
        branch_repo
            .ensure_branch_exists(&branch)
            .expect("ensure branch exists");

        record_branch_scan_complete(&branch_repo, dir.path(), &branch);

        let stored = branch_repo
            .get_last_scanned_commit(&branch)
            .expect("get last_scanned_commit");
        assert_eq!(
            stored,
            Some(expected_head),
            "branches.last_scanned_commit must match git rev-parse HEAD"
        );
    }

    #[test]
    fn record_branch_scan_complete_is_silent_noop_when_git_unavailable() {
        let dir = tempdir().expect("create temp dir");
        // No .git here — record_branch_scan_complete must NOT write a sentinel.

        let db = Database::open(":memory:").expect("open DB");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = BranchId::from("main");
        branch_repo
            .ensure_branch_exists(&branch)
            .expect("ensure branch exists");

        record_branch_scan_complete(&branch_repo, dir.path(), &branch);

        let stored = branch_repo
            .get_last_scanned_commit(&branch)
            .expect("get last_scanned_commit");
        assert_eq!(
            stored, None,
            "branches.last_scanned_commit must stay NULL when git is unavailable"
        );
    }
}
