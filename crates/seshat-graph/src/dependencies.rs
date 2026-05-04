//! Dependency analysis over deserialized IR.
//!
//! Provides `query_dependencies()` which builds a dependency index from IR
//! and returns direct dependents/dependencies with blast radius for any file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;

use seshat_storage::{RepoMetadataRepository, SqliteRepoMetadataRepository};

use crate::code_pattern::load_branch_ir;
use crate::error::GraphError;

// ── Constants ────────────────────────────────────────────────

/// Blast radius thresholds.
const BLAST_RADIUS_MEDIUM_THRESHOLD: usize = 3;
const BLAST_RADIUS_HIGH_THRESHOLD: usize = 10;

// ── Blast radius enum ────────────────────────────────────────

/// Classification of change impact based on number of dependents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlastRadius {
    /// No changes or no affected symbols.
    None,
    /// Fewer than 3 dependents.
    Low,
    /// 3–10 dependents.
    Medium,
    /// More than 10 dependents.
    High,
}

impl std::fmt::Display for BlastRadius {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

// ── Response data types ──────────────────────────────────────

/// Full response data for the `query_dependencies` tool.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyData {
    /// The target file path that was queried.
    pub target: String,
    /// Files that the target imports from.
    pub dependencies: Vec<DependencyEntry>,
    /// Files that import from the target.
    pub dependents: Vec<DependentEntry>,
    /// External dependencies used by the target file.
    pub external_dependencies: Vec<ExternalDependency>,
    /// Blast radius classification.
    pub blast_radius: BlastRadius,
    /// Backward compatibility note, present when dependents exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backward_compatibility_note: Option<String>,
    /// Whether IR loading was truncated (LIMIT reached), meaning results
    /// may be incomplete for very large repositories.
    #[serde(default)]
    pub truncated: bool,
}

/// A file that the target imports from (a dependency).
#[derive(Debug, Clone, Serialize)]
pub struct DependencyEntry {
    /// Path of the dependency file.
    pub file_path: String,
    /// Names imported from this dependency.
    pub import_names: Vec<String>,
    /// Whether the import could be resolved to a known file in the IR.
    pub resolved: bool,
}

/// A file that imports from the target (a dependent).
#[derive(Debug, Clone, Serialize)]
pub struct DependentEntry {
    /// Path of the dependent file.
    pub file_path: String,
    /// Names that this file imports from the target.
    pub import_names: Vec<String>,
    /// Line number of the import statement.
    pub line: usize,
}

/// An external dependency used by the target file.
#[derive(Debug, Clone, Serialize)]
pub struct ExternalDependency {
    /// Package name.
    pub package: String,
    /// Import path.
    pub import_path: String,
    /// Line number of the usage.
    pub line: usize,
}

// ── Suffix Index ─────────────────────────────────────────────

/// Reverse suffix index for O(1) import resolution.
///
/// Maps path suffixes (e.g. `models/user.rs`) to their full known paths,
/// replacing the O(N×E) linear scan in `resolve_by_suffix` with a single
/// hash-table lookup.
#[derive(Debug, Clone)]
struct SuffixIndex {
    map: HashMap<String, String>,
}

impl SuffixIndex {
    /// Build a suffix index from known file paths.
    ///
    /// For each known path, all suffixes of increasing depth are inserted
    /// (e.g. for `src/models/user.ts`: `user.ts`, `models/user.ts`, `src/models/user.ts`).
    /// When multiple paths share the same suffix, the first insertion wins.
    fn build(known_paths: &HashSet<String>) -> Self {
        let mut map = HashMap::new();
        let mut sorted: Vec<&String> = known_paths.iter().collect();
        sorted.sort();
        for path in sorted {
            let normalized = path.replace('\\', "/");
            let parts: Vec<&str> = normalized.split('/').collect();
            for i in 0..parts.len() {
                let suffix = parts[i..].join("/");
                map.entry(suffix).or_insert_with(|| path.clone());
            }
        }
        SuffixIndex { map }
    }

    /// Resolve a module path (e.g. `crate::models::user`) to a known file path.
    ///
    /// Converts the module path to a file-system suffix, then looks it up in the
    /// index. Also tries common file extensions (`.rs`, `.ts`, `.py`, etc.) when
    /// the bare suffix is not found.
    fn resolve(&self, module: &str) -> Option<String> {
        let suffix = module_to_path_suffix(module);

        if let Some(resolved) = self.map.get(&suffix) {
            return Some(resolved.clone());
        }

        for ext in FILE_EXTENSIONS {
            let suffix_ext = format!("{suffix}{ext}");
            if let Some(resolved) = self.map.get(&suffix_ext) {
                return Some(resolved.clone());
            }
        }

        None
    }
}

// ── Public API ───────────────────────────────────────────────

/// Build a dependency index from IR and return dependencies, dependents,
/// and blast radius for the given target file.
///
/// Returns `Err(GraphError::NodeNotFound)` if the target path is not in the IR.
pub fn query_dependencies(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    target_path: &str,
) -> Result<DependencyData, GraphError> {
    let trimmed = target_path.trim();
    if trimmed.is_empty() {
        return Err(GraphError::InvalidInput(
            "target path must not be empty".to_owned(),
        ));
    }

    // Load all IR for this branch.
    let loaded_ir = load_branch_ir(conn, branch_id)?;
    let files = &loaded_ir.files;
    let truncated = loaded_ir.truncated;

    // Load internal crate/package names from the database.
    let internal_names = load_internal_names(conn, branch_id);

    // Build a set of known file paths for resolution.
    let known_paths: HashSet<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    // Build the suffix index for O(1) import resolution.
    let suffix_index = SuffixIndex::build(&known_paths);

    // Verify the target file exists in IR.
    // IR stores absolute paths; the caller supplies a relative path.
    // Try exact match first (fast path), then fall back to suffix match so that
    // "crates/seshat-core/src/ir.rs" matches the stored
    // "/Users/kostik/Projects/seshat/crates/seshat-core/src/ir.rs".
    // `suffix_matches_at_boundary` is already used by `build_dependents` for the
    // same reason — we just extend the same tolerance to the target lookup.
    let target_normalized = normalize_path(trimmed);
    let target_file = files.iter().find(|f| {
        let stored = normalize_path(&f.path.to_string_lossy());
        stored == target_normalized || suffix_matches_at_boundary(&stored, &target_normalized)
    });

    let Some(target_file) = target_file else {
        return Err(GraphError::NodeNotFound(format!(
            "File not found in IR: {trimmed}"
        )));
    };
    let target_path_str = target_file.path.to_string_lossy().to_string();

    // Build dependencies: files the target imports from.
    let dependencies =
        build_dependencies(target_file, &known_paths, &suffix_index, &internal_names);

    // Build dependents: files that import from the target.
    let dependents = build_dependents(&target_path_str, files, &internal_names);

    // External dependencies from dependencies_used.
    let external_dependencies: Vec<ExternalDependency> = target_file
        .dependencies_used
        .iter()
        .map(|d| ExternalDependency {
            package: d.package.clone(),
            import_path: d.import_path.clone(),
            line: d.line,
        })
        .collect();

    // Blast radius classification.
    let blast_radius = classify_blast_radius(dependents.len());

    // Backward compatibility note.
    let backward_compatibility_note = if !dependents.is_empty() {
        Some(format!(
            "This file has {} direct dependent(s). Changes to its public API may require updates in those files.",
            dependents.len()
        ))
    } else {
        None
    };

    Ok(DependencyData {
        target: target_path_str,
        dependencies,
        dependents,
        external_dependencies,
        blast_radius,
        backward_compatibility_note,
        truncated,
    })
}

/// Batch query dependencies for multiple files with a single IR load.
///
/// Loads IR once and builds a dependents index, then computes
/// `DependencyData` for every requested path. This is O(N) instead
/// of N x O(IR_load) — much faster when checking many changed files.
pub fn query_dependencies_batch(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    paths: &[String],
) -> Result<Vec<DependencyData>, GraphError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let loaded_ir = load_branch_ir(conn, branch_id)?;
    let files = &loaded_ir.files;
    let truncated = loaded_ir.truncated;

    let known_paths: HashSet<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    let suffix_index = SuffixIndex::build(&known_paths);

    let mut results = Vec::with_capacity(paths.len());

    for target_path in paths {
        let trimmed = target_path.trim();
        if trimmed.is_empty() {
            continue;
        }

        let target_normalized = normalize_path(trimmed);
        let target_file = files.iter().find(|f| {
            let stored = normalize_path(&f.path.to_string_lossy());
            stored == target_normalized || suffix_matches_at_boundary(&stored, &target_normalized)
        });

        let Some(target_file) = target_file else {
            continue;
        };
        let target_path_str = target_file.path.to_string_lossy().to_string();

        let dependencies = build_dependencies(target_file, &known_paths, &suffix_index);
        let dependents = build_dependents(&target_path_str, files);

        let external_dependencies: Vec<ExternalDependency> = target_file
            .dependencies_used
            .iter()
            .map(|d| ExternalDependency {
                package: d.package.clone(),
                import_path: d.import_path.clone(),
                line: d.line,
            })
            .collect();

        let blast_radius = classify_blast_radius(dependents.len());

        let backward_compatibility_note = if !dependents.is_empty() {
            Some(format!(
                "This file has {} direct dependent(s). Changes to its public API may require updates in those files.",
                dependents.len()
            ))
        } else {
            None
        };

        results.push(DependencyData {
            target: target_path_str,
            dependencies,
            dependents,
            external_dependencies,
            blast_radius,
            backward_compatibility_note,
            truncated,
        });
    }

    Ok(results)
}

// ── Internal helpers ─────────────────────────────────────────

/// Common file extensions to try when resolving import paths.
const FILE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".rs", ".py"];

/// Index/module files to try when an import resolves to a directory.
const INDEX_FILES: &[&str] = &["/index.ts", "/index.js", "/mod.rs"];

/// Load workspace-internal package/crate names from the database.
///
/// Reads the `workspace_crates` key from `repo_metadata` and deserializes the
/// stored JSON array into a `Vec<String>`.  Returns an empty `Vec` when the
/// key is absent or the stored value is not valid JSON.
pub fn load_internal_names(conn: &Arc<Mutex<Connection>>, branch_id: &str) -> Vec<String> {
    let repo = SqliteRepoMetadataRepository::new(Arc::clone(conn));
    // The key is stored per-branch by the scanner.  For now, metadata is
    // keyed globally (not per-branch), so we use a single well-known key.
    // Future: prefix with branch_id if multi-branch metadata is needed.
    let _ = branch_id; // reserved for future per-branch scoping
    match repo.get("workspace_crates") {
        Ok(Some(json)) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
        Ok(None) => Vec::new(),
        Err(_) => Vec::new(),
    }
}

/// Convert a module path (e.g. `crate::foo::bar`) to a path suffix (`foo/bar`).
///
/// Replaces `::` and `.` separators with `/`, then strips leading `crate/`,
/// `super/`, `self/`, or workspace-crate prefixes.
fn module_to_path_suffix(module: &str) -> String {
    let path_part = module.replace("::", "/").replace('.', "/");
    let stripped = path_part
        .strip_prefix("crate/")
        .or_else(|| path_part.strip_prefix("super/"))
        .or_else(|| path_part.strip_prefix("self/"))
        .unwrap_or(&path_part);

    stripped.to_owned()
}

/// Check if `haystack` ends with `suffix` at a path component boundary
/// (preceded by `/` or the suffix is the entire string).
///
/// Returns `false` for an empty `suffix` — an empty string would otherwise
/// match every haystack via `str::strip_suffix("")`, producing bogus results.
pub(crate) fn suffix_matches_at_boundary(haystack: &str, suffix: &str) -> bool {
    if suffix.is_empty() {
        return false;
    }
    if haystack == suffix {
        return true;
    }
    match haystack.strip_suffix(suffix) {
        Some(before) => before.ends_with('/'),
        None => false,
    }
}

/// Normalize a path string for comparison (remove leading ./ and trailing /).
fn normalize_path(path: &str) -> String {
    let p = path.strip_prefix("./").unwrap_or(path);
    let p = p.strip_suffix('/').unwrap_or(p);
    p.to_string()
}

/// Build the list of files that the target imports from.
fn build_dependencies(
    target_file: &seshat_core::ProjectFile,
    known_paths: &HashSet<String>,
    suffix_index: &SuffixIndex,
    internal_names: &[String],
) -> Vec<DependencyEntry> {
    let target_dir = Path::new(&target_file.path)
        .parent()
        .unwrap_or_else(|| Path::new(""));

    let mut deps: HashMap<String, DependencyEntry> = HashMap::new();

    for import in &target_file.imports {
        let resolved_path = resolve_import(
            &import.module,
            target_dir,
            known_paths,
            suffix_index,
            internal_names,
        );

        match resolved_path {
            Some(resolved) => {
                let entry = deps
                    .entry(resolved.clone())
                    .or_insert_with(|| DependencyEntry {
                        file_path: resolved,
                        import_names: Vec::new(),
                        resolved: true,
                    });
                for name in &import.names {
                    if !entry.import_names.contains(name) {
                        entry.import_names.push(name.clone());
                    }
                }
            }
            None => {
                // Could not resolve — check if this is an external import
                // (doesn't start with . or crate:: or similar).
                if is_likely_internal(&import.module, internal_names) {
                    let key = import.module.clone();
                    let entry = deps.entry(key.clone()).or_insert_with(|| DependencyEntry {
                        file_path: key,
                        import_names: Vec::new(),
                        resolved: false,
                    });
                    for name in &import.names {
                        if !entry.import_names.contains(name) {
                            entry.import_names.push(name.clone());
                        }
                    }
                }
                // External imports are excluded from dependencies list.
            }
        }
    }

    let mut result: Vec<DependencyEntry> = deps.into_values().collect();
    result.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    result
}

/// Check if an import module path looks like an internal import.
///
/// The `internal_names` slice contains workspace-crate / package names loaded
/// from the database at query time (see `load_internal_names`).  Callers that
/// have not yet loaded names may pass `&[]`, in which case cross-crate imports
/// will not be classified as internal.
fn is_likely_internal(module: &str, internal_names: &[String]) -> bool {
    module.starts_with('.') // covers ./ and ../
        || module == "crate" || module.starts_with("crate::")
        || module == "super" || module.starts_with("super::")
        || module == "self" || module.starts_with("self::")
        || module.starts_with("src/")
        || module.starts_with("src.")
        || is_internal_crate(module, internal_names)
}

/// Extract the first segment of a module path (before `::` or `.`).
fn first_module_segment(module: &str) -> &str {
    module
        .split("::")
        .next()
        .unwrap_or(module)
        .split('.')
        .next()
        .unwrap_or(module)
}

/// Check if the first segment of a module path is a known internal crate/package.
fn is_internal_crate(module: &str, internal_names: &[String]) -> bool {
    let first = first_module_segment(module);
    internal_names.iter().any(|n| n == first)
}

/// Resolve an import module path to a known file path.
///
/// - Relative imports (starting with `.` or `..`) are resolved against the
///   importing file's directory.
/// - Workspace crate imports (e.g. `seshat_graph::validate_approach`) strip
///   the crate prefix and resolve the rest via the suffix index.
/// - Absolute-looking internal imports (starting with `crate`, `self`, `super`)
///   are matched by suffix against known paths.
/// - External imports return None.
fn resolve_import(
    module: &str,
    importing_dir: &Path,
    known_paths: &HashSet<String>,
    suffix_index: &SuffixIndex,
    internal_names: &[String],
) -> Option<String> {
    if module.starts_with('.') {
        // Relative import — resolve against importing directory.
        resolve_relative_import(module, importing_dir, known_paths)
    } else if is_internal_crate(module, internal_names) {
        // Internal crate import — strip crate prefix, resolve rest via suffix.
        resolve_internal_crate_import(module, suffix_index)
    } else if module.starts_with("crate")
        || module.starts_with("super")
        || module.starts_with("self")
    {
        // Rust-style internal import — match by suffix.
        resolve_by_suffix(module, suffix_index)
    } else if module.starts_with("src/") || module.starts_with("src.") {
        // Python-style absolute internal import.
        resolve_by_suffix(module, suffix_index)
    } else {
        // External import — exclude.
        None
    }
}

/// Resolve an internal crate import by stripping the crate prefix and
/// matching the remaining module path against the suffix index.
///
/// For example, `seshat_graph::validate_approach` strips `seshat_graph`
/// and resolves `validate_approach` as a path suffix.
/// Also handles Python dot-separated paths: `my_package.utils` strips
/// `my_package` and resolves `utils` via the suffix index.
fn resolve_internal_crate_import(module: &str, suffix_index: &SuffixIndex) -> Option<String> {
    let first = first_module_segment(module);
    let after = &module[first.len()..];
    let rest = after
        .strip_prefix("::")
        .or_else(|| after.strip_prefix('.'))
        .unwrap_or(after);
    if rest.is_empty() {
        return None;
    }
    suffix_index.resolve(rest)
}

/// Resolve a relative import (e.g., `./utils`, `../models/user`).
fn resolve_relative_import(
    module: &str,
    importing_dir: &Path,
    known_paths: &HashSet<String>,
) -> Option<String> {
    let joined = importing_dir.join(module);
    let normalized = normalize_pathbuf(&joined);
    let normalized_str = normalized.to_string_lossy().to_string();

    // Try exact match first.
    if known_paths.contains(&normalized_str) {
        return Some(normalized_str);
    }

    // Try common extensions, then index/module files.
    for ext in FILE_EXTENSIONS.iter().chain(INDEX_FILES.iter()) {
        let with_ext = format!("{normalized_str}{ext}");
        if known_paths.contains(&with_ext) {
            return Some(with_ext);
        }
    }

    None
}

/// Resolve an import by matching its module path against the suffix index.
fn resolve_by_suffix(module: &str, suffix_index: &SuffixIndex) -> Option<String> {
    suffix_index.resolve(module)
}

/// Normalize a PathBuf (resolve `.` and `..` components without filesystem access).
fn normalize_pathbuf(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {} // Skip `.`
            std::path::Component::ParentDir => {
                components.pop(); // Go up for `..`
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Build the list of files that import from the target.
fn build_dependents(
    target_path: &str,
    files: &[seshat_core::ProjectFile],
    internal_names: &[String],
) -> Vec<DependentEntry> {
    let target_normalized = normalize_path(target_path);
    let target_name_no_ext = Path::new(target_path)
        .with_extension("")
        .to_string_lossy()
        .to_string();

    let mut dependents = Vec::new();

    for file in files {
        let file_path = file.path.to_string_lossy().to_string();
        if normalize_path(&file_path) == target_normalized {
            continue; // Skip self-references.
        }

        let file_dir = Path::new(&file.path)
            .parent()
            .unwrap_or_else(|| Path::new(""));

        let mut import_names: Vec<String> = Vec::new();
        let mut first_line: Option<usize> = None;

        for import in &file.imports {
            if import_resolves_to_target(
                &import.module,
                file_dir,
                &target_normalized,
                &target_name_no_ext,
                internal_names,
            ) {
                if first_line.is_none() {
                    first_line = Some(import.line);
                }
                for name in &import.names {
                    if !import_names.contains(name) {
                        import_names.push(name.clone());
                    }
                }
            }
        }

        if let Some(line) = first_line {
            dependents.push(DependentEntry {
                file_path: file_path.clone(),
                import_names,
                line,
            });
        }
    }

    dependents.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    dependents
}

/// Check if an import module resolves to the target file.
fn import_resolves_to_target(
    module: &str,
    importing_dir: &Path,
    target_normalized: &str,
    target_name_no_ext: &str,
    internal_names: &[String],
) -> bool {
    if module.starts_with('.') {
        // Relative import.
        let joined = importing_dir.join(module);
        let normalized = normalize_pathbuf(&joined);
        let normalized_str = normalize_path(&normalized.to_string_lossy());

        // Exact match or match with extension stripped.
        if normalized_str == *target_normalized {
            return true;
        }

        // Try: import resolves to target without extension.
        let target_no_ext = normalize_path(target_name_no_ext);
        if normalized_str == target_no_ext {
            return true;
        }

        // Try: import + common extensions matches target.
        for ext in FILE_EXTENSIONS {
            if format!("{normalized_str}{ext}") == *target_normalized {
                return true;
            }
        }

        // Try: import/index matches target.
        for index in INDEX_FILES {
            if format!("{normalized_str}{index}") == *target_normalized {
                return true;
            }
        }

        false
    } else if is_internal_crate(module, internal_names) {
        // Internal crate/package import — strip the package prefix, then check
        // if the remaining suffix matches the target path.
        let first = first_module_segment(module);
        let after = &module[first.len()..];
        let rest = after
            .strip_prefix("::")
            .or_else(|| after.strip_prefix('.'))
            .unwrap_or(after);
        if rest.is_empty() {
            return false;
        }
        let suffix = module_to_path_suffix(rest);
        suffix_matches_at_boundary(target_normalized, &suffix)
            || suffix_matches_at_boundary(target_name_no_ext, &suffix)
            || target_stem == suffix
    } else if is_likely_internal(module, internal_names) {
        // Absolute-style internal import (crate::, super::, self::, src.) —
        // check suffix match at path boundary.
        let suffix = module_to_path_suffix(module);

        // `crate::` and `self::` are same-crate-only keywords in Rust — they
        // can never refer to a file in a different crate.  Guard: the importing
        // file and the target must share the same inferred package root before
        // we allow the suffix match.  This prevents `use crate::error` in
        // crate A from falsely resolving to `error.rs` in crate B.
        //
        // `super::` is also a same-crate construct but is already handled by
        // relative path resolution (it resolves to the parent module directory),
        // so we include it in the guard as well.
        let is_same_package_keyword = module.starts_with("crate")
            || module.starts_with("self")
            || module.starts_with("super");

        if is_same_package_keyword {
            let importing_root = infer_package_root(importing_dir);
            let target_root = infer_package_root(Path::new(target_normalized));
            if importing_root != target_root {
                return false;
            }
        }

        // `target_stem == suffix` is intentionally omitted: it is a strict
        // subset of `suffix_matches_at_boundary(target_name_no_ext, suffix)`
        // (which already handles single-segment names) and was the source of
        // false positives (e.g. suffix "error" matching every error.rs).
        suffix_matches_at_boundary(target_normalized, &suffix)
            || suffix_matches_at_boundary(target_name_no_ext, &suffix)
    } else {
        false
    }
}

/// Infer the "package root" of a file from its filesystem path.
///
/// Walks up the directory tree looking for a component named `src`.  When
/// found, returns its parent — e.g.:
///   `/proj/crates/seshat-graph/src/error.rs` → `/proj/crates/seshat-graph`
///
/// If no `src` ancestor exists the file's own directory is returned as a
/// conservative fallback.  This works for any project that follows the
/// conventional `<package-root>/src/` layout (Rust/Cargo, Python src-layout,
/// Node/TypeScript src/ layout, etc.).
///
/// Note: this function is only called for `crate::` / `self::` / `super::`
/// imports which are Rust-specific constructs, so the `src/` convention is
/// always correct in practice.
fn infer_package_root(path: &Path) -> PathBuf {
    // Start from the file's directory (or the path itself if it is a dir).
    let start = if path.extension().is_some() {
        path.parent().unwrap_or(path)
    } else {
        path
    };

    let mut current = start;
    loop {
        if current.file_name().and_then(|n| n.to_str()) == Some("src") {
            return current.parent().unwrap_or(current).to_path_buf();
        }
        match current.parent() {
            Some(p) if p != current => current = p,
            _ => break,
        }
    }

    // Fallback: return the starting directory.
    start.to_path_buf()
}

/// Classify blast radius based on number of dependents.
pub(crate) fn classify_blast_radius(count: usize) -> BlastRadius {
    if count > BLAST_RADIUS_HIGH_THRESHOLD {
        BlastRadius::High
    } else if count >= BLAST_RADIUS_MEDIUM_THRESHOLD {
        BlastRadius::Medium
    } else {
        BlastRadius::Low
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use seshat_core::{
        DependencyUsage, Export, Function, Import, Language, LanguageIR, ProjectFile, RustIR,
        TypeDef, TypeDefKind,
    };

    use crate::test_helpers::{insert_ir, test_conn};

    /// Create a file that imports from other modules.
    fn make_file(
        path: &str,
        imports: Vec<Import>,
        exports: Vec<Export>,
        deps_used: Vec<DependencyUsage>,
    ) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            content_hash: format!("hash_{path}"),
            imports,
            exports,
            functions: vec![Function {
                name: "main".to_owned(),
                is_public: true,
                is_async: false,
                line: 1,
                doc_comment: None,
                end_line: 10,
                parameters: Vec::new(),
            }],
            types: vec![TypeDef {
                name: "Config".to_owned(),
                kind: TypeDefKind::Interface,
                is_public: true,
                line: 12,
                doc_comment: None,
            }],
            dependencies_used: deps_used,
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    /// Set up a small project with known import relationships:
    ///
    /// src/utils.ts — no imports
    /// src/models/user.ts — imports from ../utils
    /// src/services/user_service.ts — imports from ../models/user and ../utils
    /// src/app.ts — imports from ./services/user_service
    fn setup_project(conn: &Arc<Mutex<Connection>>) {
        let utils = make_file(
            "src/utils.ts",
            vec![],
            vec![Export {
                name: "formatDate".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            vec![],
        );

        let user_model = make_file(
            "src/models/user.ts",
            vec![Import {
                module: "../utils".to_owned(),
                names: vec!["formatDate".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![Export {
                name: "User".to_owned(),
                is_default: false,
                is_type_only: true,
                line: 5,
            }],
            vec![],
        );

        let user_service = make_file(
            "src/services/user_service.ts",
            vec![
                Import {
                    module: "../models/user".to_owned(),
                    names: vec!["User".to_owned()],
                    is_type_only: true,
                    line: 1,
                },
                Import {
                    module: "../utils".to_owned(),
                    names: vec!["formatDate".to_owned()],
                    is_type_only: false,
                    line: 2,
                },
            ],
            vec![Export {
                name: "UserService".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 10,
            }],
            vec![DependencyUsage {
                package: "express".to_owned(),
                import_path: "express".to_owned(),
                line: 3,
            }],
        );

        let app = make_file(
            "src/app.ts",
            vec![Import {
                module: "./services/user_service".to_owned(),
                names: vec!["UserService".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![],
            vec![],
        );

        insert_ir(conn, "main", &utils);
        insert_ir(conn, "main", &user_model);
        insert_ir(conn, "main", &user_service);
        insert_ir(conn, "main", &app);
    }

    #[test]
    fn file_with_known_imports_returns_dependencies() {
        let conn = test_conn();
        setup_project(&conn);

        let result = query_dependencies(&conn, "main", "src/services/user_service.ts").unwrap();

        assert_eq!(result.target, "src/services/user_service.ts");

        // user_service imports from models/user and utils.
        assert!(
            !result.dependencies.is_empty(),
            "Expected at least 1 dependency, got {}",
            result.dependencies.len()
        );

        // Check that at least one resolved dependency exists.
        let resolved: Vec<_> = result.dependencies.iter().filter(|d| d.resolved).collect();
        assert!(
            !resolved.is_empty(),
            "Expected at least one resolved dependency"
        );
    }

    #[test]
    fn file_imported_by_others_returns_dependents() {
        let conn = test_conn();
        setup_project(&conn);

        // utils.ts is imported by user.ts and user_service.ts.
        let result = query_dependencies(&conn, "main", "src/utils.ts").unwrap();

        assert!(
            result.dependents.len() >= 2,
            "Expected at least 2 dependents for utils.ts, got {}",
            result.dependents.len()
        );

        let dependent_paths: Vec<&str> = result
            .dependents
            .iter()
            .map(|d| d.file_path.as_str())
            .collect();
        assert!(
            dependent_paths.contains(&"src/models/user.ts"),
            "Expected src/models/user.ts as dependent, got: {dependent_paths:?}"
        );
        assert!(
            dependent_paths.contains(&"src/services/user_service.ts"),
            "Expected src/services/user_service.ts as dependent, got: {dependent_paths:?}"
        );
    }

    #[test]
    fn blast_radius_classification() {
        assert_eq!(classify_blast_radius(0), BlastRadius::Low);
        assert_eq!(classify_blast_radius(1), BlastRadius::Low);
        assert_eq!(classify_blast_radius(2), BlastRadius::Low);
        assert_eq!(classify_blast_radius(3), BlastRadius::Medium);
        assert_eq!(classify_blast_radius(10), BlastRadius::Medium);
        assert_eq!(classify_blast_radius(11), BlastRadius::High);
        assert_eq!(classify_blast_radius(100), BlastRadius::High);
    }

    #[test]
    fn blast_radius_from_query() {
        let conn = test_conn();
        setup_project(&conn);

        // utils.ts has 2 dependents → low.
        let result = query_dependencies(&conn, "main", "src/utils.ts").unwrap();
        assert_eq!(result.blast_radius, BlastRadius::Low);
        assert_eq!(result.dependents.len(), 2);

        // app.ts has 0 dependents → low.
        let result = query_dependencies(&conn, "main", "src/app.ts").unwrap();
        assert_eq!(result.blast_radius, BlastRadius::Low);
        assert_eq!(result.dependents.len(), 0);
    }

    #[test]
    fn unresolved_imports_flagged() {
        let conn = test_conn();

        // Create a file that imports from a module not in IR.
        let file = make_file(
            "src/orphan.ts",
            vec![Import {
                module: "./nonexistent_module".to_owned(),
                names: vec!["Something".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![],
            vec![],
        );
        insert_ir(&conn, "main", &file);

        let result = query_dependencies(&conn, "main", "src/orphan.ts").unwrap();

        // The import should appear as unresolved.
        let unresolved: Vec<_> = result.dependencies.iter().filter(|d| !d.resolved).collect();
        assert!(
            !unresolved.is_empty(),
            "Expected unresolved dependency for nonexistent module"
        );
    }

    #[test]
    fn file_not_in_ir_returns_error() {
        let conn = test_conn();
        setup_project(&conn);

        let result = query_dependencies(&conn, "main", "src/nonexistent.ts");
        assert!(result.is_err());

        match result {
            Err(GraphError::NodeNotFound(msg)) => {
                assert!(msg.contains("nonexistent"));
            }
            other => panic!("Expected NodeNotFound, got: {other:?}"),
        }
    }

    #[test]
    fn empty_target_path_returns_error() {
        let conn = test_conn();

        let result = query_dependencies(&conn, "main", "");
        assert!(result.is_err());
        match result {
            Err(GraphError::InvalidInput(msg)) => {
                assert!(msg.contains("empty"));
            }
            other => panic!("Expected InvalidInput, got: {other:?}"),
        }
    }

    #[test]
    fn external_dependencies_returned() {
        let conn = test_conn();
        setup_project(&conn);

        let result = query_dependencies(&conn, "main", "src/services/user_service.ts").unwrap();

        assert_eq!(result.external_dependencies.len(), 1);
        assert_eq!(result.external_dependencies[0].package, "express");
    }

    #[test]
    fn backward_compatibility_note_present_when_dependents_exist() {
        let conn = test_conn();
        setup_project(&conn);

        // utils.ts has dependents.
        let result = query_dependencies(&conn, "main", "src/utils.ts").unwrap();
        assert!(result.backward_compatibility_note.is_some());

        // app.ts has no dependents.
        let result = query_dependencies(&conn, "main", "src/app.ts").unwrap();
        assert!(result.backward_compatibility_note.is_none());
    }

    #[test]
    fn no_self_reference_in_dependents() {
        let conn = test_conn();
        setup_project(&conn);

        let result = query_dependencies(&conn, "main", "src/utils.ts").unwrap();

        // utils.ts should not appear as its own dependent.
        let self_ref = result
            .dependents
            .iter()
            .find(|d| d.file_path == "src/utils.ts");
        assert!(self_ref.is_none(), "File should not be its own dependent");
    }

    #[test]
    fn normalize_path_works() {
        assert_eq!(normalize_path("./src/file.ts"), "src/file.ts");
        assert_eq!(normalize_path("src/file.ts"), "src/file.ts");
        assert_eq!(normalize_path("src/dir/"), "src/dir");
    }

    /// Regression test: IR stores absolute paths (e.g. from WalkBuilder), but
    /// the MCP caller supplies relative paths.  `query_dependencies` must
    /// resolve them via suffix match, not just exact equality after `./`-strip.
    #[test]
    fn query_dependencies_accepts_relative_path_when_ir_has_absolute_paths() {
        let conn = test_conn();
        let branch = "main";

        // Insert files with *absolute* paths, as the scanner produces in production.
        let abs_utils = make_file(
            "/home/user/project/src/utils.ts",
            vec![],
            vec![Export {
                name: "helper".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            vec![],
        );
        let abs_app = make_file(
            "/home/user/project/src/app.ts",
            vec![Import {
                module: "./utils".to_owned(),
                names: vec!["helper".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![],
            vec![],
        );
        insert_ir(&conn, branch, &abs_utils);
        insert_ir(&conn, branch, &abs_app);

        // The caller passes a relative path — must NOT get NODE_NOT_FOUND.
        let result = query_dependencies(&conn, branch, "src/utils.ts");
        assert!(
            result.is_ok(),
            "query_dependencies must accept relative path when IR has absolute paths, \
             got: {result:?}"
        );
        let data = result.unwrap();

        // src/app.ts imports from utils → utils has at least one dependent.
        assert!(
            !data.dependents.is_empty(),
            "src/utils.ts should have dependents (src/app.ts imports it), \
             but got none — path matching is broken"
        );
        assert!(
            data.dependents[0].file_path.contains("app.ts"),
            "dependent should be app.ts, got: {:?}",
            data.dependents[0].file_path
        );
    }

    #[test]
    fn normalize_pathbuf_resolves_dots() {
        let result = normalize_pathbuf(Path::new("src/models/../utils"));
        assert_eq!(result, PathBuf::from("src/utils"));

        let result = normalize_pathbuf(Path::new("src/./utils"));
        assert_eq!(result, PathBuf::from("src/utils"));
    }

    #[test]
    fn suffix_matches_at_boundary_empty_suffix_returns_false() {
        // Regression test for P-3: an empty suffix must never match anything.
        // `str::strip_suffix("")` always returns Some(_), so without the guard
        // every stored path would be returned as a match.
        assert!(
            !suffix_matches_at_boundary("/home/user/project/src/lib.rs", ""),
            "empty suffix must not match any haystack"
        );
        assert!(
            !suffix_matches_at_boundary("", ""),
            "empty suffix must not match empty haystack"
        );
    }

    #[test]
    fn suffix_matches_at_boundary_basic_cases() {
        assert!(suffix_matches_at_boundary(
            "/home/user/project/src/utils.ts",
            "src/utils.ts"
        ));
        assert!(suffix_matches_at_boundary("src/utils.ts", "src/utils.ts"));
        assert!(!suffix_matches_at_boundary(
            "/home/user/project/src/utils.ts",
            "other.ts"
        ));
        // Must match at component boundary, not inside a component name.
        assert!(!suffix_matches_at_boundary(
            "/home/user/project/src/io.rs",
            "o.rs"
        ));
    }

    // ── SuffixIndex tests ──────────────────────────────────────

    #[test]
    fn suffix_index_build_and_resolve_simple() {
        let mut paths = HashSet::new();
        paths.insert("src/utils.ts".to_owned());
        paths.insert("src/models/user.ts".to_owned());
        let idx = SuffixIndex::build(&paths);

        // Simple suffix: `utils.ts` → `src/utils.ts`
        assert_eq!(idx.resolve("crate::utils"), Some("src/utils.ts".to_owned()));
    }

    #[test]
    fn suffix_index_nested_suffix() {
        let mut paths = HashSet::new();
        paths.insert("src/models/user.rs".to_owned());
        paths.insert("tests/integration/user.rs".to_owned());
        let idx = SuffixIndex::build(&paths);

        // nested suffix: `models/user` → the path ending in `models/user.rs`
        assert_eq!(
            idx.resolve("crate::models::user"),
            Some("src/models/user.rs".to_owned())
        );
    }

    #[test]
    fn suffix_index_extension_match() {
        let mut paths = HashSet::new();
        paths.insert("src/lib.rs".to_owned());
        let idx = SuffixIndex::build(&paths);

        // module `lib` without extension should resolve to `lib.rs`
        assert_eq!(idx.resolve("crate::lib"), Some("src/lib.rs".to_owned()));
    }

    #[test]
    fn suffix_index_no_match_returns_none() {
        let mut paths = HashSet::new();
        paths.insert("src/lib.rs".to_owned());
        let idx = SuffixIndex::build(&paths);

        assert_eq!(idx.resolve("crate::nonexistent"), None);
    }

    #[test]
    fn suffix_index_super_prefix() {
        let mut paths = HashSet::new();
        paths.insert("src/models/user.rs".to_owned());
        let idx = SuffixIndex::build(&paths);

        assert_eq!(
            idx.resolve("super::models::user"),
            Some("src/models/user.rs".to_owned())
        );
    }

    #[test]
    fn suffix_index_self_prefix() {
        let mut paths = HashSet::new();
        paths.insert("src/models/user.rs".to_owned());
        let idx = SuffixIndex::build(&paths);

        assert_eq!(
            idx.resolve("self::models::user"),
            Some("src/models/user.rs".to_owned())
        );
    }

    #[test]
    fn suffix_index_first_insertion_wins_on_collision() {
        let mut paths = HashSet::new();
        paths.insert("src/models/user.rs".to_owned());
        paths.insert("tests/fixtures/models/user.rs".to_owned());
        let idx = SuffixIndex::build(&paths);

        // Both paths share suffix `models/user.rs`. The first inserted wins.
        let result = idx.resolve("crate::models::user").unwrap();
        assert!(
            result == "src/models/user.rs" || result == "tests/fixtures/models/user.rs",
            "resolved to: {result}"
        );
    }

    #[test]
    fn suffix_index_dot_separated_module() {
        let mut paths = HashSet::new();
        paths.insert("src/utils.py".to_owned());
        let idx = SuffixIndex::build(&paths);

        // Python-style: `src.utils` → suffix `src/utils` + ext → `src/utils.py`
        assert_eq!(idx.resolve("src.utils"), Some("src/utils.py".to_owned()));
    }

    // ── Internal crate / dynamic names tests ─────────────────

    #[test]
    fn first_module_segment_extracts_crate_name() {
        assert_eq!(first_module_segment("seshat_graph"), "seshat_graph");
        assert_eq!(
            first_module_segment("seshat_graph::validate_approach"),
            "seshat_graph"
        );
        assert_eq!(first_module_segment("serde::Serialize"), "serde");
        assert_eq!(first_module_segment("std::collections::HashMap"), "std");
    }

    #[test]
    fn is_internal_crate_with_dynamic_names() {
        let names: Vec<String> = vec!["seshat_graph".to_owned(), "seshat_core".to_owned()];
        assert!(is_internal_crate("seshat_graph", &names));
        assert!(is_internal_crate("seshat_graph::validate_approach", &names));
        assert!(is_internal_crate("seshat_core::ir", &names));
        assert!(!is_internal_crate("serde", &names));
        assert!(!is_internal_crate("tokio::runtime", &names));
    }

    #[test]
    fn is_internal_crate_empty_names_returns_false() {
        assert!(!is_internal_crate("seshat_graph", &[]));
        assert!(!is_internal_crate("serde", &[]));
    }

    #[test]
    fn is_likely_internal_with_dynamic_names() {
        let names: Vec<String> = vec!["seshat_graph".to_owned(), "seshat_core".to_owned()];
        assert!(is_likely_internal(
            "seshat_graph::validate_approach",
            &names
        ));
        assert!(is_likely_internal("seshat_core::ProjectFile", &names));
        assert!(!is_likely_internal("serde::Serialize", &names));
        assert!(!is_likely_internal("tokio", &names));
    }

    #[test]
    fn non_internal_external_import_returns_none() {
        let paths = HashSet::new();
        let idx = SuffixIndex::build(&paths);

        assert_eq!(
            resolve_import("serde::Serialize", Path::new(""), &paths, &idx, &[]),
            None
        );
    }

    // ── infer_package_root tests ─────────────────────────────

    #[test]
    fn infer_package_root_finds_src_parent() {
        // Standard Cargo layout: .../crates/seshat-graph/src/error.rs
        let path = Path::new("/home/user/project/crates/seshat-graph/src/error.rs");
        let root = infer_package_root(path);
        assert_eq!(
            root,
            PathBuf::from("/home/user/project/crates/seshat-graph")
        );
    }

    #[test]
    fn infer_package_root_nested_file() {
        // File in a subdirectory under src/
        let path = Path::new("/proj/crates/foo/src/sub/mod.rs");
        let root = infer_package_root(path);
        assert_eq!(root, PathBuf::from("/proj/crates/foo"));
    }

    #[test]
    fn infer_package_root_no_src_falls_back_to_dir() {
        // No src/ in path — fall back to file's directory
        let path = Path::new("/go/src/myapp/pkg/utils/utils.go");
        // "src" appears as a component → returns its parent
        let root = infer_package_root(path);
        assert_eq!(root, PathBuf::from("/go"));
    }

    #[test]
    fn infer_package_root_flat_layout_fallback() {
        // Python flat layout: no src/ anywhere → fallback to file's dir
        let path = Path::new("/project/mypackage/error.py");
        let root = infer_package_root(path);
        assert_eq!(root, PathBuf::from("/project/mypackage"));
    }

    // ── Cross-crate guard tests ──────────────────────────────

    #[test]
    fn crate_import_does_not_match_across_crates() {
        // `use crate::error::CliError` in seshat-cli MUST NOT match
        // seshat-graph/src/error.rs — they are in different crates.
        let result = import_resolves_to_target(
            "crate::error",
            // importing_dir: seshat-cli/src/
            Path::new("/proj/crates/seshat-cli/src"),
            // target: seshat-graph/src/error.rs
            "/proj/crates/seshat-graph/src/error.rs",
            "/proj/crates/seshat-graph/src/error",
        );
        assert!(
            !result,
            "crate::error from seshat-cli must NOT match seshat-graph/src/error.rs"
        );
    }

    #[test]
    fn crate_import_matches_within_same_crate() {
        // `use crate::error::GraphError` in seshat-graph MUST match
        // seshat-graph/src/error.rs — same crate.
        let result = import_resolves_to_target(
            "crate::error",
            // importing_dir: seshat-graph/src/
            Path::new("/proj/crates/seshat-graph/src"),
            // target: seshat-graph/src/error.rs
            "/proj/crates/seshat-graph/src/error.rs",
            "/proj/crates/seshat-graph/src/error",
        );
        assert!(
            result,
            "crate::error from seshat-graph must match seshat-graph/src/error.rs"
        );
    }

    #[test]
    fn self_import_does_not_match_across_crates() {
        let result = import_resolves_to_target(
            "self::utils",
            Path::new("/proj/crates/crate-a/src"),
            "/proj/crates/crate-b/src/utils.rs",
            "/proj/crates/crate-b/src/utils",
        );
        assert!(!result, "self::utils must not cross crate boundaries");
    }

    #[test]
    fn crate_nested_module_matches_within_same_crate() {
        // `use crate::models::user` in seshat-graph matches
        // seshat-graph/src/models/user.rs
        let result = import_resolves_to_target(
            "crate::models::user",
            Path::new("/proj/crates/seshat-graph/src"),
            "/proj/crates/seshat-graph/src/models/user.rs",
            "/proj/crates/seshat-graph/src/models/user",
        );
        assert!(
            result,
            "crate::models::user must match within the same crate"
        );
    }

    #[test]
    fn crate_nested_module_does_not_match_different_crate() {
        let result = import_resolves_to_target(
            "crate::models::user",
            Path::new("/proj/crates/seshat-cli/src"),
            "/proj/crates/seshat-graph/src/models/user.rs",
            "/proj/crates/seshat-graph/src/models/user",
        );
        assert!(
            !result,
            "crate::models::user from seshat-cli must not match seshat-graph file"
        );
    }

    #[test]
    fn query_dependencies_no_cross_crate_false_positive() {
        // Regression test for the real-world bug:
        // db.rs, init.rs, etc. in seshat-cli all use `crate::error::CliError`.
        // seshat-graph/src/error.rs must NOT appear as their dependency,
        // and seshat-cli files must NOT appear as dependents of
        // seshat-graph/src/error.rs.
        let conn = test_conn();

        // seshat-graph/src/error.rs — no imports, has exports
        let graph_error = make_file(
            "crates/seshat-graph/src/error.rs",
            vec![],
            vec![Export {
                name: "GraphError".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            vec![],
        );

        // seshat-cli/src/db.rs — imports `crate::error::CliError` (same-crate ref)
        let cli_db = make_file(
            "crates/seshat-cli/src/db.rs",
            vec![Import {
                module: "crate::error".to_owned(),
                names: vec!["CliError".to_owned()],
                is_type_only: false,
                line: 15,
            }],
            vec![],
            vec![],
        );

        // seshat-cli/src/error.rs — the actual target of the crate::error import
        let cli_error = make_file(
            "crates/seshat-cli/src/error.rs",
            vec![],
            vec![Export {
                name: "CliError".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            vec![],
        );

        insert_ir(&conn, "main", &graph_error);
        insert_ir(&conn, "main", &cli_db);
        insert_ir(&conn, "main", &cli_error);

        // seshat-graph/src/error.rs must have ZERO dependents —
        // seshat-cli/src/db.rs does NOT import from it.
        let result = query_dependencies(&conn, "main", "crates/seshat-graph/src/error.rs").unwrap();
        assert!(
            result.dependents.is_empty(),
            "seshat-graph/src/error.rs must have no dependents; \
             crate::error in seshat-cli refers to seshat-cli/src/error.rs, not this file. \
             Got: {:?}",
            result.dependents
        );

        // seshat-cli/src/error.rs must have db.rs as a dependent.
        let result = query_dependencies(&conn, "main", "crates/seshat-cli/src/error.rs").unwrap();
        assert!(
            result
                .dependents
                .iter()
                .any(|d| d.file_path.contains("db.rs")),
            "seshat-cli/src/error.rs must have db.rs as dependent. Got: {:?}",
            result.dependents
        );
    }
||||||| parent of 6dc99c7 (feat: [US-004] - Add load_internal_names() and remove hardcoded WORKSPACE_CRATES)

    #[test]
    fn workspace_crate_import_resolves_to_target() {
        // The module "seshat_graph::validate_approach" resolves to the file
        // "crates/seshat-graph/src/validate_approach.rs" since it starts with
        // the workspace crate prefix and the rest matches by suffix.
        assert!(import_resolves_to_target(
            "seshat_graph::validate_approach",
            Path::new(""),
            "crates/seshat-graph/src/validate_approach.rs",
            "validate_approach",
            "crates/seshat-graph/src/validate_approach",
        ));
    }

    #[test]
    fn query_dependencies_resolves_workspace_crate_import() {
        let conn = test_conn();

        let caller = make_file(
            "crates/seshat-cli/src/scan.rs",
            vec![Import {
                module: "seshat_graph::validate_approach".to_owned(),
                names: vec!["validate_approach".to_owned()],
                is_type_only: false,
                line: 5,
            }],
            vec![],
            vec![],
        );
        let target = make_file(
            "crates/seshat-graph/src/validate_approach.rs",
            vec![],
            vec![Export {
                name: "validate_approach".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
            }],
            vec![],
        );

        insert_ir(&conn, "main", &caller);
        insert_ir(&conn, "main", &target);

        // Query the caller — should see the resolved dependency.
        let result = query_dependencies(&conn, "main", "crates/seshat-cli/src/scan.rs").unwrap();
        let resolved: Vec<_> = result.dependencies.iter().filter(|d| d.resolved).collect();
        assert!(
            !resolved.is_empty(),
            "Expected resolved dependency for workspace crate import"
        );
        assert!(
            resolved
                .iter()
                .any(|d| d.file_path.contains("validate_approach")),
            "Expected validate_approach in resolved dependencies"
        );

        // Query the target — should see the caller as a dependent.
        let result = query_dependencies(
            &conn,
            "main",
            "crates/seshat-graph/src/validate_approach.rs",
        )
        .unwrap();
        assert!(
            result
                .dependents
                .iter()
                .any(|d| d.file_path.contains("scan.rs")),
            "Expected scan.rs as dependent of validate_approach.rs"
        );
    }
=======
>>>>>>> 6dc99c7 (feat: [US-004] - Add load_internal_names() and remove hardcoded WORKSPACE_CRATES)
}
