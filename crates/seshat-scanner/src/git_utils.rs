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

/// Outcome of comparing a branch's stored `last_scanned_commit` sentinel
/// against the current `git rev-parse HEAD` of `root` (US-010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessCheck {
    /// HEAD differs from the stored sentinel — an incremental sync should run.
    ///
    /// `old_commit` is the previously recorded HEAD; it is `None` when the
    /// branch has never been scanned (e.g. a pre-US-009 DB or a fresh branch
    /// row created without a recorded HEAD), and `Some(...)` otherwise. The
    /// hash form is suitable to feed back into [`get_head_commit`]'s gix-tree
    /// resolution as the old-side of a tree diff.
    Stale {
        old_commit: Option<String>,
        new_commit: String,
    },
    /// Sentinel matches HEAD — no sync is needed.
    UpToDate,
    /// Git is unavailable for `root` (no `.git`, empty repo, gix open failed).
    /// Per the PRD's git-optional fallback, freshness checks short-circuit
    /// and no sync is triggered.
    GitUnavailable,
}

/// Compare `branch_id`'s stored `last_scanned_commit` to the on-disk HEAD of
/// the git working tree at `root` and return a [`FreshnessCheck`] result.
///
/// Used by `seshat serve` (US-010) and `seshat review` (US-011) at startup
/// to decide whether to trigger an incremental sync before serving stale
/// data to the user.
///
/// Storage errors when reading the sentinel are logged at `warn!` and treated
/// as "no recorded sentinel" (the helper returns `Stale { old_commit: None, .. }`
/// when HEAD is reachable, or `GitUnavailable` when it is not). This matches
/// the contract of [`record_branch_scan_complete`] which also swallows
/// storage errors so freshness machinery never crashes a startup.
pub fn check_branch_freshness<R: BranchRepository>(
    branch_repo: &R,
    root: &Path,
    branch_id: &BranchId,
) -> FreshnessCheck {
    let new_commit = match get_head_commit(root) {
        Some(c) => c,
        None => return FreshnessCheck::GitUnavailable,
    };
    let old_commit = match branch_repo.get_last_scanned_commit(branch_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                branch = %branch_id.0,
                "failed to read last_scanned_commit; treating as never-scanned"
            );
            None
        }
    };
    match &old_commit {
        Some(prev) if *prev == new_commit => FreshnessCheck::UpToDate,
        _ => FreshnessCheck::Stale {
            old_commit,
            new_commit,
        },
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
        assert!(get_head_commit(dir.path()).is_none());
        assert!(get_head_commit(dir.path()).is_none());
    }

    #[test]
    fn returns_none_for_nonexistent_path() {
        assert!(get_head_commit(Path::new("/tmp/does-not-exist-seshat-test")).is_none());
        assert!(get_head_commit(Path::new("/tmp/does-not-exist-seshat-test")).is_none());
    }

    #[test]
    fn returns_none_for_empty_git_repo() {
        let dir = tempdir().expect("create temp dir");
        // Create .git dir but no commits
        fs::create_dir(dir.path().join(".git")).expect("create .git");
        assert!(get_head_commit(dir.path()).is_none());
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

    /// Initialise a git repo at `path`, make a single commit (returning its
    /// SHA), then make a second commit with a follow-up file (returning its
    /// SHA). Used by the freshness-check tests to produce a real two-commit
    /// history to compare against.
    fn init_git_repo_with_two_commits(path: &Path) -> (String, String) {
        let head1 = init_git_repo_with_commit(path);
        fs::write(path.join("CHANGES.md"), "# changes").expect("write CHANGES.md");
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .stdout(Stdio::null())
            .status()
            .expect("git add second");
        Command::new("git")
            .args(["commit", "-m", "follow-up commit"])
            .current_dir(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git commit second");
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .expect("git rev-parse HEAD second");
        let head2 = String::from_utf8(out.stdout)
            .expect("rev-parse output utf8 second")
            .trim()
            .to_owned();
        assert_ne!(head1, head2, "two commits must have distinct SHAs");
        (head1, head2)
    }

    #[test]
    fn check_branch_freshness_returns_up_to_date_when_sentinel_matches_head() {
        let dir = tempdir().expect("create temp dir");
        let head = init_git_repo_with_commit(dir.path());

        let db = Database::open(":memory:").expect("open DB");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = BranchId::from("main");
        branch_repo
            .set_last_scanned_commit(&branch, &head)
            .expect("set sentinel");

        let result = check_branch_freshness(&branch_repo, dir.path(), &branch);
        assert_eq!(result, FreshnessCheck::UpToDate);
    }

    #[test]
    fn check_branch_freshness_returns_stale_when_head_advances() {
        let dir = tempdir().expect("create temp dir");
        let (head1, head2) = init_git_repo_with_two_commits(dir.path());

        let db = Database::open(":memory:").expect("open DB");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = BranchId::from("main");
        // Pin the sentinel at the OLDER commit; HEAD now points at the newer one.
        branch_repo
            .set_last_scanned_commit(&branch, &head1)
            .expect("set sentinel at head1");

        let result = check_branch_freshness(&branch_repo, dir.path(), &branch);
        assert_eq!(
            result,
            FreshnessCheck::Stale {
                old_commit: Some(head1),
                new_commit: head2,
            },
            "sentinel at head1 with HEAD at head2 must be Stale"
        );
    }

    #[test]
    fn check_branch_freshness_returns_stale_with_none_old_commit_when_never_scanned() {
        let dir = tempdir().expect("create temp dir");
        let head = init_git_repo_with_commit(dir.path());

        let db = Database::open(":memory:").expect("open DB");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = BranchId::from("main");
        // Branch row is NOT registered with a sentinel — emulates a pre-US-009
        // DB or a fresh branch that has never had its HEAD recorded.

        let result = check_branch_freshness(&branch_repo, dir.path(), &branch);
        assert_eq!(
            result,
            FreshnessCheck::Stale {
                old_commit: None,
                new_commit: head,
            },
            "no recorded sentinel + reachable HEAD must be Stale with old_commit=None"
        );
    }

    #[test]
    fn check_branch_freshness_returns_git_unavailable_for_non_git_directory() {
        let dir = tempdir().expect("create temp dir");
        // No `.git` here.

        let db = Database::open(":memory:").expect("open DB");
        let branch_repo = SqliteBranchRepository::new(db.connection().clone());
        let branch = BranchId::from("main");
        // Even with a recorded sentinel, git-unavailable wins.
        branch_repo
            .set_last_scanned_commit(&branch, "deadbeefcafebabedeadbeefcafebabedeadbeef")
            .expect("set sentinel");

        let result = check_branch_freshness(&branch_repo, dir.path(), &branch);
        assert_eq!(result, FreshnessCheck::GitUnavailable);
    }
}
