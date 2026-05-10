//! Diff-to-impact mapping: identify changed files and their status.
//!
//! Provides `get_changed_files()` which uses `gix` to diff the working tree
//! against HEAD (or staged/index, or a base commit) and returns structured
//! change information including file status and conflict detection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use sha1::Digest;

use crate::code_pattern::load_branch_ir;
use crate::dependencies::{self, BlastRadius, query_dependencies_batch};
use crate::error::GraphError;
use crate::golden_files::{DEFAULT_GOLDEN_FILES_LIMIT, get_golden_files};
use seshat_core::BranchId;

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

/// A single changed file annotated with the ObjectIds needed to read both
/// sides of the diff for hunk-level analysis (US-008/US-009).
///
/// `base_blob_id` and `index_blob_id` semantics:
/// - **Modified (staged or unstaged)**: both are `Some`; `base_blob_id` is
///   the HEAD blob (or base-commit blob when the `base` parameter is set),
///   `index_blob_id` is the current index entry's ObjectId.
/// - **Added**: `base_blob_id == None`, `index_blob_id == Some(...)`.
/// - **Deleted**: `base_blob_id == Some(...)`, `index_blob_id == None`.
/// - **Untracked**: both are `None` — the file's contents only exist on
///   disk; callers reading content must fall back to the working tree.
/// - **Conflicted**: same shape as Modified — the conflict status is
///   detected after blob enumeration, blob IDs are preserved.
#[derive(Debug, Clone)]
pub struct ChangedFileWithBlobs {
    /// Relative path from the repository root.
    pub path: String,
    /// Change status (modified, added, deleted, untracked, conflicted).
    pub status: FileStatus,
    /// ObjectId of the blob on the *old* side of the diff. `None` when
    /// the file did not exist on the old side (Added) or there is no
    /// tracked old version (Untracked).
    pub base_blob_id: Option<gix::ObjectId>,
    /// ObjectId of the blob recorded in the git index. `None` when the
    /// file is removed from the index (Deleted) or only exists on disk
    /// (Untracked).
    pub index_blob_id: Option<gix::ObjectId>,
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

// ── Hunk extraction primitives ────────────────────────────────

/// A 1-based half-open range of line numbers `[start, end)`.
///
/// Examples:
/// - `LineRange { start: 5, end: 8 }` covers lines 5, 6, 7 (three lines).
/// - `LineRange { start: 5, end: 5 }` is empty (zero lines), used to
///   represent the position of a pure deletion or pure insertion site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LineRange {
    /// First line included in the range (1-based, inclusive).
    pub start: usize,
    /// First line *not* included in the range (1-based, exclusive).
    pub end: usize,
}

impl LineRange {
    /// Return `true` if the range contains no lines (`start == end`).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// A single hunk: a contiguous region of `old` lines that was replaced by a
/// contiguous region of `new` lines.
///
/// - `old.is_empty()` ⇒ pure insertion (no old lines deleted).
/// - `new.is_empty()` ⇒ pure deletion (no new lines inserted).
/// - Otherwise the hunk is a replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Hunk {
    /// Range of removed lines in the old blob.
    pub old: LineRange,
    /// Range of inserted lines in the new blob.
    pub new: LineRange,
}

impl Hunk {
    /// Sentinel hunk covering an entire file. Used as a conservative fallback
    /// when blobs cannot be diffed (binary, oversized, missing). Any line
    /// queried via [`Hunk::touches_new_line`] returns `true` against this
    /// hunk.
    pub const ALL: Hunk = Hunk {
        old: LineRange {
            start: 1,
            end: usize::MAX,
        },
        new: LineRange {
            start: 1,
            end: usize::MAX,
        },
    };

    /// `true` if no old lines were removed (pure insertion).
    pub fn is_pure_insertion(&self) -> bool {
        self.old.is_empty()
    }

    /// `true` if no new lines were inserted (pure deletion).
    pub fn is_pure_deletion(&self) -> bool {
        self.new.is_empty()
    }

    /// `true` if `line` (1-based) is touched by this hunk in the new file.
    ///
    /// For non-empty new ranges this is plain containment. For pure
    /// deletions the new range is empty `[s, s)` and the deletion sits
    /// between new lines `s - 1` and `s`; both adjacent lines are
    /// reported as touched so a symbol whose body borders the gap still
    /// gets flagged.
    pub fn touches_new_line(&self, line: usize) -> bool {
        if self.new.is_empty() {
            line == self.new.start || (self.new.start > 1 && line == self.new.start - 1)
        } else {
            self.new.start <= line && line < self.new.end
        }
    }
}

/// Diff two blob contents and return the list of hunks computed via the
/// Histogram algorithm (the algorithm git uses by default for `diff`).
///
/// Each emitted [`Hunk`] reports the corresponding 1-based half-open
/// line ranges in the old and new blobs. Returns an empty `Vec` when
/// the two byte slices are identical.
pub fn diff_blobs_to_hunks(old: &[u8], new: &[u8]) -> Vec<Hunk> {
    use gix::diff::blob::{Algorithm, diff, intern::InternedInput};

    let input = InternedInput::new(old, new);
    let mut hunks: Vec<Hunk> = Vec::new();
    diff(
        Algorithm::Histogram,
        &input,
        |before: std::ops::Range<u32>, after: std::ops::Range<u32>| {
            hunks.push(Hunk {
                old: LineRange {
                    start: before.start as usize + 1,
                    end: before.end as usize + 1,
                },
                new: LineRange {
                    start: after.start as usize + 1,
                    end: after.end as usize + 1,
                },
            });
        },
    );
    hunks
}

/// Return the subset of `hunks` whose new-side ranges overlap the symbol's
/// inclusive `[line, end_line]` body.
///
/// Both endpoints are 1-based and inclusive; `end_line` may equal `line` for
/// single-line symbols (e.g. `pub use foo;`). Pure-deletion hunks (empty new
/// range) are reported as overlapping when they sit immediately before or
/// after the symbol — matching [`Hunk::touches_new_line`]'s deletion
/// semantics. The returned vector preserves the order of `hunks`.
///
/// Used by `compute_affected_symbols` to decide which symbols are touched by
/// a diff, and to build [`AffectedSymbol::changed_lines`] by clamping each
/// matching hunk's range to the symbol's body.
pub fn symbol_intersects_hunks(line: usize, end_line: usize, hunks: &[Hunk]) -> Vec<Hunk> {
    if line == 0 || end_line < line {
        return Vec::new();
    }
    hunks
        .iter()
        .copied()
        .filter(|h| {
            if h.new.is_empty() {
                // Pure deletion at [s, s): touches new lines `s - 1` and `s`.
                let s = h.new.start;
                (s >= line && s <= end_line)
                    || (s > 0 && s.saturating_sub(1) >= line && s.saturating_sub(1) <= end_line)
            } else {
                // Half-open intersection of [line..=end_line] with [h.new.start..h.new.end).
                line < h.new.end && end_line >= h.new.start
            }
        })
        .collect()
}

/// Clamp a hunk's new-side range to the symbol's `[line, end_line]` body and
/// return the result as an inclusive `(start, end)` tuple suitable for
/// [`AffectedSymbol::changed_lines`].
///
/// For pure-deletion hunks (empty new range), reports the symbol's full range
/// since the deletion site borders the body — picking one endpoint would
/// hide useful context. For [`Hunk::ALL`] the formula naturally yields
/// `(line, end_line)`, so binary/oversized fallback callers see the symbol's
/// own range without a separate code path.
fn intersection_inclusive(h: &Hunk, line: usize, end_line: usize) -> (usize, usize) {
    if h.new.is_empty() {
        return (line, end_line);
    }
    let lo = line.max(h.new.start);
    let hi = end_line.min(h.new.end.saturating_sub(1));
    if hi < lo { (lo, lo) } else { (lo, hi) }
}

/// A public symbol whose definition is touched by a hunk in a changed file.
///
/// `dependent_count` reports the **transitive** total (direct + 2nd/3rd-order
/// dependents up to [`crate::DEFAULT_TRANSITIVE_DEPTH`]); `direct_dependent_count`
/// is the subset that imports this symbol by name. `changed_lines` lists the
/// inclusive `(start, end)` line ranges (1-based) where each intersecting hunk
/// overlaps the symbol's `[line, end_line]` range — empty means the entire
/// symbol body is untouched (in which case the symbol is omitted from the
/// result).
#[derive(Debug, Clone, Serialize)]
pub struct AffectedSymbol {
    /// Symbol name (function, type, or export).
    pub name: String,
    /// File path where the symbol is defined.
    pub file: String,
    /// Kind of symbol: "function", "type", or "export".
    pub kind: String,
    /// Total transitive dependent count: every file that imports this symbol
    /// directly, plus every file reachable through up to
    /// `DEFAULT_TRANSITIVE_DEPTH` import hops (file-level over-approximation —
    /// transitive entries are flagged for any symbol with at least one direct
    /// importer in the changed file).
    pub dependent_count: usize,
    /// Number of files that import this symbol *directly* (depth = 1, with the
    /// symbol's name appearing in the import statement). Always
    /// `<= dependent_count`.
    #[serde(default)]
    pub direct_dependent_count: usize,
    /// Up to 5 direct dependent file references.
    pub dependents: Vec<DependentRef>,
    /// Inclusive `(start_line, end_line)` ranges (1-based) where the changed
    /// hunks overlap this symbol's body. One tuple per intersecting hunk;
    /// empty means the symbol's body was not touched (in which case the symbol
    /// is excluded from `compute_affected_symbols`'s output).
    #[serde(default)]
    pub changed_lines: Vec<(usize, usize)>,
    /// Blast radius classification: "low", "medium", or "high". Computed from
    /// the **transitive** `dependent_count` since US-009.
    pub blast_radius: BlastRadius,
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
    /// Convention topic/category.
    pub topic: String,
    /// Convention description.
    pub description: String,
    /// Affected file that contributes evidence.
    pub affected_file: String,
    /// Confidence percentage (0–100).
    pub confidence_pct: f64,
    /// Convention weight (rule, strong, etc.).
    pub weight: String,
    /// Adoption statistics.
    pub adoption: AdoptionSummary,
    /// Whether the affected file is a golden file for this convention.
    pub is_golden_file: bool,
    /// Human-readable note about the risk.
    pub note: String,
}

/// Summary of convention adoption statistics across the project.
#[derive(Debug, Clone, Serialize)]
pub struct AdoptionSummary {
    /// Number of files that follow the convention.
    pub count: usize,
    /// Total number of files examined.
    pub total: usize,
    /// Adoption rate as a percentage.
    pub rate_pct: f64,
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
    pub risk: BlastRadius,
}

/// Metadata about the diff impact analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactMetadata {
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
    /// Total number of hunks observed across every analyzable changed file
    /// (Modified/Added). A binary or oversized file contributes a single
    /// `Hunk::ALL` to this count. Files filtered out before hunk computation
    /// (Deleted/Untracked/Conflicted) contribute zero.
    #[serde(default)]
    pub total_hunks: usize,
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
///
/// Thin wrapper around [`enumerate_changes_with_blobs`] — kept for callers
/// that don't need blob ObjectIds.
#[tracing::instrument(skip_all, fields(repo_path = %repo_path.display()))]
pub fn get_changed_files(
    repo_path: &Path,
    staged_only: bool,
    base: Option<&str>,
) -> Result<Vec<ChangedFile>, GraphError> {
    let with_blobs = enumerate_changes_with_blobs(repo_path, staged_only, base)?;
    Ok(with_blobs
        .into_iter()
        .map(|c| ChangedFile {
            path: c.path,
            status: c.status,
        })
        .collect())
}

/// Like [`get_changed_files`] but also returns the base/index blob ObjectIds
/// for each changed file so callers can read both sides of the diff for
/// hunk-level analysis. See [`ChangedFileWithBlobs`] for the per-status
/// blob-ID semantics and [`read_blob_pair`] for the canonical reader.
#[tracing::instrument(skip_all, fields(repo_path = %repo_path.display()))]
pub fn enumerate_changes_with_blobs(
    repo_path: &Path,
    staged_only: bool,
    base: Option<&str>,
) -> Result<Vec<ChangedFileWithBlobs>, GraphError> {
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
    let mut staged_paths: HashMap<String, ChangedFileWithBlobs> = HashMap::new();

    for entry in index.entries() {
        let rel_path = entry.path(&index).to_string();

        match head_entry_ids.get(&rel_path) {
            None => {
                staged_paths.insert(
                    rel_path.clone(),
                    ChangedFileWithBlobs {
                        path: rel_path,
                        status: FileStatus::Added,
                        base_blob_id: None,
                        index_blob_id: Some(entry.id),
                    },
                );
            }
            Some(head_id) => {
                if entry.id != *head_id {
                    staged_paths.insert(
                        rel_path.clone(),
                        ChangedFileWithBlobs {
                            path: rel_path,
                            status: FileStatus::Modified,
                            base_blob_id: Some(*head_id),
                            index_blob_id: Some(entry.id),
                        },
                    );
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
    for (path, head_id) in &head_entry_ids {
        if !index_path_set.contains(path) {
            staged_paths.insert(
                path.clone(),
                ChangedFileWithBlobs {
                    path: path.clone(),
                    status: FileStatus::Deleted,
                    base_blob_id: Some(*head_id),
                    index_blob_id: None,
                },
            );
        }
    }

    let mut changed_files: Vec<ChangedFileWithBlobs> = Vec::new();

    // 2. Unstaged changes: for files in the index, check if disk content
    //    differs from the index entry's ObjectId.
    let mut unstaged_paths: HashMap<String, ChangedFileWithBlobs> = HashMap::new();

    if !staged_only {
        for entry in index.entries() {
            let rel_path = entry.path(&index).to_string();
            let full_path = repo_path.join(&rel_path);

            if !full_path.exists() {
                unstaged_paths
                    .entry(rel_path.clone())
                    .or_insert(ChangedFileWithBlobs {
                        path: rel_path,
                        status: FileStatus::Deleted,
                        base_blob_id: head_entry_ids
                            .get(entry.path(&index).to_string().as_str())
                            .copied(),
                        index_blob_id: Some(entry.id),
                    });
                continue;
            }

            if let Some(disk_oid) = hash_file_on_disk(&full_path) {
                if disk_oid != entry.id {
                    unstaged_paths
                        .entry(rel_path.clone())
                        .or_insert(ChangedFileWithBlobs {
                            path: rel_path,
                            status: FileStatus::Modified,
                            base_blob_id: head_entry_ids
                                .get(entry.path(&index).to_string().as_str())
                                .copied(),
                            index_blob_id: Some(entry.id),
                        });
                }
            }
        }

        // 3. Untracked files: in worktree but not in index or HEAD tree.
        //    WalkBuilder respects .gitignore so target/, .claude/, etc. are excluded.
        let known_paths = collect_tree_paths(&head_tree)?;
        let staged_keys: HashSet<String> = staged_paths.keys().cloned().collect();
        let unstaged_keys: HashSet<String> = unstaged_paths.keys().cloned().collect();
        for path in collect_worktree_paths(repo_path) {
            let path_str = path.to_string_lossy().to_string();
            if !known_paths.contains(&path_str)
                && !staged_keys.contains(&path_str)
                && !unstaged_keys.contains(&path_str)
            {
                changed_files.push(ChangedFileWithBlobs {
                    path: path_str,
                    status: FileStatus::Untracked,
                    base_blob_id: None,
                    index_blob_id: None,
                });
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
                    staged_paths.insert(
                        rel_path.clone(),
                        ChangedFileWithBlobs {
                            path: rel_path,
                            status: FileStatus::Added,
                            base_blob_id: None,
                            index_blob_id: Some(entry.id),
                        },
                    );
                }
                Some(base_id) => {
                    if entry.id != *base_id {
                        staged_paths.insert(
                            rel_path.clone(),
                            ChangedFileWithBlobs {
                                path: rel_path,
                                status: FileStatus::Modified,
                                base_blob_id: Some(*base_id),
                                index_blob_id: Some(entry.id),
                            },
                        );
                    }
                }
            }
        }

        let index_path_set: HashSet<String> = index
            .entries()
            .iter()
            .map(|e| e.path(&index).to_string())
            .collect();
        for (path, base_id) in &base_entry_ids {
            if !index_path_set.contains(path) {
                staged_paths.insert(
                    path.clone(),
                    ChangedFileWithBlobs {
                        path: path.clone(),
                        status: FileStatus::Deleted,
                        base_blob_id: Some(*base_id),
                        index_blob_id: None,
                    },
                );
            }
        }

        let base_known = collect_tree_paths(&base_tree)?;
        let staged_keys: HashSet<String> = staged_paths.keys().cloned().collect();
        for path in collect_worktree_paths(repo_path) {
            let path_str = path.to_string_lossy().to_string();
            if !base_known.contains(&path_str) && !staged_keys.contains(&path_str) {
                changed_files.push(ChangedFileWithBlobs {
                    path: path_str,
                    status: FileStatus::Untracked,
                    base_blob_id: None,
                    index_blob_id: None,
                });
            }
        }
    } else if staged_only {
        for (_, info) in staged_paths {
            changed_files.push(info);
        }
        mark_conflicts(repo_path, &mut changed_files);
        changed_files.sort_by(|a, b| a.path.cmp(&b.path));
        return Ok(changed_files);
    }

    // Merge staged and unstaged into a single de-duplicated list.
    for (_, info) in staged_paths {
        changed_files.push(info);
    }
    for (path, info) in unstaged_paths {
        if !changed_files.iter().any(|c| c.path == path) {
            changed_files.push(info);
        }
    }

    mark_conflicts(repo_path, &mut changed_files);
    changed_files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changed_files)
}

/// Compute affected symbols from changed files using **hunk-level**
/// granularity.
///
/// For each changed file with a recoverable blob pair, the diff is computed
/// (Histogram algorithm — same as git) and only the symbols whose
/// `[line, end_line]` body intersects a hunk are reported. Symbols whose
/// definition lies between hunks are excluded — a regression on the
/// pre-US-009 behaviour where every public symbol in any modified file was
/// returned.
///
/// Per-status handling:
/// - **Modified**: `read_blob_pair` against `base_blob_id` and either the
///   index blob (`staged_only=true`) or the working-tree file. Binary,
///   oversized, or missing-on-disk blobs fall back to [`Hunk::ALL`].
/// - **Added**: `base_blob_id == None`, so the diff covers the entire new
///   file as a single insertion hunk; intersection with each symbol's
///   range yields `[(line, end_line)]`.
/// - **Deleted**: V1 limitation — the old IR is not reloaded, so deleted
///   files appear in the response's `changed_files` but produce no
///   per-symbol entries.
/// - **Untracked / Conflicted**: skipped entirely (preserves existing
///   behaviour).
///
/// Dependent counts are computed against [`DEFAULT_TRANSITIVE_DEPTH`] so
/// `dependent_count` reports the file-level transitive total (direct +
/// 2nd/3rd-order) and `direct_dependent_count` reports the per-symbol
/// direct count. Symbols with zero direct importers are dropped from the
/// output.
pub fn compute_affected_symbols(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    changed_files: &[ChangedFileWithBlobs],
    repo_path: &Path,
    staged_only: bool,
) -> Result<(Vec<AffectedSymbol>, usize), GraphError> {
    let analyzable: Vec<&ChangedFileWithBlobs> = changed_files
        .iter()
        .filter(|c| {
            !matches!(
                c.status,
                FileStatus::Deleted | FileStatus::Untracked | FileStatus::Conflicted
            )
        })
        .collect();

    if analyzable.is_empty() {
        return Ok((Vec::new(), 0));
    }

    let loaded_ir = load_branch_ir(conn, branch_id)?;
    let files = &loaded_ir.files;

    let path_strs: Vec<String> = analyzable.iter().map(|c| c.path.clone()).collect();
    let dep_results = query_dependencies_batch(
        conn,
        branch_id,
        &path_strs,
        crate::dependencies::QueryDependenciesOptions {
            depth: crate::dependencies::DEFAULT_TRANSITIVE_DEPTH,
        },
    )?;

    let dep_map: HashMap<&str, &crate::dependencies::DependencyData> =
        dep_results.iter().map(|d| (d.target.as_str(), d)).collect();

    // Open the gix repo lazily — we only need it when at least one analyzable
    // file requires hunk computation. `gix::open` is cheap relative to the
    // diff cost so re-using a single handle across all changed files is
    // worthwhile.
    let gix_repo = gix::open(repo_path).map_err(|e| {
        GraphError::query(format!(
            "Not a git repository: {} — {e}",
            repo_path.display()
        ))
    })?;

    let mut symbols = Vec::new();
    let mut total_hunks: usize = 0;

    for changed in &analyzable {
        let file = files.iter().find(|f| {
            let stored = f.path.to_string_lossy().to_string();
            stored == changed.path
                || crate::dependencies::suffix_matches_at_boundary(&stored, &changed.path)
        });

        let Some(file) = file else {
            continue;
        };

        let dep_info = dep_map.get(file.path.to_string_lossy().as_ref());

        // Compute hunks once per changed file. Empty result (= no real
        // textual change) is treated as a no-op for symbol intersection:
        // every symbol in the file is excluded.
        let hunks = match read_blob_pair(
            &gix_repo,
            repo_path,
            &changed.path,
            changed.base_blob_id,
            changed.index_blob_id,
            staged_only,
        )? {
            Some((old, new)) => diff_blobs_to_hunks(&old, &new),
            None => vec![Hunk::ALL],
        };

        if hunks.is_empty() {
            // No textual change — skip this file entirely.
            continue;
        }

        total_hunks += hunks.len();

        let file_path_str = file.path.to_string_lossy().to_string();

        for export in &file.exports {
            push_affected_symbol(
                &mut symbols,
                &export.name,
                &file_path_str,
                "export",
                export.line,
                export.end_line,
                &hunks,
                dep_info,
            );
        }

        for func in file.functions.iter().filter(|f| f.is_public) {
            push_affected_symbol(
                &mut symbols,
                &func.name,
                &file_path_str,
                "function",
                func.line,
                func.end_line,
                &hunks,
                dep_info,
            );
        }

        for typ in file.types.iter().filter(|t| t.is_public) {
            push_affected_symbol(
                &mut symbols,
                &typ.name,
                &file_path_str,
                "type",
                typ.line,
                typ.end_line,
                &hunks,
                dep_info,
            );
        }
    }

    // Deduplicate by (name, file): the same symbol may appear as both an
    // export and a type/function (e.g. `GraphError` is exported AND is a
    // TypeDef). Keep the entry with the highest dependent_count; ties prefer
    // "export" > "function" > "type" so the most descriptive kind survives.
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for (i, sym) in symbols.iter().enumerate() {
        let key = (sym.name.clone(), sym.file.clone());
        seen.entry(key)
            .and_modify(|best_idx| {
                let best = &symbols[*best_idx];
                let better = sym.dependent_count > best.dependent_count
                    || (sym.dependent_count == best.dependent_count
                        && kind_rank(&sym.kind) < kind_rank(&best.kind));
                if better {
                    *best_idx = i;
                }
            })
            .or_insert(i);
    }
    let mut best_indices: Vec<usize> = seen.into_values().collect();
    best_indices.sort_unstable();
    let deduped: Vec<AffectedSymbol> = best_indices
        .into_iter()
        .map(|i| symbols[i].clone())
        .collect();

    Ok((deduped, total_hunks))
}

/// Rank symbol kinds for deduplication tie-breaking (lower = preferred).
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "export" => 0,
        "function" => 1,
        "type" => 2,
        _ => 3,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_affected_symbol(
    symbols: &mut Vec<AffectedSymbol>,
    name: &str,
    file_path: &str,
    kind: &str,
    line: usize,
    end_line: usize,
    hunks: &[Hunk],
    dep_info: Option<&&crate::dependencies::DependencyData>,
) {
    // Determine the symbol's effective body range. Some IR producers may emit
    // `end_line == 0` for single-line declarations that pre-date schema v8;
    // fall back to the start line in that case so legacy IR still intersects
    // hunks correctly.
    let effective_end = if end_line == 0 || end_line < line {
        line
    } else {
        end_line
    };

    // 1. Skip the symbol when no hunk overlaps its body.
    let intersecting = symbol_intersects_hunks(line, effective_end, hunks);
    if intersecting.is_empty() {
        return;
    }

    // 2. Only include direct dependents that explicitly import this symbol
    //    by name. Wildcard imports (`*`) count as importing everything.
    //    Transitive entries (depth >= 2) are discovered file-level by the
    //    BFS — they don't carry per-symbol import metadata, so they are
    //    rolled into the transitive total below if any direct importer
    //    exists for this symbol.
    let direct_dependents: Vec<&crate::dependencies::DependentEntry> = dep_info
        .map(|d| {
            d.dependents
                .iter()
                .filter(|dep| {
                    dep.depth == 1 && dep.import_names.iter().any(|n| n == name || n == "*")
                })
                .collect()
        })
        .unwrap_or_default();

    // Skip the symbol entirely if nothing actually imports it by name.
    if direct_dependents.is_empty() {
        return;
    }

    let direct_dependent_count = direct_dependents.len();

    // Transitive entries (depth >= 2) are flagged conservatively for any
    // symbol with at least one direct importer — at the IR level we cannot
    // tell which transitive chain originated from which direct import. This
    // is a file-level over-approximation and matches the "blast radius"
    // intent of `dependent_count`.
    let transitive_only_count = dep_info
        .map(|d| d.dependents.iter().filter(|e| e.depth >= 2).count())
        .unwrap_or(0);
    let dependent_count = direct_dependent_count + transitive_only_count;

    let dependents = direct_dependents
        .iter()
        .map(|dep| DependentRef {
            file: dep.file_path.clone(),
            line: dep.line,
        })
        .take(5)
        .collect();

    let changed_lines: Vec<(usize, usize)> = intersecting
        .iter()
        .map(|h| intersection_inclusive(h, line, effective_end))
        .collect();

    symbols.push(AffectedSymbol {
        name: name.to_owned(),
        file: file_path.to_owned(),
        kind: kind.to_owned(),
        dependent_count,
        direct_dependent_count,
        dependents,
        changed_lines,
        blast_radius: dependencies::classify_blast_radius(dependent_count),
    });
}

/// Compute convention risks by matching changed files against convention
/// evidence stored in `ext_data`.
///
/// Uses `json_each(json_extract(ext_data, '$.evidence'))` to batch-match
/// changed files against convention nodes. Only conventions with
/// `weight IN ('rule','strong')` OR `adoption_count >= 3` are considered.
/// Results are grouped by (description, affected_file).
///
/// Golden file status is determined by comparing the affected file against
/// the top convention-compliant files from `get_golden_files()`. Golden file
/// status does NOT inflate blast_radius_summary.risk.
pub fn compute_convention_risks(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    changed_files: &[ChangedFile],
) -> Result<Vec<ConventionRisk>, GraphError> {
    if changed_files.is_empty() {
        return Ok(Vec::new());
    }

    let analyzable: Vec<&ChangedFile> = changed_files
        .iter()
        .filter(|c| !matches!(c.status, FileStatus::Untracked | FileStatus::Conflicted))
        .collect();

    if analyzable.is_empty() {
        return Ok(Vec::new());
    }

    let golden = get_golden_files(conn, &BranchId::from(branch_id), DEFAULT_GOLDEN_FILES_LIMIT)
        .unwrap_or_default();
    let golden_paths: std::collections::HashSet<String> =
        golden.iter().map(|g| g.path.clone()).collect();

    let conn_guard = crate::lock_conn(conn)?;

    let mut risks = Vec::new();

    let sql = "SELECT n.description, n.confidence, n.adoption_count, n.total_count, n.weight,
                      je.value ->> '$.file' AS evidence_file,
                      COALESCE(json_extract(n.ext_data, '$.detector_name'), 'convention') AS topic
               FROM nodes n,
                    json_each(json_extract(n.ext_data, '$.evidence')) AS je
               WHERE n.branch_id = ?1
                 AND COALESCE(json_extract(n.ext_data, '$.removed'), 0) NOT IN (1, 'true', '1')
                 AND n.confidence >= 0.50
                 AND json_type(n.ext_data, '$.evidence') = 'array'
                 AND (n.weight IN ('rule','strong') OR n.adoption_count >= 3)";

    let mut stmt = conn_guard.prepare(sql).map_err(GraphError::query)?;

    let rows = stmt
        .query_map(rusqlite::params![branch_id], |row| {
            let description: String = row.get(0)?;
            let confidence: f64 = row.get(1)?;
            let adoption_count_i64: i64 = row.get(2)?;
            let total_count_i64: i64 = row.get(3)?;
            let weight: String = row.get(4)?;
            let evidence_file: Option<String> = row.get(5)?;
            let topic: String = row.get(6)?;
            Ok((
                description,
                confidence,
                adoption_count_i64,
                total_count_i64,
                weight,
                evidence_file,
                topic,
            ))
        })
        .map_err(GraphError::query)?;

    let mut seen: HashSet<(String, String)> = HashSet::new();

    for row in rows {
        let (
            description,
            confidence,
            adoption_count_i64,
            total_count_i64,
            weight,
            evidence_file,
            topic,
        ) = row.map_err(GraphError::query)?;
        let adoption_count = adoption_count_i64.max(0) as usize;
        let total_count = total_count_i64.max(0) as usize;

        let Some(ev_file) = evidence_file else {
            continue;
        };

        let matched = analyzable.iter().find(|c| {
            let p = &c.path;
            ev_file == *p || ev_file.ends_with(&format!("/{p}"))
        });

        let Some(changed) = matched else {
            continue;
        };

        let key = (description.clone(), changed.path.clone());
        if seen.contains(&key) {
            continue;
        }

        let confidence_pct = (confidence.clamp(0.0, 1.0) * 100.0).round();
        let is_golden = golden_paths.contains(&changed.path);

        let adoption = AdoptionSummary {
            count: adoption_count,
            total: total_count,
            rate_pct: if total_count > 0 {
                ((adoption_count as f64 / total_count as f64) * 100.0).round()
            } else {
                0.0
            },
        };

        let note = if changed.status == FileStatus::Deleted {
            format!(
                "{} was evidence for the '{}' convention. After deletion, the convention's confidence may decrease.",
                changed.path, description
            )
        } else if is_golden {
            format!(
                "{} is a golden file for this convention — it has the highest compliance score in the project. If you intentionally evolve this pattern, consider calling record_decision afterwards to update the convention baseline.",
                changed.path
            )
        } else {
            format!(
                "{} contributes evidence to the '{}' convention ({}% confidence, {}/{} files follow). Changing this file may reduce its convention compliance.",
                changed.path, description, confidence_pct, adoption_count, total_count
            )
        };

        seen.insert(key);

        risks.push(ConventionRisk {
            topic: topic.clone(),
            description,
            affected_file: changed.path.clone(),
            confidence_pct,
            weight: weight.clone(),
            adoption,
            is_golden_file: is_golden,
            note,
        });
    }

    Ok(risks)
}

// ── map_diff_impact orchestration ─────────────────────────────

/// Orchestrate the full diff impact analysis: identify changed files,
/// compute affected symbols, identify convention risks, and generate
/// a blast radius summary with actionable next steps.
///
/// This is the single entry point that ties together:
/// 1. `get_changed_files()` — git diff against HEAD/index/base
/// 2. `compute_affected_symbols()` — exports + public functions with dependents
/// 3. `compute_convention_risks()` — convention evidence matches
pub fn map_diff_impact(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    repo_path: &Path,
    request: &DiffImpactRequest,
) -> Result<DiffImpactData, GraphError> {
    let changed_with_blobs =
        enumerate_changes_with_blobs(repo_path, request.staged_only, request.base.as_deref())?;

    // Mirror the with-blobs list down to the simpler `ChangedFile` shape for
    // the response data and for `compute_convention_risks` which doesn't
    // need blob ObjectIds.
    let changed_files: Vec<ChangedFile> = changed_with_blobs
        .iter()
        .map(|c| ChangedFile {
            path: c.path.clone(),
            status: c.status,
        })
        .collect();

    let (affected_symbols, total_hunks) = compute_affected_symbols(
        conn,
        branch_id,
        &changed_with_blobs,
        repo_path,
        request.staged_only,
    )?;
    let convention_risks = compute_convention_risks(conn, branch_id, &changed_files)?;

    // Sum max(dependent_count) per changed file — avoids double-counting when
    // multiple symbols from the same file all report the same 27 dependents.
    // Using the truncated `sym.dependents` list (max 5) would give wrong results;
    // `dependent_count` is the accurate full count from the IR.
    let total_dependents: usize = {
        let mut per_file: HashMap<&str, usize> = HashMap::new();
        for sym in &affected_symbols {
            per_file
                .entry(sym.file.as_str())
                .and_modify(|v| *v = (*v).max(sym.dependent_count))
                .or_insert(sym.dependent_count);
        }
        per_file.values().sum()
    };
    let total_affected_symbols = affected_symbols.len();
    let total_changed_files = changed_files.len();

    let risk = compute_overall_risk(&affected_symbols);

    let blast_radius_summary = BlastRadiusSummary {
        total_dependents,
        total_affected_symbols,
        total_changed_files,
        risk,
    };

    let branch = branch_id.to_owned();

    let metadata = ImpactMetadata { branch };

    Ok(DiffImpactData {
        changed_files,
        affected_symbols,
        convention_risks,
        blast_radius_summary,
        total_hunks,
        metadata,
    })
}

/// Compute overall risk level from the max blast radius among affected symbols.
fn compute_overall_risk(affected_symbols: &[AffectedSymbol]) -> BlastRadius {
    if affected_symbols.is_empty() {
        return BlastRadius::None;
    }

    let has_high = affected_symbols
        .iter()
        .any(|s| s.blast_radius == BlastRadius::High);
    let has_medium = affected_symbols
        .iter()
        .any(|s| s.blast_radius == BlastRadius::Medium);

    if has_high {
        BlastRadius::High
    } else if has_medium {
        BlastRadius::Medium
    } else {
        BlastRadius::Low
    }
}

// ── Internal helpers ────────────────────────────────────────────

/// Mark files containing conflict markers as Conflicted.
fn mark_conflicts(repo_path: &Path, changed_files: &mut [ChangedFileWithBlobs]) {
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

/// Resolve a base reference (branch name, tag, remote ref, or commit hash) to a Tree.
fn resolve_base_tree<'repo>(
    repo: &'repo gix::Repository,
    base_ref: &str,
) -> Result<gix::Tree<'repo>, GraphError> {
    let ref_candidates = [
        format!("refs/heads/{base_ref}"),
        format!("refs/tags/{base_ref}"),
        format!("refs/remotes/{base_ref}"),
        base_ref.to_owned(),
    ];

    let mut oid = None;

    for ref_name in &ref_candidates {
        match repo.try_find_reference(ref_name) {
            Ok(Some(reference)) => {
                oid = Some(
                    reference
                        .into_fully_peeled_id()
                        .map_err(|e| {
                            GraphError::query(format!("Failed to peel reference '{base_ref}': {e}"))
                        })?
                        .detach(),
                );
                break;
            }
            Ok(None) => {}
            Err(_) => {}
        }
    }

    if oid.is_none() {
        if let Ok(id) = gix::ObjectId::from_hex(base_ref.as_bytes()) {
            oid = Some(id);
        } else {
            return Err(GraphError::query(format!(
                "Cannot resolve base reference '{base_ref}'"
            )));
        }
    }

    let oid = oid.unwrap();

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

/// Max file size to hash on disk — skip larger files to avoid OOM.
const MAX_HASH_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Maximum blob size considered for hunk-level diffing. Blobs larger than
/// this on either side cause [`read_blob_pair`] to return `Ok(None)` so the
/// caller can fall back to [`Hunk::ALL`].
pub const MAX_DIFF_FILE_SIZE: usize = 5 * 1024 * 1024;

/// Number of leading bytes scanned by [`is_binary_blob`].
const BINARY_PROBE_LEN: usize = 8 * 1024;

/// Raw `(old_bytes, new_bytes)` pair returned by [`read_blob_pair`].
pub type BlobPair = (Vec<u8>, Vec<u8>);

/// Heuristic used by git itself: a blob is treated as binary if a NUL byte
/// appears in the first 8 KiB.
fn is_binary_blob(bytes: &[u8]) -> bool {
    let probe_len = bytes.len().min(BINARY_PROBE_LEN);
    bytes[..probe_len].contains(&0)
}

/// Read both sides of a file diff as raw bytes for hunk computation.
///
/// Returns:
/// - `Ok(Some((old_bytes, new_bytes)))` when both sides could be read and
///   neither is binary or oversized.
/// - `Ok(None)` when either side is binary (NUL byte in the first
///   [`BINARY_PROBE_LEN`] bytes), oversized (> [`MAX_DIFF_FILE_SIZE`]), or
///   missing on disk in non-`staged_only` mode. Callers should fall back
///   to [`Hunk::ALL`] in this case.
/// - `Err` when a git or I/O error prevents reading.
///
/// Routing of the new side:
/// - `staged_only == true` ⇒ read the blob identified by `index_blob_id`
///   from the object DB. `None` ⇒ new side is empty (deleted in index).
/// - `staged_only == false` ⇒ read the working-tree file at
///   `repo_path/rel_path`. Missing file ⇒ `Ok(None)`.
///
/// Routing of the old side:
/// - `Some(oid)` ⇒ read the blob from the object DB.
/// - `None` ⇒ old side is empty (Added or Untracked).
pub fn read_blob_pair(
    repo: &gix::Repository,
    repo_path: &Path,
    rel_path: &str,
    base_blob_id: Option<gix::ObjectId>,
    index_blob_id: Option<gix::ObjectId>,
    staged_only: bool,
) -> Result<Option<BlobPair>, GraphError> {
    let old_bytes = match base_blob_id {
        Some(oid) => match read_blob_bytes(repo, oid)? {
            Some(b) => b,
            None => return Ok(None),
        },
        None => Vec::new(),
    };

    let new_bytes = if staged_only {
        match index_blob_id {
            Some(oid) => match read_blob_bytes(repo, oid)? {
                Some(b) => b,
                None => return Ok(None),
            },
            None => Vec::new(),
        }
    } else {
        let full_path = repo_path.join(rel_path);
        match read_disk_file_bytes(&full_path)? {
            Some(b) => b,
            None => return Ok(None),
        }
    };

    if is_binary_blob(&old_bytes) || is_binary_blob(&new_bytes) {
        return Ok(None);
    }

    Ok(Some((old_bytes, new_bytes)))
}

/// Read a blob's bytes from the object DB. Returns `Ok(None)` when the
/// blob exceeds [`MAX_DIFF_FILE_SIZE`].
fn read_blob_bytes(
    repo: &gix::Repository,
    oid: gix::ObjectId,
) -> Result<Option<Vec<u8>>, GraphError> {
    let obj = repo
        .find_object(oid)
        .map_err(|e| GraphError::query(format!("Failed to find blob {oid}: {e}")))?;

    if obj.data.len() > MAX_DIFF_FILE_SIZE {
        return Ok(None);
    }
    Ok(Some(obj.data.clone()))
}

/// Read a working-tree file's bytes. Returns `Ok(None)` when the file is
/// missing or exceeds [`MAX_DIFF_FILE_SIZE`].
fn read_disk_file_bytes(path: &Path) -> Result<Option<Vec<u8>>, GraphError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if meta.len() > MAX_DIFF_FILE_SIZE as u64 {
        return Ok(None);
    }
    let bytes = std::fs::read(path)
        .map_err(|e| GraphError::query(format!("Failed to read {}: {e}", path.display())))?;
    Ok(Some(bytes))
}

/// Collect worktree file paths respecting `.gitignore`, the global gitignore,
/// and `.git/info/exclude` — identical to what `git status` would consider.
///
/// Uses the `ignore` crate's `WalkBuilder` so that:
/// - `target/`, `.claude/`, and any other gitignored paths are silently skipped
/// - Hidden files that are *not* gitignored (e.g. `.env.local`) are included,
///   matching standard `git status` behaviour
/// - Symlinks are not followed (avoids cycles)
fn collect_worktree_paths(repo_path: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(repo_path)
        .hidden(false) // include hidden files — git status shows them too
        .git_ignore(true) // respect .gitignore
        .git_global(true) // respect ~/.gitconfig core.excludesFile
        .git_exclude(true) // respect .git/info/exclude
        .follow_links(false) // no symlink traversal
        .filter_entry(|e| {
            // Always skip the .git directory itself
            e.file_name() != std::ffi::OsStr::new(".git")
        })
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .filter_map(|e| {
            e.path()
                .strip_prefix(repo_path)
                .ok()
                .map(|rel| PathBuf::from(rel.to_string_lossy().replace('\\', "/")))
        })
        .collect()
}

/// Hash a file on disk using SHA-1 (blob header + content) and return its gix ObjectId.
fn hash_file_on_disk(path: &Path) -> Option<gix::ObjectId> {
    let Ok(meta) = std::fs::metadata(path) else {
        return None;
    };

    if meta.len() > MAX_HASH_FILE_SIZE {
        return None;
    }

    let Ok(bytes) = std::fs::read(path) else {
        return None;
    };

    let mut hasher = sha1::Sha1::new();
    let header = format!("blob {}\0", bytes.len());
    hasher.update(header.as_bytes());
    hasher.update(&bytes);

    let hash_bytes: [u8; 20] = hasher.finalize().into();
    Some(gix::ObjectId::Sha1(hash_bytes))
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
    use seshat_core::ProjectFile;
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

    // ── enumerate_changes_with_blobs / read_blob_pair tests ────

    #[test]
    fn enumerate_changes_staged_deleted_produces_no_index_blob_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("gone.txt"), "byebye").expect("write file");
        git_commit_all(&repo, "initial");

        fs::remove_file(repo.join("gone.txt")).expect("delete");
        Command::new("git")
            .args(["add", "gone.txt"])
            .current_dir(&repo)
            .output()
            .expect("git add deletion");

        let changes =
            enumerate_changes_with_blobs(&repo, true, None).expect("enumerate_changes_with_blobs");
        let entry = changes
            .iter()
            .find(|c| c.path == "gone.txt")
            .expect("gone.txt must appear in changes");

        assert_eq!(entry.status, FileStatus::Deleted);
        assert!(
            entry.base_blob_id.is_some(),
            "staged-deleted file must carry the HEAD blob as base_blob_id"
        );
        assert!(
            entry.index_blob_id.is_none(),
            "staged-deleted file must have index_blob_id == None, got {:?}",
            entry.index_blob_id
        );
    }

    #[test]
    fn enumerate_changes_added_produces_no_base_blob_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("seed.txt"), "seed").expect("write seed");
        git_commit_all(&repo, "initial");

        fs::write(repo.join("brand_new.txt"), "fresh").expect("write new");
        Command::new("git")
            .args(["add", "brand_new.txt"])
            .current_dir(&repo)
            .output()
            .expect("git add new");

        let changes =
            enumerate_changes_with_blobs(&repo, true, None).expect("enumerate_changes_with_blobs");
        let entry = changes
            .iter()
            .find(|c| c.path == "brand_new.txt")
            .expect("brand_new.txt must appear in changes");

        assert_eq!(entry.status, FileStatus::Added);
        assert!(
            entry.base_blob_id.is_none(),
            "added file must have base_blob_id == None, got {:?}",
            entry.base_blob_id
        );
        assert!(
            entry.index_blob_id.is_some(),
            "added file must carry an index_blob_id"
        );
    }

    #[test]
    fn read_blob_pair_returns_none_for_binary_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        // Write a binary blob (NUL byte well within the 8 KiB probe window)
        // and commit it.
        let binary_initial: Vec<u8> = b"\x00\x01\x02\x03\x04abc".to_vec();
        fs::write(repo.join("blob.bin"), &binary_initial).expect("write binary");
        git_commit_all(&repo, "initial");

        // Modify the binary file on disk so the diff is non-empty.
        let binary_modified: Vec<u8> = b"\x00\x01\x02\x03\x04XYZ".to_vec();
        fs::write(repo.join("blob.bin"), &binary_modified).expect("modify binary");

        let changes =
            enumerate_changes_with_blobs(&repo, false, None).expect("enumerate_changes_with_blobs");
        let entry = changes
            .iter()
            .find(|c| c.path == "blob.bin")
            .expect("blob.bin must appear as modified");
        assert_eq!(entry.status, FileStatus::Modified);

        let gix_repo = gix::open(&repo).expect("gix open");
        let pair = read_blob_pair(
            &gix_repo,
            &repo,
            &entry.path,
            entry.base_blob_id,
            entry.index_blob_id,
            false,
        )
        .expect("read_blob_pair");

        assert!(
            pair.is_none(),
            "binary blob must make read_blob_pair return Ok(None)"
        );
    }

    #[test]
    fn read_blob_pair_reads_text_blob_pair_in_staged_only_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("hello.txt"), "hello\n").expect("write");
        git_commit_all(&repo, "initial");

        // Stage a modification.
        fs::write(repo.join("hello.txt"), "hello world\n").expect("modify");
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(&repo)
            .output()
            .expect("git add");

        let changes =
            enumerate_changes_with_blobs(&repo, true, None).expect("enumerate_changes_with_blobs");
        let entry = changes
            .iter()
            .find(|c| c.path == "hello.txt")
            .expect("hello.txt must appear");
        assert_eq!(entry.status, FileStatus::Modified);

        let gix_repo = gix::open(&repo).expect("gix open");
        let pair = read_blob_pair(
            &gix_repo,
            &repo,
            &entry.path,
            entry.base_blob_id,
            entry.index_blob_id,
            true,
        )
        .expect("read_blob_pair");

        let (old, new) = pair.expect("text-vs-text diff must yield Some");
        assert_eq!(old, b"hello\n");
        assert_eq!(new, b"hello world\n");
    }

    // ── map_diff_impact integration tests ─────────────────────

    #[test]
    fn map_diff_impact_no_changes_returns_empty_none_risk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("hello.txt"), "hello").expect("write file");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();
        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");
        assert!(
            result.changed_files.is_empty(),
            "Expected no changes, got: {:?}",
            result.changed_files
        );
        assert!(result.affected_symbols.is_empty());
        assert!(result.convention_risks.is_empty());
        assert_eq!(result.blast_radius_summary.risk, BlastRadius::None);
        assert_eq!(result.blast_radius_summary.total_changed_files, 0);
        assert_eq!(result.blast_radius_summary.total_affected_symbols, 0);
    }

    #[test]
    fn map_diff_impact_modified_no_exports_returns_empty_symbols_none_risk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("plain.txt"), "hello").expect("write file");
        git_commit_all(&repo, "initial");

        // Insert IR for the committed file (no exports).
        let conn = crate::test_helpers::test_conn();
        let file = ProjectFile {
            path: std::path::PathBuf::from("plain.txt"),
            language: seshat_core::Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &file);

        fs::write(repo.join("plain.txt"), "hello world").expect("modify");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");
        assert_eq!(result.changed_files.len(), 1);
        assert_eq!(result.changed_files[0].status, FileStatus::Modified);
        assert!(
            result.affected_symbols.is_empty(),
            "Expected no affected symbols, got: {:?}",
            result.affected_symbols
        );
        assert_eq!(result.blast_radius_summary.risk, BlastRadius::None);
        assert_eq!(result.blast_radius_summary.total_changed_files, 1);
    }

    #[test]
    fn map_diff_impact_modified_with_dependent_symbols_returns_medium_risk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        // Create src/utils.ts that will be imported by other files.
        let utils_path = "src/utils.ts";
        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(repo.join(utils_path), "export function formatDate() {}").expect("write utils");
        git_commit_all(&repo, "initial");

        // Insert IR with import relationships.
        let conn = crate::test_helpers::test_conn();

        let utils = ProjectFile {
            path: std::path::PathBuf::from(utils_path),
            language: seshat_core::Language::TypeScript,
            content_hash: "u1".to_owned(),
            imports: Vec::new(),
            exports: vec![seshat_core::Export {
                name: "formatDate".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: vec![seshat_core::Function {
                name: "formatDate".to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                end_line: 5,
                parameters: Vec::new(),
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &utils);

        // Insert 5 dependent files that import from ../utils.
        for i in 1..=5 {
            let dep_file = ProjectFile {
                path: std::path::PathBuf::from(format!("src/module_{i}.ts")),
                language: seshat_core::Language::TypeScript,
                content_hash: format!("d{i}"),
                imports: vec![seshat_core::Import {
                    module: "./utils".to_owned(),
                    names: vec!["formatDate".to_owned()],
                    is_type_only: false,
                    line: 1,
                }],
                exports: Vec::new(),
                functions: Vec::new(),
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
                file_doc: None,
            };
            crate::test_helpers::insert_ir(&conn, "main", &dep_file);
        }

        // Modify utils.ts
        fs::write(
            repo.join(utils_path),
            "export function formatDate() { return 'today' }",
        )
        .expect("modify");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        let utils_sym = result
            .affected_symbols
            .iter()
            .find(|s| s.file.contains("utils"));
        assert!(
            utils_sym.is_some(),
            "Expected affected symbol for utils, got: {:?}",
            result.affected_symbols
        );
        let utils_sym = utils_sym.unwrap();
        assert_eq!(utils_sym.name, "formatDate");
        assert_eq!(utils_sym.kind, "export");
        assert_eq!(utils_sym.dependent_count, 5);
        assert_eq!(utils_sym.blast_radius, BlastRadius::Medium);
        assert_eq!(result.blast_radius_summary.risk, BlastRadius::Medium);
    }

    #[test]
    fn map_diff_impact_deleted_file_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("delete_me.rs"), "fn foo() {}").expect("write file");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();
        let file = ProjectFile {
            path: std::path::PathBuf::from("delete_me.rs"),
            language: seshat_core::Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: Vec::new(),
            exports: vec![seshat_core::Export {
                name: "foo".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: vec![seshat_core::Function {
                name: "foo".to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                end_line: 5,
                parameters: Vec::new(),
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &file);

        fs::remove_file(repo.join("delete_me.rs")).expect("delete");
        Command::new("git")
            .args(["add", "delete_me.rs"])
            .current_dir(&repo)
            .output()
            .expect("git add deletion");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");
        let deleted = result
            .changed_files
            .iter()
            .find(|c| c.path == "delete_me.rs");
        assert!(
            deleted.is_some(),
            "Expected delete_me.rs in changed files, got: {:?}",
            result.changed_files
        );
        assert_eq!(deleted.unwrap().status, FileStatus::Deleted);
        // Deleted files are excluded from affected_symbols
        assert!(
            result
                .affected_symbols
                .iter()
                .all(|s| !s.file.contains("delete_me")),
            "Deleted files should not appear in affected symbols"
        );
    }

    #[test]
    fn map_diff_impact_untracked_file_excluded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("tracked.txt"), "committed").expect("write file");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();

        // Write untracked file (not in git and not in IR).
        fs::write(repo.join("untracked.rs"), "fn secret() {}").expect("write untracked");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");
        let untracked = result
            .changed_files
            .iter()
            .find(|c| c.path.contains("untracked"));
        assert!(
            untracked.is_some(),
            "Expected untracked file in changed_files, got: {:?}",
            result.changed_files
        );
        assert_eq!(untracked.unwrap().status, FileStatus::Untracked);
        // Untracked files are excluded from affected_symbols and convention_risks.
        assert!(result.affected_symbols.is_empty());
        assert!(result.convention_risks.is_empty());
    }

    #[test]
    fn map_diff_impact_conflicted_file_excluded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("conflict.rs"), "fn safe() {}").expect("write file");
        git_commit_all(&repo, "initial");

        // Insert IR for the file.
        let conn = crate::test_helpers::test_conn();
        let file = ProjectFile {
            path: std::path::PathBuf::from("conflict.rs"),
            language: seshat_core::Language::Rust,
            content_hash: "c1".to_owned(),
            imports: Vec::new(),
            exports: vec![seshat_core::Export {
                name: "safe".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: vec![seshat_core::Function {
                name: "safe".to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                end_line: 5,
                parameters: Vec::new(),
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &file);

        let conflict_content = "<<<<<<< HEAD\nour change\n=======\ntheir change\n>>>>>>> branch\n";
        fs::write(repo.join("conflict.rs"), conflict_content).expect("write conflict");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");
        let conflicted = result
            .changed_files
            .iter()
            .find(|c| c.path == "conflict.rs");
        assert!(
            conflicted.is_some(),
            "Expected conflict.rs in changed_files"
        );
        assert_eq!(conflicted.unwrap().status, FileStatus::Conflicted);
        // Conflicted files are excluded from affected_symbols and convention_risks.
        assert!(
            result.affected_symbols.is_empty(),
            "Conflicted files should be excluded from symbols"
        );
        assert!(
            result.convention_risks.is_empty(),
            "Conflicted files should be excluded from convention risks"
        );
    }

    #[test]
    fn map_diff_impact_golden_file_modified_returns_is_golden_true() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        let golden_path = "src/golden.rs";
        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(repo.join(golden_path), "fn perfect() {}").expect("write golden");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();

        // Insert golden file IR with high convention_compliance_count.
        {
            let c = conn.lock().unwrap();
            let file = ProjectFile {
                path: std::path::PathBuf::from(golden_path),
                language: seshat_core::Language::Rust,
                content_hash: "gf1".to_owned(),
                imports: Vec::new(),
                exports: vec![seshat_core::Export {
                    name: "perfect".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 1,
                    end_line: 1,
                }],
                functions: vec![seshat_core::Function {
                    name: "perfect".to_owned(),
                    is_public: true,
                    is_async: false,
                    line: 1,
                    end_line: 5,
                    parameters: Vec::new(),
                    doc_comment: None,
                }],
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
                file_doc: None,
            };
            let ir_data = seshat_storage::serialize_ir(&file).expect("serialize");
            c.execute(
                "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data, convention_compliance_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    "main",
                    golden_path,
                    file.language.as_str(),
                    file.content_hash,
                    ir_data,
                    10,
                ],
            ).expect("insert golden IR");
        }

        // Insert convention node whose evidence references the golden file.
        {
            let c = conn.lock().unwrap();
            let ext = serde_json::json!({
                "source": "auto_detected",
                "detector_name": "test_detector",
                "trend": "stable",
                "evidence": [{
                    "file": golden_path,
                    "line": 1,
                    "end_line": 5,
                    "snippet": "fn perfect() {}"
                }]
            });
            c.execute(
                "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
                 VALUES (?1, 'convention', 'rule', ?2, 9, 10, ?3, ?4)",
                rusqlite::params![
                    "main",
                    0.95,
                    "Golden convention: always use fn prefix",
                    ext.to_string(),
                ],
            ).expect("insert convention node");
        }

        // Insert another non-golden IR file for same evidence path.
        {
            let c = conn.lock().unwrap();
            let file = ProjectFile {
                path: std::path::PathBuf::from("src/other.rs"),
                language: seshat_core::Language::Rust,
                content_hash: "o1".to_owned(),
                imports: Vec::new(),
                exports: Vec::new(),
                functions: Vec::new(),
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
                file_doc: None,
            };
            let ir_data = seshat_storage::serialize_ir(&file).expect("serialize");
            c.execute(
                "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data, convention_compliance_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params!["main", "src/other.rs", file.language.as_str(), file.content_hash, ir_data, 1],
            ).expect("insert other IR");
        }

        // Modify the golden file.
        fs::write(repo.join(golden_path), "fn perfect() { return 42 }").expect("modify golden");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");
        assert_eq!(result.changed_files.len(), 1);

        let risk = result
            .convention_risks
            .iter()
            .find(|r| r.affected_file == golden_path);
        assert!(
            risk.is_some(),
            "Expected convention risk for golden file, got: {:?}",
            result.convention_risks
        );
        let risk = risk.unwrap();
        assert!(risk.is_golden_file, "Expected is_golden_file: true");
        assert!(
            !risk.note.contains("WARNING"),
            "Golden file note should NOT contain WARNING, got: {}",
            risk.note
        );
        assert!(
            risk.note.contains("highest compliance score"),
            "Golden note should mention highest compliance, got: {}",
            risk.note
        );
        assert!(
            risk.note.contains("record_decision"),
            "Golden note should suggest record_decision, got: {}",
            risk.note
        );
    }

    #[test]
    fn map_diff_impact_detached_head_works_without_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::write(repo.join("hello.txt"), "hello").expect("write file");
        git_commit_all(&repo, "initial");

        // Get the commit hash.
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("rev-parse");
        let commit_hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();

        // Detach HEAD to that commit.
        Command::new("git")
            .args(["checkout", &commit_hash])
            .current_dir(&repo)
            .output()
            .expect("git checkout commit hash");

        let conn = crate::test_helpers::test_conn();
        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        // Should NOT error on detached HEAD.
        let result = map_diff_impact(&conn, "main", &repo, &request);
        assert!(
            result.is_ok(),
            "map_diff_impact should not error on detached HEAD, got: {result:?}"
        );
    }

    // ── Hunk extraction tests ──────────────────────────────────

    #[test]
    fn hunks_no_change_returns_empty() {
        let blob = b"a\nb\nc\n";
        let hunks = diff_blobs_to_hunks(blob, blob);
        assert!(hunks.is_empty(), "identical blobs should produce no hunks");
    }

    #[test]
    fn hunks_single_replacement_one_hunk() {
        let old = b"a\nb\nc\n";
        let new = b"a\nx\nc\n";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 1, "expected exactly one hunk");
        let h = hunks[0];
        assert_eq!(h.old, LineRange { start: 2, end: 3 });
        assert_eq!(h.new, LineRange { start: 2, end: 3 });
        assert!(!h.is_pure_insertion());
        assert!(!h.is_pure_deletion());
        assert!(h.touches_new_line(2));
        assert!(!h.touches_new_line(1));
        assert!(!h.touches_new_line(3));
    }

    #[test]
    fn hunks_pure_insertion_at_top() {
        let old = b"b\nc\n";
        let new = b"a\nb\nc\n";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        let h = hunks[0];
        assert!(h.is_pure_insertion(), "should be pure insertion");
        assert_eq!(h.old, LineRange { start: 1, end: 1 });
        assert_eq!(h.new, LineRange { start: 1, end: 2 });
        assert!(h.touches_new_line(1));
        assert!(!h.touches_new_line(2));
    }

    #[test]
    fn hunks_pure_insertion_at_bottom() {
        let old = b"a\nb\nc\n";
        let new = b"a\nb\nc\nd\n";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        let h = hunks[0];
        assert!(h.is_pure_insertion());
        assert_eq!(h.old, LineRange { start: 4, end: 4 });
        assert_eq!(h.new, LineRange { start: 4, end: 5 });
        assert!(h.touches_new_line(4));
        assert!(!h.touches_new_line(3));
    }

    #[test]
    fn hunks_pure_insertion_in_middle() {
        let old = b"a\nc\n";
        let new = b"a\nb\nc\n";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        let h = hunks[0];
        assert!(h.is_pure_insertion());
        assert_eq!(h.old, LineRange { start: 2, end: 2 });
        assert_eq!(h.new, LineRange { start: 2, end: 3 });
        assert!(h.touches_new_line(2));
        assert!(!h.touches_new_line(1));
        assert!(!h.touches_new_line(3));
    }

    #[test]
    fn hunks_pure_deletion() {
        let old = b"a\nb\nc\n";
        let new = b"a\nc\n";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        let h = hunks[0];
        assert!(h.is_pure_deletion(), "should be pure deletion");
        assert_eq!(h.old, LineRange { start: 2, end: 3 });
        assert_eq!(h.new, LineRange { start: 2, end: 2 });
        // Pure deletion: deletion sits between new lines 1 and 2; both
        // adjacent lines in the new file are reported as touched.
        assert!(h.touches_new_line(1));
        assert!(h.touches_new_line(2));
        assert!(!h.touches_new_line(3));
    }

    #[test]
    fn hunks_multi_hunk_two_replacements() {
        let old = b"a\nb\nc\nd\ne\nf\n";
        let new = b"a\nX\nc\nd\nY\nf\n";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 2, "expected two separate hunks");
        assert_eq!(hunks[0].old, LineRange { start: 2, end: 3 });
        assert_eq!(hunks[0].new, LineRange { start: 2, end: 3 });
        assert_eq!(hunks[1].old, LineRange { start: 5, end: 6 });
        assert_eq!(hunks[1].new, LineRange { start: 5, end: 6 });
    }

    #[test]
    fn hunks_empty_old_inserts_full_new_file() {
        let old: &[u8] = b"";
        let new = b"a\nb\nc\n";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        let h = hunks[0];
        assert!(h.is_pure_insertion());
        assert_eq!(h.old.start, h.old.end, "old range must be empty");
        assert_eq!(h.new, LineRange { start: 1, end: 4 });
        assert!(h.touches_new_line(1));
        assert!(h.touches_new_line(2));
        assert!(h.touches_new_line(3));
        assert!(!h.touches_new_line(4));
    }

    /// Symmetric counterpart of `hunks_empty_old_*`: the entire file is
    /// deleted (old has lines, new is empty). Locks the
    /// pure-deletion-of-everything case so it cannot regress to the
    /// modified-but-empty branch (which would mis-report a single
    /// pure-deletion hunk against a non-existent new side).
    #[test]
    fn hunks_empty_new_deletes_full_old_file() {
        let old = b"a\nb\nc\n";
        let new: &[u8] = b"";
        let hunks = diff_blobs_to_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        let h = hunks[0];
        assert!(h.is_pure_deletion());
        assert_eq!(h.old, LineRange { start: 1, end: 4 });
        assert_eq!(h.new.start, h.new.end, "new range must be empty");
        // Pure-deletion at the top: lines >= start (and the row above,
        // if any) are reported as touched.
        assert!(h.touches_new_line(1));
    }

    #[test]
    fn hunk_all_touches_every_line() {
        let h = Hunk::ALL;
        assert!(!h.is_pure_insertion());
        assert!(!h.is_pure_deletion());
        assert!(h.touches_new_line(1));
        assert!(h.touches_new_line(42));
        assert!(h.touches_new_line(usize::MAX - 1));
    }

    // ── compute_affected_symbols hunk-level integration tests ─────

    /// Build a canonical two-function TypeScript fixture: `foo` at
    /// lines 1-3, `bar` at lines 5-7, with a single dependent file that
    /// imports both names. Returns `(repo_path, conn, file_content)`.
    fn build_two_function_fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        Arc<Mutex<Connection>>,
        &'static str,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::create_dir_all(repo.join("src")).expect("create src");
        let utils_content =
            "export function foo() {\n  return 1;\n}\n\nexport function bar() {\n  return 2;\n}\n";
        fs::write(repo.join("src/utils.ts"), utils_content).expect("write utils");

        // Dependent that imports both foo and bar — supplies a direct
        // dependent for each symbol so push_affected_symbol does not
        // bail out on "no direct importers".
        fs::write(
            repo.join("src/consumer.ts"),
            "import { foo, bar } from './utils';\nfoo(); bar();\n",
        )
        .expect("write consumer");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();

        let utils_ir = ProjectFile {
            path: std::path::PathBuf::from("src/utils.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "u1".to_owned(),
            imports: Vec::new(),
            exports: vec![
                seshat_core::Export {
                    name: "foo".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 1,
                    end_line: 3,
                },
                seshat_core::Export {
                    name: "bar".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 5,
                    end_line: 7,
                },
            ],
            functions: vec![
                seshat_core::Function {
                    name: "foo".to_owned(),
                    is_public: true,
                    is_async: false,
                    line: 1,
                    end_line: 3,
                    parameters: Vec::new(),
                    doc_comment: None,
                },
                seshat_core::Function {
                    name: "bar".to_owned(),
                    is_public: true,
                    is_async: false,
                    line: 5,
                    end_line: 7,
                    parameters: Vec::new(),
                    doc_comment: None,
                },
            ],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &utils_ir);

        let consumer_ir = ProjectFile {
            path: std::path::PathBuf::from("src/consumer.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "c1".to_owned(),
            imports: vec![seshat_core::Import {
                module: "./utils".to_owned(),
                names: vec!["foo".to_owned(), "bar".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &consumer_ir);

        (dir, repo, conn, utils_content)
    }

    #[test]
    fn single_hunk_in_function_body_flags_only_that_function() {
        let (_dir, repo, conn, _initial) = build_two_function_fixture();

        // Modify line 2 (inside foo's body); leave bar untouched.
        let modified =
            "export function foo() {\n  return 99;\n}\n\nexport function bar() {\n  return 2;\n}\n";
        fs::write(repo.join("src/utils.ts"), modified).expect("modify utils");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        let foo = result
            .affected_symbols
            .iter()
            .find(|s| s.name == "foo")
            .expect("foo must be flagged");
        assert!(
            !foo.changed_lines.is_empty(),
            "foo must have changed_lines, got: {:?}",
            foo.changed_lines
        );
        // The hunk is at line 2 only — clamped to foo's [1, 3] body becomes (2, 2).
        assert_eq!(foo.changed_lines, vec![(2, 2)]);

        assert!(
            !result.affected_symbols.iter().any(|s| s.name == "bar"),
            "bar must not be flagged — its body [5, 7] does not intersect the hunk at line 2; got: {:?}",
            result.affected_symbols
        );
    }

    #[test]
    fn multi_hunk_flags_each_intersecting_symbol() {
        let (_dir, repo, conn, _initial) = build_two_function_fixture();

        // Modify line 2 (foo body) AND line 6 (bar body) — two hunks.
        let modified = "export function foo() {\n  return 99;\n}\n\nexport function bar() {\n  return 88;\n}\n";
        fs::write(repo.join("src/utils.ts"), modified).expect("modify utils");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        let foo = result
            .affected_symbols
            .iter()
            .find(|s| s.name == "foo")
            .expect("foo must be flagged");
        assert_eq!(foo.changed_lines, vec![(2, 2)]);

        let bar = result
            .affected_symbols
            .iter()
            .find(|s| s.name == "bar")
            .expect("bar must be flagged");
        assert_eq!(bar.changed_lines, vec![(6, 6)]);
    }

    #[test]
    fn hunk_between_symbols_flags_neither() {
        let (_dir, repo, conn, _initial) = build_two_function_fixture();

        // Modify line 4 (the blank line between foo and bar) only.
        let modified = "export function foo() {\n  return 1;\n}\n// gap comment\nexport function bar() {\n  return 2;\n}\n";
        fs::write(repo.join("src/utils.ts"), modified).expect("modify utils");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        // Neither foo (lines 1-3) nor bar (lines 5-7) overlap the hunk at line 4.
        assert!(
            !result.affected_symbols.iter().any(|s| s.name == "foo"),
            "foo must NOT be flagged for an inter-symbol gap edit, got: {:?}",
            result.affected_symbols
        );
        assert!(
            !result.affected_symbols.iter().any(|s| s.name == "bar"),
            "bar must NOT be flagged for an inter-symbol gap edit, got: {:?}",
            result.affected_symbols
        );
    }

    #[test]
    fn added_file_flags_all_symbols_with_their_ranges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        // Seed a committed file so HEAD is non-empty.
        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(repo.join("src/seed.ts"), "// seed\n").expect("write seed");
        git_commit_all(&repo, "initial");

        // Now write + stage a brand-new file.
        let new_file =
            "export function foo() {\n  return 1;\n}\n\nexport function bar() {\n  return 2;\n}\n";
        fs::write(repo.join("src/utils.ts"), new_file).expect("write utils");
        Command::new("git")
            .args(["add", "src/utils.ts"])
            .current_dir(&repo)
            .output()
            .expect("git add");

        // Insert IR + a consumer so direct_dependent_count > 0 for both symbols.
        let conn = crate::test_helpers::test_conn();
        let utils_ir = ProjectFile {
            path: std::path::PathBuf::from("src/utils.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "u1".to_owned(),
            imports: Vec::new(),
            exports: vec![
                seshat_core::Export {
                    name: "foo".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 1,
                    end_line: 3,
                },
                seshat_core::Export {
                    name: "bar".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 5,
                    end_line: 7,
                },
            ],
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &utils_ir);

        let consumer_ir = ProjectFile {
            path: std::path::PathBuf::from("src/consumer.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "c1".to_owned(),
            imports: vec![seshat_core::Import {
                module: "./utils".to_owned(),
                names: vec!["foo".to_owned(), "bar".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &consumer_ir);

        let request = DiffImpactRequest {
            staged_only: true,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        // The added file diff is one big insertion hunk; intersection with
        // each symbol's range collapses to (line, end_line) per AC #6.
        let foo = result
            .affected_symbols
            .iter()
            .find(|s| s.name == "foo")
            .expect("foo must be flagged on added file");
        assert_eq!(foo.changed_lines, vec![(1, 3)]);

        let bar = result
            .affected_symbols
            .iter()
            .find(|s| s.name == "bar")
            .expect("bar must be flagged on added file");
        assert_eq!(bar.changed_lines, vec![(5, 7)]);
    }

    #[test]
    fn deleted_file_reports_no_symbols_only_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(
            repo.join("src/utils.ts"),
            "export function foo() {\n  return 1;\n}\n",
        )
        .expect("write utils");
        fs::write(
            repo.join("src/consumer.ts"),
            "import { foo } from './utils';\nfoo();\n",
        )
        .expect("write consumer");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();
        let utils_ir = ProjectFile {
            path: std::path::PathBuf::from("src/utils.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "u1".to_owned(),
            imports: Vec::new(),
            exports: vec![seshat_core::Export {
                name: "foo".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 3,
            }],
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &utils_ir);
        let consumer_ir = ProjectFile {
            path: std::path::PathBuf::from("src/consumer.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "c1".to_owned(),
            imports: vec![seshat_core::Import {
                module: "./utils".to_owned(),
                names: vec!["foo".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &consumer_ir);

        // Stage a deletion of utils.ts.
        fs::remove_file(repo.join("src/utils.ts")).expect("delete");
        Command::new("git")
            .args(["add", "src/utils.ts"])
            .current_dir(&repo)
            .output()
            .expect("git add deletion");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        // utils.ts is in changed_files with status=Deleted.
        let utils = result
            .changed_files
            .iter()
            .find(|c| c.path == "src/utils.ts")
            .expect("utils.ts must appear in changed_files");
        assert_eq!(utils.status, FileStatus::Deleted);

        // V1 limitation: deleted files do NOT produce per-symbol entries.
        assert!(
            !result
                .affected_symbols
                .iter()
                .any(|s| s.file.contains("utils")),
            "deleted file must not produce any AffectedSymbol entries, got: {:?}",
            result.affected_symbols
        );
    }

    #[test]
    fn binary_modified_file_falls_back_to_hunk_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        // Use a `.rs` extension so the suffix resolver can match imports;
        // make the contents binary by injecting a NUL byte in the first 8
        // KiB (the heuristic git itself uses).
        fs::create_dir_all(repo.join("src")).expect("create src");
        let initial: Vec<u8> = b"// header\n\x00binary-fixture-bytes\n".to_vec();
        fs::write(repo.join("src/blob.rs"), &initial).expect("write binary");
        fs::write(
            repo.join("src/consumer.rs"),
            "use crate::blob::binary_export;\nfn main() { binary_export(); }\n",
        )
        .expect("write consumer");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();
        let blob_ir = ProjectFile {
            path: std::path::PathBuf::from("src/blob.rs"),
            language: seshat_core::Language::Rust,
            content_hash: "b1".to_owned(),
            imports: Vec::new(),
            exports: vec![seshat_core::Export {
                name: "binary_export".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &blob_ir);
        let consumer_ir = ProjectFile {
            path: std::path::PathBuf::from("src/consumer.rs"),
            language: seshat_core::Language::Rust,
            content_hash: "c1".to_owned(),
            imports: vec![seshat_core::Import {
                module: "crate::blob".to_owned(),
                names: vec!["binary_export".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &consumer_ir);

        // Modify the binary file on disk so the diff is non-empty.
        let modified: Vec<u8> = b"// header\n\x00binary-fixture-bytes-CHANGED\n".to_vec();
        fs::write(repo.join("src/blob.rs"), &modified).expect("modify binary");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        // Binary blob → read_blob_pair returns None → fall back to Hunk::ALL,
        // which intersects every symbol in the file.
        let sym = result
            .affected_symbols
            .iter()
            .find(|s| s.name == "binary_export")
            .expect("binary_export must be flagged via Hunk::ALL fallback");
        // Hunk::ALL clamped to [1, 1] is just (1, 1).
        assert_eq!(sym.changed_lines, vec![(1, 1)]);
    }

    #[test]
    fn transitive_dependent_count_uses_depth_3() {
        // Build a 3-level chain: utils ← handler ← main ← app
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create dir");
        init_git_repo(&repo);

        fs::create_dir_all(repo.join("src")).expect("create src");
        fs::write(
            repo.join("src/utils.ts"),
            "export function formatDate() {\n  return 'now';\n}\n",
        )
        .expect("write utils");
        fs::write(
            repo.join("src/handler.ts"),
            "import { formatDate } from './utils';\nexport function handle() { return formatDate(); }\n",
        )
        .expect("write handler");
        fs::write(
            repo.join("src/main.ts"),
            "import { handle } from './handler';\nexport function start() { return handle(); }\n",
        )
        .expect("write main");
        fs::write(
            repo.join("src/app.ts"),
            "import { start } from './main';\nstart();\n",
        )
        .expect("write app");
        git_commit_all(&repo, "initial");

        let conn = crate::test_helpers::test_conn();

        let utils_ir = ProjectFile {
            path: std::path::PathBuf::from("src/utils.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "u1".to_owned(),
            imports: Vec::new(),
            exports: vec![seshat_core::Export {
                name: "formatDate".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 3,
            }],
            functions: vec![seshat_core::Function {
                name: "formatDate".to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                end_line: 3,
                parameters: Vec::new(),
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &utils_ir);

        let handler_ir = ProjectFile {
            path: std::path::PathBuf::from("src/handler.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "h1".to_owned(),
            imports: vec![seshat_core::Import {
                module: "./utils".to_owned(),
                names: vec!["formatDate".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: vec![seshat_core::Export {
                name: "handle".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 2,
                end_line: 2,
            }],
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &handler_ir);

        let main_ir = ProjectFile {
            path: std::path::PathBuf::from("src/main.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "m1".to_owned(),
            imports: vec![seshat_core::Import {
                module: "./handler".to_owned(),
                names: vec!["handle".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: vec![seshat_core::Export {
                name: "start".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 2,
                end_line: 2,
            }],
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &main_ir);

        let app_ir = ProjectFile {
            path: std::path::PathBuf::from("src/app.ts"),
            language: seshat_core::Language::TypeScript,
            content_hash: "a1".to_owned(),
            imports: vec![seshat_core::Import {
                module: "./main".to_owned(),
                names: vec!["start".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: seshat_core::LanguageIR::Rust(seshat_core::RustIR::default()),
            file_doc: None,
        };
        crate::test_helpers::insert_ir(&conn, "main", &app_ir);

        // Modify utils.ts to trigger the diff.
        fs::write(
            repo.join("src/utils.ts"),
            "export function formatDate() {\n  return 'today';\n}\n",
        )
        .expect("modify utils");

        let request = DiffImpactRequest {
            staged_only: false,
            base: None,
            repo_path: repo.to_string_lossy().to_string(),
        };

        let result = map_diff_impact(&conn, "main", &repo, &request).expect("map_diff_impact");

        let sym = result
            .affected_symbols
            .iter()
            .find(|s| s.name == "formatDate")
            .expect("formatDate must be flagged");

        // direct importers of formatDate: only handler.ts.
        assert_eq!(
            sym.direct_dependent_count, 1,
            "direct_dependent_count must equal the count of files that import formatDate by name"
        );
        // Total transitive (depth=3 BFS): handler (d=1) + main (d=2) + app (d=3) = 3.
        assert_eq!(
            sym.dependent_count, 3,
            "dependent_count must include 2nd- and 3rd-order dependents up to DEFAULT_TRANSITIVE_DEPTH"
        );
        // blast_radius classified from the transitive total (3 ⇒ Medium).
        assert_eq!(sym.blast_radius, BlastRadius::Medium);
    }
}
