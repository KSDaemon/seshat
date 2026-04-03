//! Git utility functions for submodule operations.
//!
//! Provides helpers for extracting git metadata from submodules,
//! such as the current commit hash.

use std::path::Path;

/// Get the current commit hash (HEAD) of a git submodule at `submodule_path`.
///
/// Opens the submodule directory as a git repository (works with both
/// `.git` files pointing to `../../.git/modules/...` and standalone
/// `.git` directories) and reads the HEAD commit object ID.
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
/// use seshat_scanner::get_submodule_commit_hash;
///
/// let hash = get_submodule_commit_hash(Path::new("vendor/some-lib"));
/// if let Some(h) = hash {
///     println!("Submodule at HEAD: {h}");
/// }
/// ```
pub fn get_submodule_commit_hash(submodule_path: &Path) -> Option<String> {
    let repo = gix::open(submodule_path).ok()?;
    let head = repo.head_commit().ok()?;
    Some(head.id().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn returns_none_for_non_git_directory() {
        let dir = tempdir().expect("create temp dir");
        assert!(get_submodule_commit_hash(dir.path()).is_none());
    }

    #[test]
    fn returns_none_for_nonexistent_path() {
        assert!(get_submodule_commit_hash(Path::new("/tmp/does-not-exist-seshat-test")).is_none());
    }

    #[test]
    fn returns_none_for_empty_git_repo() {
        let dir = tempdir().expect("create temp dir");
        // Create .git dir but no commits
        fs::create_dir(dir.path().join(".git")).expect("create .git");
        assert!(get_submodule_commit_hash(dir.path()).is_none());
    }

    #[test]
    fn returns_hash_for_real_git_repo() {
        // Use the current seshat repo itself as a known-good git repo
        let repo_root = std::env::current_dir().expect("current dir");
        // Walk up to find the repo root (contains .git)
        let mut root = repo_root.as_path();
        loop {
            if root.join(".git").exists() {
                break;
            }
            root = root.parent().expect("should find repo root");
        }
        let hash = get_submodule_commit_hash(root);
        assert!(hash.is_some(), "should get hash from real repo");
        let hash_str = hash.unwrap();
        // SHA-1 hex is 40 chars
        assert_eq!(hash_str.len(), 40, "SHA-1 hash should be 40 hex chars");
        assert!(
            hash_str.chars().all(|c| c.is_ascii_hexdigit()),
            "hash should be hex: {hash_str}"
        );
    }
}
