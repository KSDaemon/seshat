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
use crate::dependencies::query_dependencies_batch;
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

/// Compute affected symbols from changed files.
///
/// For each changed file, extracts its exports and public functions from
/// the IR, then queries dependencies in batch. Files that are deleted,
/// untracked, or conflicted are excluded.
pub fn compute_affected_symbols(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    changed_files: &[ChangedFile],
) -> Result<Vec<AffectedSymbol>, GraphError> {
    let analyzable: Vec<&ChangedFile> = changed_files
        .iter()
        .filter(|c| {
            !matches!(
                c.status,
                FileStatus::Deleted | FileStatus::Untracked | FileStatus::Conflicted
            )
        })
        .collect();

    if analyzable.is_empty() {
        return Ok(Vec::new());
    }

    let files = load_branch_ir(conn, branch_id)?;

    let path_strs: Vec<String> = analyzable.iter().map(|c| c.path.clone()).collect();
    let dep_results = query_dependencies_batch(conn, branch_id, &path_strs)?;

    let dep_map: HashMap<&str, &crate::dependencies::DependencyData> =
        dep_results.iter().map(|d| (d.target.as_str(), d)).collect();

    let mut symbols = Vec::new();

    for changed in &analyzable {
        let file = files.iter().find(|f| {
            let stored = f.path.to_string_lossy().to_string();
            stored.ends_with(&changed.path) || stored == changed.path
        });

        let Some(file) = file else {
            continue;
        };

        let dep_info = dep_map.get(file.path.to_string_lossy().as_ref());

        for export in &file.exports {
            let dependent_count = dep_info.map(|d| d.dependents.len()).unwrap_or(0);
            let dependents = dep_info
                .map(|d| {
                    d.dependents
                        .iter()
                        .map(|dep| DependentRef {
                            file: dep.file_path.clone(),
                            line: dep.line,
                        })
                        .take(5)
                        .collect()
                })
                .unwrap_or_default();
            let blast_radius = classify_blast_radius(dependent_count);

            symbols.push(AffectedSymbol {
                name: export.name.clone(),
                file: file.path.to_string_lossy().to_string(),
                kind: "export".to_owned(),
                dependent_count,
                dependents,
                blast_radius,
            });
        }

        for func in file.functions.iter().filter(|f| f.is_public) {
            let dependent_count = dep_info.map(|d| d.dependents.len()).unwrap_or(0);
            let dependents = dep_info
                .map(|d| {
                    d.dependents
                        .iter()
                        .map(|dep| DependentRef {
                            file: dep.file_path.clone(),
                            line: dep.line,
                        })
                        .take(5)
                        .collect()
                })
                .unwrap_or_default();
            let blast_radius = classify_blast_radius(dependent_count);

            symbols.push(AffectedSymbol {
                name: func.name.clone(),
                file: file.path.to_string_lossy().to_string(),
                kind: "function".to_owned(),
                dependent_count,
                dependents,
                blast_radius,
            });
        }
    }

    Ok(symbols)
}

/// Classify blast radius based on number of dependents (used by
/// both dependents analysis and affected symbols).
fn classify_blast_radius(count: usize) -> String {
    if count > 10 {
        "high".to_owned()
    } else if count >= 3 {
        "medium".to_owned()
    } else {
        "low".to_owned()
    }
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
                      je.value ->> '$.file' AS evidence_file
               FROM nodes n,
                    json_each(json_extract(n.ext_data, '$.evidence')) AS je
               WHERE n.branch_id = ?1
                 AND COALESCE(json_extract(n.ext_data, '$.removed'), 0) NOT IN (1, 'true')
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
            Ok((
                description,
                confidence,
                adoption_count_i64,
                total_count_i64,
                weight,
                evidence_file,
            ))
        })
        .map_err(GraphError::query)?;

    let mut seen: HashSet<(String, String)> = HashSet::new();

    for row in rows {
        let (description, confidence, adoption_count_i64, total_count_i64, _weight, evidence_file) =
            row.map_err(GraphError::query)?;
        let adoption_count = adoption_count_i64 as usize;
        let total_count = total_count_i64 as usize;

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

        let note = if changed.status == FileStatus::Deleted {
            format!(
                "{} was evidence for the {} convention. After deletion, the convention confidence may decrease.",
                changed.path, description
            )
        } else if is_golden {
            format!(
                "{} is a golden file for this convention — it has the highest compliance score in the project. If you intentionally evolve this pattern, consider calling record_decision afterwards to update the convention baseline.",
                changed.path
            )
        } else {
            format!(
                "{} contributes evidence to the {} convention ({}% confidence, {}/{} files follow). Changing this file may reduce its convention compliance.",
                changed.path, description, confidence_pct, adoption_count, total_count
            )
        };

        seen.insert(key);

        risks.push(ConventionRisk {
            description,
            affected_file: changed.path.clone(),
            confidence_pct,
            adoption_count,
            total_count,
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
    let changed_files = get_changed_files(repo_path, request.staged_only, request.base.as_deref())?;

    let affected_symbols = compute_affected_symbols(conn, branch_id, &changed_files)?;
    let convention_risks = compute_convention_risks(conn, branch_id, &changed_files)?;

    let total_dependents: usize = affected_symbols.iter().map(|s| s.dependent_count).sum();
    let total_affected_symbols = affected_symbols.len();
    let total_changed_files = changed_files.len();

    let risk = compute_overall_risk(&affected_symbols);

    let blast_radius_summary = BlastRadiusSummary {
        total_dependents,
        total_affected_symbols,
        total_changed_files,
        risk,
    };

    let next_steps = generate_next_steps(&changed_files, &affected_symbols, &convention_risks);
    let branch = branch_id.to_owned();

    let metadata = ImpactMetadata { next_steps, branch };

    Ok(DiffImpactData {
        changed_files,
        affected_symbols,
        convention_risks,
        blast_radius_summary,
        metadata,
    })
}

/// Compute overall risk level from the max blast radius among affected symbols.
fn compute_overall_risk(affected_symbols: &[AffectedSymbol]) -> String {
    if affected_symbols.is_empty() {
        return "none".to_owned();
    }

    let has_high = affected_symbols.iter().any(|s| s.blast_radius == "high");
    let has_medium = affected_symbols.iter().any(|s| s.blast_radius == "medium");

    if has_high {
        "high".to_owned()
    } else if has_medium {
        "medium".to_owned()
    } else {
        "low".to_owned()
    }
}

/// Generate actionable next steps based on analysis results.
fn generate_next_steps(
    changed_files: &[ChangedFile],
    affected_symbols: &[AffectedSymbol],
    convention_risks: &[ConventionRisk],
) -> Vec<String> {
    let mut steps = Vec::new();

    if changed_files.is_empty() {
        steps.push("Nothing to review — no uncommitted changes detected".to_owned());
        return steps;
    }

    let high_impact: Vec<&AffectedSymbol> = affected_symbols
        .iter()
        .filter(|s| s.dependent_count >= 3)
        .collect();
    if !high_impact.is_empty() {
        let names: Vec<&str> = high_impact.iter().map(|s| s.name.as_str()).collect();
        steps.push(format!(
            "Review affected symbols with >= 3 dependents (potential blast radius): {}",
            names.join(", ")
        ));
    }

    if convention_risks.iter().any(|r| r.is_golden_file) {
        steps.push(
            "A modified file is a golden file for a convention — if you intentionally evolved \
             the pattern, consider calling record_decision to update the convention baseline"
                .to_owned(),
        );
    }

    let has_deleted = changed_files
        .iter()
        .any(|c| c.status == FileStatus::Deleted);
    if has_deleted {
        steps.push(
            "Verify that deleted files do not break any dependents or convention evidence"
                .to_owned(),
        );
    }

    if !affected_symbols.is_empty() {
        steps.push(
            "Run the project test suite to catch regressions introduced by the changes".to_owned(),
        );
    }

    steps.push("Consider calling validate_approach to verify convention compliance".to_owned());

    steps
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
        assert_eq!(result.blast_radius_summary.risk, "none");
        assert_eq!(result.blast_radius_summary.total_changed_files, 0);
        assert_eq!(result.blast_radius_summary.total_affected_symbols, 0);
        assert!(
            result
                .metadata
                .next_steps
                .iter()
                .any(|s| s.contains("nothing to review") || s.contains("Nothing to review"))
        );
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
        assert_eq!(result.blast_radius_summary.risk, "none");
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
        assert_eq!(utils_sym.blast_radius, "medium");
        assert_eq!(result.blast_radius_summary.risk, "medium");
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
}
