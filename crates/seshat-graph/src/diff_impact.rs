//! Diff-to-impact mapping: identify changed files and their status.
//!
//! Provides `get_changed_files()` which uses `gix` to diff the working tree
//! against HEAD (or staged/index, or a base commit) and returns structured
//! change information including file status and conflict detection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha1::Digest;

use crate::error::GraphError;

// ── File status enum ───────────────────────────────────────────

/// Status of a changed file relative to the comparison baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// File was modified but not yet staged.
    Modified,
    /// File was newly added.
    Added,
    /// File was deleted.
    Deleted,
    /// File is not tracked by git (not yet added to index).
    Untracked,
    /// File has merge conflict markers.
    Conflicted,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Modified => write!(f, "modified"),
            Self::Added => write!(f, "added"),
            Self::Deleted => write!(f, "deleted"),
            Self::Untracked => write!(f, "untracked"),
            Self::Conflicted => write!(f, "conflicted"),
        }
    }
}

// ── Data types ─────────────────────────────────────────────────

/// A single changed file with its status.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    /// Relative path from the repository root.
    pub path: String,
    /// Change status (modified, added, deleted, untracked, conflicted).
    pub status: FileStatus,
}

/// Request parameters for `get_changed_files()`.
#[derive(Debug, Clone, Serialize)]
pub struct DiffImpactRequest {
    /// If `true`, compare staged changes (index vs HEAD) instead of
    /// working tree vs HEAD. Mutually exclusive with `base`.
    pub staged_only: bool,
    /// Optional base commitish to diff against instead of HEAD.
    /// Mutually exclusive with `staged_only`.
    pub base: Option<String>,
    /// Repository path on disk.
    pub repo_path: String,
}

/// Placeholder for affected symbol — will be populated in US-002.
#[derive(Debug, Clone, Serialize)]
pub struct AffectedSymbol {
    /// Symbol name (function, type, or export).
    pub name: String,
    /// File path where the symbol is defined.
    pub file: String,
    /// Kind of symbol: "function", "type", or "export".
    pub kind: String,
    /// Number of files that depend on this symbol.
    pub dependent_count: usize,
    /// Up to 5 dependent file references.
    pub dependents: Vec<DependentRef>,
    /// Blast radius classification: "low", "medium", or "high".
    pub blast_radius: String,
}

/// A reference to a file that depends on an affected symbol.
#[derive(Debug, Clone, Serialize)]
pub struct DependentRef {
    /// File path of the dependent.
    pub file: String,
    /// Line number of the import/usage.
    pub line: usize,
}

/// Convention risk — will be populated in US-003.
#[derive(Debug, Clone, Serialize)]
pub struct ConventionRisk {
    /// Convention description.
    pub description: String,
    /// Affected file that contributes evidence.
    pub affected_file: String,
    /// Confidence percentage (0–100).
    pub confidence_pct: f64,
    /// Adoption count (files following this convention).
    pub adoption_count: usize,
    /// Total number of files examined.
    pub total_count: usize,
    /// Whether the affected file is a golden file for this convention.
    pub is_golden_file: bool,
    /// Human-readable note about the risk.
    pub note: String,
}

/// Summary of convention adoption statistics across the project.
#[derive(Debug, Clone, Serialize)]
pub struct AdoptionSummary {
    /// Number of files that follow the convention.
    pub adoption_count: usize,
    /// Total number of files examined.
    pub total_count: usize,
    /// Confidence percentage (0–100).
    pub confidence_pct: f64,
    /// Whether this file is a golden file (highest convention compliance).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_golden_file: Option<bool>,
}

/// Aggregated blast radius summary.
#[derive(Debug, Clone, Serialize)]
pub struct BlastRadiusSummary {
    /// Total number of files with dependents across all affected symbols.
    pub total_dependents: usize,
    /// Total number of affected symbols.
    pub total_affected_symbols: usize,
    /// Total number of changed files.
    pub total_changed_files: usize,
    /// Overall risk level: "none", "low", "medium", or "high".
    pub risk: String,
}

/// Metadata about the diff impact analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactMetadata {
    /// Suggested next steps based on the analysis.
    pub next_steps: Vec<String>,
    /// Current git branch name (or commit hash if detached HEAD).
    pub branch: String,
}

/// Full diff-impact response data.
#[derive(Debug, Clone, Serialize)]
pub struct DiffImpactData {
    /// Files that have changed (uncommitted or relative to base).
    pub changed_files: Vec<ChangedFile>,
    /// Symbols affected by the changes (populated in US-002).
    pub affected_symbols: Vec<AffectedSymbol>,
    /// Convention risks (populated in US-003).
    pub convention_risks: Vec<ConventionRisk>,
    /// Blast radius summary.
    pub blast_radius_summary: BlastRadiusSummary,
    /// Metadata.
    pub metadata: ImpactMetadata,
}

// ── Public API ─────────────────────────────────────────────────

/// Discover which files are changed in the working tree (or index) relative
/// to HEAD (or a specified base commit).
///
/// - `repo_path` — filesystem path to the git repository root.
/// - `staged_only` — if `true`, diff the index vs HEAD (shows only staged
///   changes). Mutually exclusive with `base`.
/// - `base` — optional commitish to diff against (e.g. `"main"`, `"abc123"`).
///   When set, all worktree/index changes are shown relative to this base.
///   Mutually exclusive with `staged_only`.
///
/// Returns a list of `ChangedFile` entries sorted by path. Files that are
/// both modified *and* contain conflict markers get status `Conflicted`.
#[tracing::instrument(skip_all, fields(repo_path = %repo_path.display()))]
pub fn get_changed_files(
    repo_path: &Path,
    staged_only: bool,
    base: Option<&str>,
) -> Result<Vec<ChangedFile>, GraphError> {
    if staged_only && base.is_some() {
        return Err(GraphError::InvalidInput(
            "staged_only and base are mutually exclusive".to_owned(),
        ));
    }

    let repo = gix::open(repo_path).map_err(|e| {
        GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
            "Not a git repository: {} — {e}",
            repo_path.display()
        )))
    })?;

    let head_tree = resolve_head_tree(&repo)?;

    let index = repo
        .open_index()
        .map_err(|e| GraphError::query(format!("Failed to read git index: {e}")))?;

    // 1. Staged changes: diff index vs HEAD tree by comparing ObjectIds.
    let head_entry_ids = collect_tree_entry_ids(&head_tree)?;
    let mut staged_paths: HashMap<String, FileStatus> = HashMap::new();

    for entry in index.entries() {
        let rel_path = entry.path(&index).to_string();

        match head_entry_ids.get(&rel_path) {
            None => {
                staged_paths.insert(rel_path, FileStatus::Added);
            }
            Some(head_id) => {
                if entry.id != *head_id {
                    staged_paths.insert(rel_path, FileStatus::Modified);
                }
            }
        }
    }

    // Files in HEAD but absent from index = deleted (via staged removal).
    let index_path_set: HashSet<String> = index
        .entries()
        .iter()
        .map(|e| e.path(&index).to_string())
        .collect();
    for path in head_entry_ids.keys() {
        if !index_path_set.contains(path) {
            staged_paths.insert(path.clone(), FileStatus::Deleted);
        }
    }

    let mut changed_files = Vec::new();

    // 2. Unstaged changes: for files in the index, check if disk content
    //    differs from the index entry's ObjectId.
    let mut unstaged_paths: HashMap<String, FileStatus> = HashMap::new();

    if !staged_only {
        for entry in index.entries() {
            let rel_path = entry.path(&index).to_string();
            let full_path = repo_path.join(&rel_path);

            if !full_path.exists() {
                unstaged_paths.insert(rel_path, FileStatus::Deleted);
                continue;
            }

            if let Some(disk_oid) = hash_file_on_disk(&full_path) {
                if disk_oid != entry.id {
                    unstaged_paths
                        .entry(rel_path)
                        .or_insert(FileStatus::Modified);
                }
            }
        }

        // 3. Untracked files: in worktree but not in index or HEAD tree.
        let known_paths = collect_tree_paths(&head_tree)?;
        let staged_keys: HashSet<String> = staged_paths.keys().cloned().collect();
        let unstaged_keys: HashSet<String> = unstaged_paths.keys().cloned().collect();
        if let Ok(worktree_files) = collect_worktree_paths(repo_path) {
            for path in worktree_files {
                let path_str = path.to_string_lossy().to_string();
                if !known_paths.contains(&path_str)
                    && !staged_keys.contains(&path_str)
                    && !unstaged_keys.contains(&path_str)
                {
                    changed_files.push(ChangedFile {
                        path: path_str,
                        status: FileStatus::Untracked,
                    });
                }
            }
        }
    }

    // When `base` is specified, recompute staged_paths relative to the base commit.
    if let Some(base_ref) = base {
        let base_tree = resolve_base_tree(&repo, base_ref)?;
        let base_entry_ids = collect_tree_entry_ids(&base_tree)?;

        staged_paths.clear();

        for entry in index.entries() {
            let rel_path = entry.path(&index).to_string();

            match base_entry_ids.get(&rel_path) {
                None => {
                    staged_paths.insert(rel_path, FileStatus::Added);
                }
                Some(base_id) => {
                    if entry.id != *base_id {
                        staged_paths.insert(rel_path, FileStatus::Modified);
                    }
                }
            }
        }

        let index_path_set: HashSet<String> = index
            .entries()
            .iter()
            .map(|e| e.path(&index).to_string())
            .collect();
        for path in base_entry_ids.keys() {
            if !index_path_set.contains(path) {
                staged_paths.insert(path.clone(), FileStatus::Deleted);
            }
        }

        let base_known = collect_tree_paths(&base_tree)?;
        let staged_keys: HashSet<String> = staged_paths.keys().cloned().collect();
        if let Ok(worktree_files) = collect_worktree_paths(repo_path) {
            for path in worktree_files {
                let path_str = path.to_string_lossy().to_string();
                if !base_known.contains(&path_str) && !staged_keys.contains(&path_str) {
                    changed_files.push(ChangedFile {
                        path: path_str,
                        status: FileStatus::Untracked,
                    });
                }
            }
        }
    } else if staged_only {
        for (path, status) in staged_paths {
            changed_files.push(ChangedFile { path, status });
        }
        mark_conflicts(repo_path, &mut changed_files);
        changed_files.sort_by(|a, b| a.path.cmp(&b.path));
        return Ok(changed_files);
    }

    // Merge staged and unstaged into a single de-duplicated list.
    for (path, status) in staged_paths {
        changed_files.push(ChangedFile { path, status });
    }
    for (path, status) in unstaged_paths {
        if !changed_files.iter().any(|c| c.path == path) {
            changed_files.push(ChangedFile { path, status });
        }
    }

    mark_conflicts(repo_path, &mut changed_files);
    changed_files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changed_files)
}

// ── Internal helpers ────────────────────────────────────────────

/// Mark files containing conflict markers as Conflicted.
fn mark_conflicts(repo_path: &Path, changed_files: &mut [ChangedFile]) {
    for changed_file in changed_files.iter_mut() {
        if changed_file.status != FileStatus::Deleted
            && changed_file.status != FileStatus::Untracked
            && has_conflict_markers(repo_path, &changed_file.path)
        {
            changed_file.status = FileStatus::Conflicted;
        }
    }
}

/// Resolve HEAD to a tree. Falls back to empty tree for unborn HEAD.
fn resolve_head_tree(repo: &gix::Repository) -> Result<gix::Tree<'_>, GraphError> {
    match repo.head_tree() {
        Ok(tree) => Ok(tree),
        Err(_) => Ok(repo.empty_tree()),
    }
}

/// Resolve a base reference (branch name or commit hash) to a Tree.
fn resolve_base_tree<'repo>(
    repo: &'repo gix::Repository,
    base_ref: &str,
) -> Result<gix::Tree<'repo>, GraphError> {
    let ref_name = format!("refs/heads/{base_ref}");
    let oid = match repo.try_find_reference(&ref_name) {
        Ok(Some(reference)) => reference
            .into_fully_peeled_id()
            .map_err(|e| GraphError::query(format!("Failed to peel reference '{base_ref}': {e}")))?
            .detach(),
        Ok(None) => {
            if let Ok(oid) = gix::ObjectId::from_hex(base_ref.as_bytes()) {
                oid
            } else {
                return Err(GraphError::query(format!(
                    "Cannot resolve base reference '{base_ref}'"
                )));
            }
        }
        Err(e) => {
            return Err(GraphError::query(format!(
                "Failed to look up reference '{base_ref}': {e}"
            )));
        }
    };

    let tree_id = repo
        .find_object(oid)
        .map_err(|e| GraphError::query(format!("Failed to find base object '{base_ref}': {e}")))?
        .try_into_commit()
        .map_err(|_| GraphError::query(format!("'{base_ref}' is not a valid commit")))?
        .tree_id()
        .map_err(|e| GraphError::query(format!("Failed to get base tree: {e}")))?;

    repo.find_tree(tree_id)
        .map_err(|e| GraphError::query(format!("Failed to find base tree: {e}")))
}

/// Collect (relative_path, ObjectId) pairs for all blob entries in a tree.
fn collect_tree_entry_ids(
    tree: &gix::Tree<'_>,
) -> Result<HashMap<String, gix::ObjectId>, GraphError> {
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .map_err(|e| GraphError::query(format!("Failed to traverse tree: {e}")))?;

    let mut entries = HashMap::new();
    for entry in recorder.records {
        if entry.mode.is_blob() {
            entries.insert(entry.filepath.to_string(), entry.oid);
        }
    }
    Ok(entries)
}

/// Collect all blob file paths from a gix Tree.
fn collect_tree_paths(tree: &gix::Tree<'_>) -> Result<HashSet<String>, GraphError> {
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse()
        .breadthfirst(&mut recorder)
        .map_err(|e| GraphError::query(format!("Failed to traverse tree: {e}")))?;

    let mut paths = HashSet::new();
    for entry in recorder.records {
        if entry.mode.is_blob() {
            paths.insert(entry.filepath.to_string());
        }
    }
    Ok(paths)
}

/// Collect worktree file paths (walk the filesystem, skip .git).
fn collect_worktree_paths(repo_path: &Path) -> Result<Vec<PathBuf>, GraphError> {
    let mut paths = Vec::new();
    walk_dir(repo_path, repo_path, &mut paths)?;
    Ok(paths)
}

fn walk_dir(root: &Path, current: &Path, paths: &mut Vec<PathBuf>) -> Result<(), GraphError> {
    let entries = std::fs::read_dir(current).map_err(|e| {
        GraphError::query(format!(
            "Failed to read directory {}: {e}",
            current.display()
        ))
    })?;

    for entry in entries {
        let entry =
            entry.map_err(|e| GraphError::query(format!("Failed to read directory entry: {e}")))?;

        let path = entry.path();

        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }

        if path.is_dir() {
            walk_dir(root, &path, paths)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                paths.push(rel.to_path_buf());
            }
        }
    }

    Ok(())
}

/// Hash a file on disk using SHA-1 (blob header + content) and return its gix ObjectId.
fn hash_file_on_disk(path: &Path) -> Option<gix::ObjectId> {
    let bytes = std::fs::read(path).ok()?;

    let mut hasher = sha1::Sha1::new();
    let header = format!("blob {}\0", bytes.len());
    hasher.update(header.as_bytes());
    hasher.update(&bytes);

    let hash_bytes: [u8; 20] = hasher.finalize().into();
    Some(gix::ObjectId::Sha1(hash_bytes))
}

/// Convert FileStatus to a short slug for display.
#[allow(dead_code)]
fn status_slug(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Modified => "M",
        FileStatus::Added => "A",
        FileStatus::Deleted => "D",
        FileStatus::Untracked => "U",
        FileStatus::Conflicted => "C",
    }
}

/// Check if a file contains merge conflict markers.
fn has_conflict_markers(repo_path: &Path, relative_path: &str) -> bool {
    let full_path = repo_path.join(relative_path);
    match std::fs::read_to_string(&full_path) {
        Ok(content) => content.lines().any(|line| line.starts_with("<<<<<<<")),
        Err(_) => false,
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn init_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
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

    fn git_commit_all(dir: &Path, msg: &str) {
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .expect("git add");

        Command::new("git")
            .args(["commit", "-m", msg])
            .current_dir(dir)
            .output()
            .expect("git commit");
    }

    #[test]
    fn no_changes_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("hello.txt"), "hello").expect("write file");
        git_commit_all(&repo, "initial");

        let changes = get_changed_files(&repo, false, None).expect("get_changed_files");
        assert!(changes.is_empty(), "Expected no changes, got: {changes:?}");
    }

    #[test]
    fn modified_file_detected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("hello.txt"), "hello").expect("write file");
        git_commit_all(&repo, "initial");

        // Modify the file (unstaged).
        fs::write(repo.join("hello.txt"), "hello world").expect("modify file");

        let changes = get_changed_files(&repo, false, None).expect("get_changed_files");
        assert!(
            changes
                .iter()
                .any(|c| c.path == "hello.txt" && c.status == FileStatus::Modified),
            "Expected hello.txt as modified, got: {changes:?}"
        );
    }

    #[test]
    fn deleted_file_detected_staged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("deleteme.txt"), "delete me").expect("write file");
        git_commit_all(&repo, "initial");

        // Delete and stage.
        fs::remove_file(repo.join("deleteme.txt")).expect("delete file");
        Command::new("git")
            .args(["add", "deleteme.txt"])
            .current_dir(&repo)
            .output()
            .expect("git add deletion");

        let changes = get_changed_files(&repo, false, None).expect("get_changed_files");
        assert!(
            changes
                .iter()
                .any(|c| c.path == "deleteme.txt" && c.status == FileStatus::Deleted),
            "Expected deleteme.txt as deleted, got: {changes:?}"
        );
    }

    #[test]
    fn staged_only_and_base_are_mutually_exclusive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("a.txt"), "a").expect("write");
        git_commit_all(&repo, "initial");

        let result = get_changed_files(&repo, true, Some("main"));
        assert!(result.is_err());
        match result.unwrap_err() {
            GraphError::InvalidInput(msg) => {
                assert!(
                    msg.contains("mutually exclusive"),
                    "Expected mutually exclusive error, got: {msg}"
                );
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn not_a_git_repo_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake_path = dir.path().join("not-a-repo");
        fs::create_dir_all(&fake_path).expect("create dir");

        let result = get_changed_files(&fake_path, false, None);
        assert!(result.is_err(), "Expected error for non-git directory");
    }

    #[test]
    fn conflict_markers_detected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("conflict.txt"), "hello").expect("write file");
        git_commit_all(&repo, "initial");

        // Write conflict markers and stage the file.
        let conflict_content = "<<<<<<< HEAD\nour change\n=======\ntheir change\n>>>>>>> branch\n";
        fs::write(repo.join("conflict.txt"), conflict_content).expect("write conflict markers");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");

        let changes = get_changed_files(&repo, true, None).expect("get_changed_files");
        let conflict_file = changes.iter().find(|c| c.path == "conflict.txt");
        assert!(
            conflict_file.is_some_and(|c| c.status == FileStatus::Conflicted),
            "Expected conflict.txt to be conflicted, got: {changes:?}"
        );
    }

    #[test]
    fn added_file_staged_detected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("existing.txt"), "exists").expect("write file");
        git_commit_all(&repo, "initial");

        // Add a new file and stage it.
        fs::write(repo.join("new_file.txt"), "new").expect("write new file");
        Command::new("git")
            .args(["add", "new_file.txt"])
            .current_dir(&repo)
            .output()
            .expect("git add");

        let changes = get_changed_files(&repo, true, None).expect("get_changed_files");
        assert!(
            changes
                .iter()
                .any(|c| c.path == "new_file.txt" && c.status == FileStatus::Added),
            "Expected new_file.txt as added, got: {changes:?}"
        );
    }
}
