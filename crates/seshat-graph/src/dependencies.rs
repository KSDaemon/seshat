//! Dependency analysis over deserialized IR.
//!
//! Provides `query_dependencies()` which builds a dependency index from IR
//! and returns direct dependents/dependencies with blast radius for any file.

use std::collections::{BTreeSet, HashMap, HashSet};
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

/// Maximum number of dependents enumerated by `compute_transitive_dependents`
/// across all depths combined. On overflow the result is capped and
/// [`TransitiveResult::truncated`] is set to `true`. Direct (depth-1) entries
/// are enumerated before any transitive expansion, so they survive capping
/// even when the depth-2+ frontier would push the total above this bound.
pub const MAX_DEPENDENTS: usize = 500;

/// Maximum allowed value for [`QueryDependenciesOptions::depth`]. Requests
/// outside the inclusive range `1..=MAX_TRANSITIVE_DEPTH` are rejected with
/// [`GraphError::InvalidInput`] before any IR load.
pub const MAX_TRANSITIVE_DEPTH: u32 = 10;

/// AI-agent-friendly default depth for transitive dependent queries. Surfaces
/// 1st/2nd/3rd-order ripple in a single call without forcing the caller to
/// know the parameter exists. Used by the MCP tool layer and by
/// `compute_affected_symbols` so that `map_diff_impact` reports transitive
/// blast radius by default.
pub const DEFAULT_TRANSITIVE_DEPTH: u32 = 3;

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
    /// Files that import from the target. When `requested_depth > 1` this
    /// contains both direct (`depth == 1`) and transitive (`depth >= 2`)
    /// entries; direct entries appear first.
    pub dependents: Vec<DependentEntry>,
    /// External dependencies used by the target file.
    pub external_dependencies: Vec<ExternalDependency>,
    /// Blast radius classification. Computed from the **direct** dependent
    /// count only, so existing thresholds remain stable when callers opt
    /// into transitive expansion.
    pub blast_radius: BlastRadius,
    /// Total number of entries in `dependents` (direct + transitive). Equal
    /// to `dependents.len()` and to the count of files reached by BFS up to
    /// `requested_depth`.
    pub transitive_dependent_count: usize,
    /// The depth value the caller requested (echoed from
    /// [`QueryDependenciesOptions::depth`]). `1` for direct-only queries.
    pub requested_depth: u32,
    /// Backward compatibility note, present when dependents exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backward_compatibility_note: Option<String>,
    /// Whether IR loading was truncated (LIMIT reached), meaning results
    /// may be incomplete for very large repositories.
    #[serde(default)]
    pub truncated: bool,
}

/// Optional parameters for [`query_dependencies`] / [`query_dependencies_batch`].
///
/// Defaults to direct-only (`depth = 1`) so existing callsites can opt out of
/// transitive expansion by passing [`QueryDependenciesOptions::default()`].
#[derive(Debug, Clone, Copy)]
pub struct QueryDependenciesOptions {
    /// Maximum BFS depth for the dependents traversal. `1` returns direct
    /// dependents only (preserves the historical contract); `2..=MAX_TRANSITIVE_DEPTH`
    /// enables transitive expansion. Out-of-range values cause the public
    /// API to return [`GraphError::InvalidInput`] before any IR load.
    pub depth: u32,
}

impl Default for QueryDependenciesOptions {
    fn default() -> Self {
        Self { depth: 1 }
    }
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
    /// Names that this file imports from the target. Populated only for
    /// direct dependents (`depth == 1`); empty for transitive entries because
    /// transitive imports are not visible at the IR level.
    pub import_names: Vec<String>,
    /// Line number of the import statement. Populated only for direct
    /// dependents (`depth == 1`); `0` for transitive entries.
    pub line: usize,
    /// Depth at which this dependent was discovered: `1` for direct,
    /// `2` for second-order, and so on.
    pub depth: u32,
    /// Intermediate file paths between the target and this dependent
    /// (full paths, in BFS order, excluding both endpoints). Empty for
    /// direct dependents (`depth == 1`).
    pub via: Vec<String>,
}

/// A reverse-adjacency edge: a file that imports from a target.
///
/// Stored as the value of the `HashMap<target_path, Vec<ReverseEdge>>`
/// returned by `build_reverse_adjacency`.
#[derive(Debug, Clone)]
pub struct ReverseEdge {
    /// File path of the importing (dependent) file.
    pub from: String,
    /// Names imported from the target (deduplicated across multiple
    /// import statements in the same file).
    pub import_names: Vec<String>,
    /// Line number of the first import statement on this edge.
    pub line: usize,
}

/// Result of a transitive-dependents BFS over a reverse-adjacency map.
///
/// Returned by `compute_transitive_dependents`.
#[derive(Debug, Clone, Serialize)]
pub struct TransitiveResult {
    /// Discovered dependents. Direct entries (depth 1) appear before
    /// transitive entries; within a depth, entries are sorted lexicographically
    /// by `file_path` (deterministic across runs).
    pub entries: Vec<DependentEntry>,
    /// `true` if the result was capped at [`MAX_DEPENDENTS`].
    pub truncated: bool,
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
///
/// Visible across the crate so [`build_reverse_adjacency`] and
/// [`query_dependencies`] can amortise the build cost across many
/// resolutions, but kept out of the public surface — there are no
/// external consumers and exposing it would lock in the implementation
/// details (the `HashMap<String, String>` shape, the first-insertion-wins
/// rule, etc.).
#[derive(Debug, Clone)]
pub(crate) struct SuffixIndex {
    map: HashMap<String, String>,
}

impl SuffixIndex {
    /// Build a suffix index from known file paths.
    ///
    /// For each known path, all suffixes of increasing depth are inserted
    /// (e.g. for `src/models/user.ts`: `user.ts`, `models/user.ts`, `src/models/user.ts`).
    /// When multiple paths share the same suffix, the first insertion wins.
    pub(crate) fn build(known_paths: &HashSet<String>) -> Self {
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
/// `opts.depth` controls how far the dependents traversal walks: `1` returns
/// direct dependents only (the historical contract); higher values enable
/// transitive expansion up to [`MAX_TRANSITIVE_DEPTH`]. The returned
/// `DependencyData::dependents` then contains both direct and transitive
/// entries; `blast_radius` is still derived from the direct count only.
///
/// Returns `Err(GraphError::NodeNotFound)` if the target path is not in the IR,
/// or `Err(GraphError::InvalidInput)` if the target path is empty or
/// `opts.depth` is outside `1..=MAX_TRANSITIVE_DEPTH`.
pub fn query_dependencies(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    target_path: &str,
    opts: QueryDependenciesOptions,
) -> Result<DependencyData, GraphError> {
    validate_depth(opts.depth)?;

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

    // Build dependents. For depth=1 take the direct-only fast path; for
    // depth>=2 build the reverse adjacency once and BFS over it.
    // BFS truncation (when MAX_DEPENDENTS is hit) is OR-ed into the
    // existing IR-loading truncation flag so callers see a single
    // truncated signal regardless of which layer capped first.
    let (dependents, direct_count, dependents_truncated) = if opts.depth == 1 {
        let direct = build_dependents(&target_path_str, files, &internal_names);
        let direct_count = direct.len();
        // Direct-only path uses `build_dependents` which has no internal
        // cap; only an IR-loading truncation can show up here.
        (direct, direct_count, false)
    } else {
        let reverse = build_reverse_adjacency(files, &internal_names, &suffix_index);
        let result = compute_transitive_dependents(&target_path_str, &reverse, opts.depth);
        let direct_count = result.entries.iter().filter(|e| e.depth == 1).count();
        (result.entries, direct_count, result.truncated)
    };
    let truncated = truncated || dependents_truncated;

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

    // Blast radius is classified from the direct count only so existing
    // thresholds remain stable across opt-in transitive queries.
    let blast_radius = classify_blast_radius(direct_count);

    // Backward compatibility note.
    let backward_compatibility_note = if direct_count > 0 {
        Some(format!(
            "This file has {direct_count} direct dependent(s). Changes to its public API may require updates in those files."
        ))
    } else {
        None
    };

    let transitive_dependent_count = dependents.len();

    Ok(DependencyData {
        target: target_path_str,
        dependencies,
        dependents,
        external_dependencies,
        blast_radius,
        transitive_dependent_count,
        requested_depth: opts.depth,
        backward_compatibility_note,
        truncated,
    })
}

/// Validate that `depth` is within `1..=MAX_TRANSITIVE_DEPTH`.
fn validate_depth(depth: u32) -> Result<(), GraphError> {
    if depth == 0 || depth > MAX_TRANSITIVE_DEPTH {
        return Err(GraphError::InvalidInput(format!(
            "depth must be between 1 and {MAX_TRANSITIVE_DEPTH} (got {depth})"
        )));
    }
    Ok(())
}

/// Batch query dependencies for multiple files with a single IR load.
///
/// Loads IR once and builds a dependents index, then computes
/// `DependencyData` for every requested path. This is O(N) instead
/// of N x O(IR_load) — much faster when checking many changed files.
///
/// When `opts.depth > 1` the reverse-adjacency map is built **exactly once**
/// across the whole IR and reused for every target — so transitive batch
/// queries are O(IR + targets × BFS), not O(targets × IR).
pub fn query_dependencies_batch(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    paths: &[String],
    opts: QueryDependenciesOptions,
) -> Result<Vec<DependencyData>, GraphError> {
    validate_depth(opts.depth)?;

    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let loaded_ir = load_branch_ir(conn, branch_id)?;
    let files = &loaded_ir.files;
    let truncated = loaded_ir.truncated;

    // Load internal crate/package names from the database.
    let internal_names = load_internal_names(conn, branch_id);

    let known_paths: HashSet<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    let suffix_index = SuffixIndex::build(&known_paths);

    // Build the reverse adjacency once when transitive expansion is requested.
    // For depth==1 callers we keep the fast path through `build_dependents`.
    let reverse: Option<HashMap<String, Vec<ReverseEdge>>> = if opts.depth > 1 {
        Some(build_reverse_adjacency(
            files,
            &internal_names,
            &suffix_index,
        ))
    } else {
        None
    };

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

        let dependencies =
            build_dependencies(target_file, &known_paths, &suffix_index, &internal_names);

        let (dependents, direct_count, dependents_truncated) = match &reverse {
            None => {
                let direct = build_dependents(&target_path_str, files, &internal_names);
                let direct_count = direct.len();
                (direct, direct_count, false)
            }
            Some(reverse) => {
                let result = compute_transitive_dependents(&target_path_str, reverse, opts.depth);
                let direct_count = result.entries.iter().filter(|e| e.depth == 1).count();
                (result.entries, direct_count, result.truncated)
            }
        };
        // OR the per-target BFS truncation into the shared IR-loading
        // truncated flag so each result accurately reflects whether the
        // *specific* target hit the cap.
        let truncated = truncated || dependents_truncated;

        let external_dependencies: Vec<ExternalDependency> = target_file
            .dependencies_used
            .iter()
            .map(|d| ExternalDependency {
                package: d.package.clone(),
                import_path: d.import_path.clone(),
                line: d.line,
            })
            .collect();

        let blast_radius = classify_blast_radius(direct_count);

        let backward_compatibility_note = if direct_count > 0 {
            Some(format!(
                "This file has {direct_count} direct dependent(s). Changes to its public API may require updates in those files."
            ))
        } else {
            None
        };

        let transitive_dependent_count = dependents.len();

        results.push(DependencyData {
            target: target_path_str,
            dependencies,
            dependents,
            external_dependencies,
            blast_radius,
            transitive_dependent_count,
            requested_depth: opts.depth,
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
/// `super/`, `self/` prefixes. Internal crate prefixes are handled by callers
/// (see [`strip_first_segment`]) before this function is called.
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
///
/// Uses a linear scan over `internal_names`.  For projects with hundreds of
/// workspace members, callers may convert `internal_names` to a `HashSet`
/// before calling this function in a hot loop.
fn is_internal_crate(module: &str, internal_names: &[String]) -> bool {
    let first = first_module_segment(module);
    internal_names.iter().any(|n| n == first)
}

/// Strip the first segment from a module path, returning the remaining suffix
/// with any leading separator (`::` or `.`) removed.
///
/// Returns `None` when the suffix is empty (the module consisted only of the
/// first segment, e.g. `seshat_graph` without a sub-path).
fn strip_first_segment(module: &str) -> Option<&str> {
    let first = first_module_segment(module);
    let after = &module[first.len()..];
    let rest = after
        .strip_prefix("::")
        .or_else(|| after.strip_prefix('.'))
        .unwrap_or(after);
    if rest.is_empty() { None } else { Some(rest) }
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
///
/// When the module is a bare crate name (e.g. `seshat_graph` without sub-path),
/// resolves to the crate root entry-point file: `lib.rs` for Rust, `__init__.py`
/// for Python, or `index.ts`/`index.js` for JS/TS.
fn resolve_internal_crate_import(module: &str, suffix_index: &SuffixIndex) -> Option<String> {
    match strip_first_segment(module) {
        Some(rest) => suffix_index.resolve(rest),
        None => resolve_crate_root(module, suffix_index),
    }
}

/// Resolve a bare crate/package name to its root entry-point file.
///
/// Tries common root-file suffixes: `mod.rs` (Rust 2015), `lib.rs` (Rust 2018+),
/// `index.ts`, `index.js`, `__init__.py`.
fn resolve_crate_root(module: &str, suffix_index: &SuffixIndex) -> Option<String> {
    for suffix in &["mod", "lib", "index"] {
        let path = format!("{module}/{suffix}");
        if let Some(resolved) = suffix_index.resolve(&path) {
            return Some(resolved);
        }
    }
    None
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
                // `build_dependents` only enumerates direct (depth-1) dependents.
                // Transitive expansion is performed by `compute_transitive_dependents`.
                depth: 1,
                via: Vec::new(),
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
        match strip_first_segment(module) {
            Some(rest) => {
                let suffix = module_to_path_suffix(rest);
                suffix_matches_at_boundary(target_normalized, &suffix)
                    || suffix_matches_at_boundary(target_name_no_ext, &suffix)
            }
            None => {
                // Bare crate name — check if target is the crate root.
                false
            }
        }
    } else if is_likely_internal(module, internal_names) {
        // Absolute-style internal import (crate::, super::, self::, src.) —
        // check suffix match at path boundary.
        let suffix = module_to_path_suffix(module);

        // Apply the cross-crate guard for `crate::` / `self::` / `super::`
        // keywords (same-package-only constructs) before allowing the suffix
        // match. Shared with [`resolve_import_to_known_path`].
        if !package_boundary_ok(module, importing_dir, Path::new(target_normalized)) {
            return false;
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

/// Returns `true` unless `module` is a same-crate keyword
/// (`crate::` / `self::` / `super::`) AND the resolved file lives in a
/// different package root than the importing file.
///
/// This prevents `use crate::error` in crate A from spuriously matching
/// `error.rs` in crate B. `super::` is also a same-crate construct; for
/// relative-path imports the resolution already operates on real filesystem
/// joins so the guard is conservative there too.
///
/// Shared between [`import_resolves_to_target`] (single-target check used
/// by `build_dependents`) and [`resolve_import_to_known_path`] (any-target
/// resolver used by [`build_reverse_adjacency`]).
fn package_boundary_ok(module: &str, importing_dir: &Path, resolved: &Path) -> bool {
    let is_same_package_keyword = module == "crate"
        || module.starts_with("crate::")
        || module == "self"
        || module.starts_with("self::")
        || module == "super"
        || module.starts_with("super::");

    if !is_same_package_keyword {
        return true;
    }

    infer_package_root(importing_dir) == infer_package_root(resolved)
}

/// Resolve an import expression to the absolute file path of a known IR
/// file, applying the same package-boundary guards as
/// [`import_resolves_to_target`].
///
/// Returns `None` when the import does not resolve to any file in the IR
/// (e.g. external packages, unresolved type-only imports, or cross-crate
/// `crate::` references blocked by [`package_boundary_ok`]).
///
/// Used by [`build_reverse_adjacency`] to compute reverse-edge keys: each
/// resolved path becomes a key in the resulting `HashMap`. Sharing the
/// resolution logic with `import_resolves_to_target` ensures forward
/// (`build_dependents`) and reverse (`build_reverse_adjacency`) views agree
/// on which (file, import) pairs constitute a dependency edge.
pub(crate) fn resolve_import_to_known_path(
    module: &str,
    importing_dir: &Path,
    known_paths: &HashSet<String>,
    suffix_index: &SuffixIndex,
    internal_names: &[String],
) -> Option<String> {
    let resolved = resolve_import(
        module,
        importing_dir,
        known_paths,
        suffix_index,
        internal_names,
    )?;

    if !package_boundary_ok(module, importing_dir, Path::new(&resolved)) {
        return None;
    }

    Some(resolved)
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

// ── Transitive dependents (BFS over reverse adjacency) ───────

/// Build a reverse-adjacency map from the IR in a single O(N×D) pass over
/// every (file, import) pair (`N` = files, `D` = average imports per file).
///
/// The returned map is keyed by **target file path** (the absolute path of the
/// imported file as stored in the IR) and the value is the list of
/// [`ReverseEdge`]s pointing to it. Each edge carries the dependent file's
/// path together with the names imported on the first import statement
/// (deduplicated across multiple imports of the same target from one file).
///
/// Self-edges (a file resolving an import to its own path, possible via
/// re-exports) are skipped so the BFS never revisits the seed node.
///
/// Reverse-edge keys go through [`resolve_import_to_known_path`], which
/// applies the same cross-crate guard as [`import_resolves_to_target`] so
/// the reverse view matches the forward view exactly.
pub(crate) fn build_reverse_adjacency(
    files: &[seshat_core::ProjectFile],
    internal_names: &[String],
    suffix_index: &SuffixIndex,
) -> HashMap<String, Vec<ReverseEdge>> {
    let known_paths: HashSet<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    let mut reverse: HashMap<String, Vec<ReverseEdge>> = HashMap::new();

    for file in files {
        let from = file.path.to_string_lossy().to_string();
        let importing_dir = Path::new(&file.path)
            .parent()
            .unwrap_or_else(|| Path::new(""));

        // Per-file deduplication: when a single file imports the same target
        // multiple times (e.g. one type-only import + one runtime import),
        // we want a single ReverseEdge whose `import_names` is the union.
        // Uses `BTreeSet` for O(log n) insertion + automatic dedup; the
        // previous `Vec::contains` was O(n^2) in the per-file import-name
        // count, which the perf tests don't exercise (single-import
        // fixtures) but heavy real files can hit.
        let mut per_target: HashMap<String, (BTreeSet<String>, usize)> = HashMap::new();

        for import in &file.imports {
            let Some(resolved) = resolve_import_to_known_path(
                &import.module,
                importing_dir,
                &known_paths,
                suffix_index,
                internal_names,
            ) else {
                continue;
            };

            // Skip self-edges so reverse[X] never contains an edge from X.
            if resolved == from {
                continue;
            }

            let entry = per_target
                .entry(resolved)
                .or_insert_with(|| (BTreeSet::new(), import.line));
            for name in &import.names {
                entry.0.insert(name.clone());
            }
            // Track the smallest line number across all imports in the file
            // that resolve to this target, matching `build_dependents`.
            if import.line < entry.1 {
                entry.1 = import.line;
            }
        }

        for (target, (import_names, line)) in per_target {
            reverse.entry(target).or_default().push(ReverseEdge {
                from: from.clone(),
                // BTreeSet -> sorted Vec; sortedness is also a small
                // determinism win for downstream consumers comparing
                // ReverseEdges across runs.
                import_names: import_names.into_iter().collect(),
                line,
            });
        }
    }

    reverse
}

/// BFS over a reverse-adjacency map to enumerate all transitive dependents
/// of `target` up to `depth` levels deep.
///
/// Properties:
/// - **Cycle-safe**: a `HashSet<String>` keyed by file path tracks visited
///   nodes; the seed is inserted up front so cycles `a → b → a` terminate.
/// - **Direct-first**: depth-1 entries are pushed before any depth-2
///   expansion, so they are preserved across [`MAX_DEPENDENTS`] capping.
/// - **Deterministic**: parents and edges are processed in lexicographic
///   order, so when a diamond reaches the same node via multiple paths the
///   chosen `via` is reproducible across runs.
/// - **Capped**: the total number of entries is bounded by [`MAX_DEPENDENTS`];
///   on overflow [`TransitiveResult::truncated`] is set to `true`.
///
/// `depth = 0` returns an empty result (no dependents at depth 0). The
/// MCP-layer caller validates the depth range; this function does not.
pub(crate) fn compute_transitive_dependents(
    target: &str,
    reverse: &HashMap<String, Vec<ReverseEdge>>,
    depth: u32,
) -> TransitiveResult {
    /// One BFS frontier node: the discovered file and the chain of
    /// intermediates from (just after) the target down to and including
    /// this node. The chain becomes the `via` we pass to children.
    struct Node {
        path: String,
        chain: Vec<String>,
    }

    // Internal callers must respect the same bounds the public API
    // validates (see [`validate_depth`]). The public entry points
    // (`query_dependencies`, `query_dependencies_batch`, MCP layer)
    // already reject out-of-range values; this assertion catches any
    // future internal caller that bypasses them. We also accept
    // `depth == 0` at runtime by returning an empty result, but flag it
    // in debug builds — production code should never request 0.
    debug_assert!(
        depth <= MAX_TRANSITIVE_DEPTH,
        "compute_transitive_dependents: depth={depth} exceeds MAX_TRANSITIVE_DEPTH={MAX_TRANSITIVE_DEPTH}; \
         callers should validate via validate_depth()"
    );

    let mut entries: Vec<DependentEntry> = Vec::new();
    let mut truncated = false;

    if depth == 0 {
        return TransitiveResult { entries, truncated };
    }

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(target.to_string());

    let mut current: Vec<Node> = vec![Node {
        path: target.to_string(),
        chain: Vec::new(),
    }];

    'outer: for d in 1..=depth {
        // Collect all (child, candidate parent.chain, edge) tuples
        // reachable from the current frontier at this depth. Diamonds
        // surface here as multiple candidates for the same child.
        let mut candidates: HashMap<String, Vec<(Vec<String>, &ReverseEdge)>> = HashMap::new();

        for parent in &current {
            let Some(edges) = reverse.get(&parent.path) else {
                continue;
            };
            for edge in edges {
                if visited.contains(&edge.from) {
                    continue;
                }
                candidates
                    .entry(edge.from.clone())
                    .or_default()
                    .push((parent.chain.clone(), edge));
            }
        }

        // Stable iteration over children: lex on the child path. The
        // tie-break BETWEEN candidate chains for the same child uses
        // the joined `via` string (PRD Q2 resolved decision).
        let mut sorted_children: Vec<String> = candidates.keys().cloned().collect();
        sorted_children.sort();

        let mut next: Vec<Node> = Vec::new();

        for child_path in sorted_children {
            if entries.len() >= MAX_DEPENDENTS {
                truncated = true;
                break 'outer;
            }
            // Defensive: the per-edge `visited.contains` filter above
            // should already prevent this, but cheap to double-check.
            if visited.contains(&child_path) {
                continue;
            }

            // PRD Q2: "Diamond `via` tie-break: lexicographic on joined
            // `via` string". Pick the candidate whose joined chain is
            // lex-smallest. The "/" separator is conventional and
            // matches what the caller would produce when rendering the
            // `via` Vec<String> for human display.
            //
            // `expect` is sound: HashMap entries reach this point only
            // when the key was inserted at least once, so the Vec is
            // non-empty by construction.
            let cands = candidates
                .remove(&child_path)
                .expect("candidates entry must exist for sorted child path");
            let (best_chain, best_edge) = cands
                .into_iter()
                .min_by(|(a, _), (b, _)| a.join("/").cmp(&b.join("/")))
                .expect("candidates list must be non-empty by construction");

            visited.insert(child_path.clone());

            entries.push(DependentEntry {
                file_path: child_path.clone(),
                // import_names + line are only meaningful for direct
                // dependents — at depth >= 2 the IR doesn't carry the
                // information needed to attribute names to a chain.
                import_names: if d == 1 {
                    best_edge.import_names.clone()
                } else {
                    Vec::new()
                },
                line: if d == 1 { best_edge.line } else { 0 },
                depth: d,
                via: best_chain.clone(),
            });

            let mut child_chain = best_chain;
            child_chain.push(child_path.clone());
            next.push(Node {
                path: child_path,
                chain: child_chain,
            });
        }

        current = next;
    }

    TransitiveResult { entries, truncated }
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
                end_line: 12,
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
                end_line: 1,
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
                end_line: 5,
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
                end_line: 10,
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

        let result = query_dependencies(
            &conn,
            "main",
            "src/services/user_service.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();

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
        let result = query_dependencies(
            &conn,
            "main",
            "src/utils.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();

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
        let result = query_dependencies(
            &conn,
            "main",
            "src/utils.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();
        assert_eq!(result.blast_radius, BlastRadius::Low);
        assert_eq!(result.dependents.len(), 2);

        // app.ts has 0 dependents → low.
        let result = query_dependencies(
            &conn,
            "main",
            "src/app.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();
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

        let result = query_dependencies(
            &conn,
            "main",
            "src/orphan.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();

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

        let result = query_dependencies(
            &conn,
            "main",
            "src/nonexistent.ts",
            QueryDependenciesOptions::default(),
        );
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

        let result = query_dependencies(&conn, "main", "", QueryDependenciesOptions::default());
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

        let result = query_dependencies(
            &conn,
            "main",
            "src/services/user_service.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();

        assert_eq!(result.external_dependencies.len(), 1);
        assert_eq!(result.external_dependencies[0].package, "express");
    }

    #[test]
    fn backward_compatibility_note_present_when_dependents_exist() {
        let conn = test_conn();
        setup_project(&conn);

        // utils.ts has dependents.
        let result = query_dependencies(
            &conn,
            "main",
            "src/utils.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();
        assert!(result.backward_compatibility_note.is_some());

        // app.ts has no dependents.
        let result = query_dependencies(
            &conn,
            "main",
            "src/app.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();
        assert!(result.backward_compatibility_note.is_none());
    }

    #[test]
    fn no_self_reference_in_dependents() {
        let conn = test_conn();
        setup_project(&conn);

        let result = query_dependencies(
            &conn,
            "main",
            "src/utils.ts",
            QueryDependenciesOptions::default(),
        )
        .unwrap();

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
                end_line: 1,
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
        let result = query_dependencies(
            &conn,
            branch,
            "src/utils.ts",
            QueryDependenciesOptions::default(),
        );
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

    // ── Helper: seed workspace_crates in repo_metadata ────────

    /// Seed `workspace_crates` JSON into `repo_metadata` so that
    /// `load_internal_names` / `query_dependencies` picks them up.
    fn seed_internal_names(conn: &Arc<Mutex<Connection>>, names: &[&str]) {
        use seshat_storage::{RepoMetadataRepository, SqliteRepoMetadataRepository};
        let json = serde_json::to_string(names).unwrap();
        let repo = SqliteRepoMetadataRepository::new(Arc::clone(conn));
        repo.set("workspace_crates", &json)
            .expect("seed workspace_crates");
    }

    // ── Dynamic internal names — unit-level (resolve_import / is_internal_crate)

    #[test]
    fn rust_internal_crate_import_resolves_when_name_in_db() {
        // `use seshat_graph::foo` with ['seshat_graph'] → resolved to file path.
        let mut paths = HashSet::new();
        paths.insert("crates/seshat-graph/src/foo.rs".to_owned());
        let idx = SuffixIndex::build(&paths);
        let internal_names = vec!["seshat_graph".to_owned()];

        let result = resolve_import(
            "seshat_graph::foo",
            Path::new(""),
            &paths,
            &idx,
            &internal_names,
        );

        assert_eq!(
            result,
            Some("crates/seshat-graph/src/foo.rs".to_owned()),
            "seshat_graph::foo should resolve to foo.rs"
        );
    }

    #[test]
    fn external_crate_import_not_resolved() {
        // `use serde::Serialize` with any internal_names list → None (external).
        let paths = HashSet::new();
        let idx = SuffixIndex::build(&paths);
        let internal_names = vec!["seshat_graph".to_owned(), "my_crate".to_owned()];

        assert_eq!(
            resolve_import(
                "serde::Serialize",
                Path::new(""),
                &paths,
                &idx,
                &internal_names
            ),
            None,
            "serde::Serialize must not resolve — it is external"
        );
    }

    #[test]
    fn empty_internal_names_all_double_colon_imports_are_external() {
        // With empty internal_names, all `foo::Bar` style imports → external (None).
        let paths = HashSet::new();
        let idx = SuffixIndex::build(&paths);

        for module in &[
            "serde::Serialize",
            "tokio::runtime::Runtime",
            "std::collections::HashMap",
        ] {
            assert_eq!(
                resolve_import(module, Path::new(""), &paths, &idx, &[]),
                None,
                "with empty internal_names, {module} must be external"
            );
        }
    }

    #[test]
    fn python_absolute_import_resolves_via_suffix_index() {
        // `from my_package.utils import foo` with ['my_package'] → my_package/utils.py
        let mut paths = HashSet::new();
        paths.insert("my_package/utils.py".to_owned());
        let idx = SuffixIndex::build(&paths);
        let internal_names = vec!["my_package".to_owned()];

        let result = resolve_import(
            "my_package.utils",
            Path::new(""),
            &paths,
            &idx,
            &internal_names,
        );

        assert_eq!(
            result,
            Some("my_package/utils.py".to_owned()),
            "my_package.utils should resolve to my_package/utils.py"
        );
    }

    #[test]
    fn external_python_import_not_resolved() {
        // `from django.db import models` with ['my_package'] → None (external).
        let mut paths = HashSet::new();
        paths.insert("my_package/utils.py".to_owned());
        let idx = SuffixIndex::build(&paths);
        let internal_names = vec!["my_package".to_owned()];

        assert_eq!(
            resolve_import("django.db", Path::new(""), &paths, &idx, &internal_names),
            None,
            "django.db must not resolve — it is external"
        );
    }

    // ── Dynamic internal names — end-to-end via query_dependencies ────────────

    #[test]
    fn query_dependencies_resolves_internal_crate_import_from_db() {
        // End-to-end: internal names loaded from DB → DependencyEntry.resolved = true.
        let conn = test_conn();

        // Seed workspace_crates in repo_metadata (as the scanner does).
        seed_internal_names(&conn, &["seshat_graph"]);

        // The "library" file being depended on.
        let lib_file = make_file(
            "crates/seshat-graph/src/foo.rs",
            vec![],
            vec![Export {
                name: "Foo".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            vec![],
        );

        // The "consumer" file that imports via the internal crate name.
        let mut consumer = make_file(
            "src/main.rs",
            vec![Import {
                module: "seshat_graph::foo".to_owned(),
                names: vec!["Foo".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![],
            vec![],
        );
        // Make consumer Rust language so the IR is valid.
        use seshat_core::LanguageIR;
        consumer.language = seshat_core::ir::Language::Rust;
        consumer.language_ir = LanguageIR::Rust(seshat_core::ir::RustIR::default());

        insert_ir(&conn, "main", &lib_file);
        insert_ir(&conn, "main", &consumer);

        // Query from the consumer's perspective — it should have a resolved dep.
        let result = query_dependencies(
            &conn,
            "main",
            "src/main.rs",
            QueryDependenciesOptions::default(),
        )
        .expect("query should succeed");

        let resolved_deps: Vec<_> = result.dependencies.iter().filter(|d| d.resolved).collect();
        assert!(
            !resolved_deps.is_empty(),
            "seshat_graph::foo import must be resolved to a file, got deps: {:?}",
            result.dependencies
        );
        assert!(
            resolved_deps[0].file_path.contains("foo.rs"),
            "resolved dependency must be foo.rs, got: {:?}",
            resolved_deps[0].file_path
        );
    }

    #[test]
    fn query_dependencies_external_import_not_resolved() {
        // `use serde::Serialize` with internal_names=['my_crate'] → unresolved / excluded.
        let conn = test_conn();

        seed_internal_names(&conn, &["my_crate"]);

        let file = make_file(
            "src/lib.rs",
            vec![Import {
                module: "serde::Serialize".to_owned(),
                names: vec!["Serialize".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![],
            vec![],
        );
        insert_ir(&conn, "main", &file);

        let result = query_dependencies(
            &conn,
            "main",
            "src/lib.rs",
            QueryDependenciesOptions::default(),
        )
        .expect("query should succeed");

        // serde is external — it should not appear in dependencies at all
        // (external imports are excluded, not shown as unresolved).
        let serde_dep = result
            .dependencies
            .iter()
            .find(|d| d.file_path.contains("serde"));
        assert!(
            serde_dep.is_none(),
            "external serde import must not appear in dependencies; got: {:?}",
            result.dependencies
        );
    }

    #[test]
    fn query_dependencies_empty_internal_names_all_crate_imports_excluded() {
        // With no workspace_crates in DB, all `foo::Bar` imports → excluded.
        let conn = test_conn();
        // Do NOT seed workspace_crates.

        let file = make_file(
            "src/lib.rs",
            vec![
                Import {
                    module: "serde::Serialize".to_owned(),
                    names: vec![],
                    is_type_only: false,
                    line: 1,
                },
                Import {
                    module: "tokio::runtime::Runtime".to_owned(),
                    names: vec![],
                    is_type_only: false,
                    line: 2,
                },
            ],
            vec![],
            vec![],
        );
        insert_ir(&conn, "main", &file);

        let result = query_dependencies(
            &conn,
            "main",
            "src/lib.rs",
            QueryDependenciesOptions::default(),
        )
        .expect("query should succeed");

        // With no internal names, serde and tokio are external → dependencies list is empty.
        assert!(
            result.dependencies.is_empty(),
            "with no internal names, all :: imports must be excluded from dependencies; got: {:?}",
            result.dependencies
        );
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
            &[],
        );
        assert!(
            !result,
            "crate::error from seshat-cli must NOT match seshat-graph/src/error.rs"
        );
    }

    #[test]
    fn crate_import_matches_within_same_crate() {
        let result = import_resolves_to_target(
            "crate::error",
            Path::new("/proj/crates/seshat-graph/src"),
            "/proj/crates/seshat-graph/src/error.rs",
            "/proj/crates/seshat-graph/src/error",
            &[],
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
            &[],
        );
        assert!(!result, "self::utils must not cross crate boundaries");
    }

    #[test]
    fn crate_nested_module_matches_within_same_crate() {
        let result = import_resolves_to_target(
            "crate::models::user",
            Path::new("/proj/crates/seshat-graph/src"),
            "/proj/crates/seshat-graph/src/models/user.rs",
            "/proj/crates/seshat-graph/src/models/user",
            &[],
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
            &[],
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
                end_line: 1,
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
                end_line: 1,
            }],
            vec![],
        );

        insert_ir(&conn, "main", &graph_error);
        insert_ir(&conn, "main", &cli_db);
        insert_ir(&conn, "main", &cli_error);

        // seshat-graph/src/error.rs must have ZERO dependents —
        // seshat-cli/src/db.rs does NOT import from it.
        let result = query_dependencies(
            &conn,
            "main",
            "crates/seshat-graph/src/error.rs",
            QueryDependenciesOptions::default(),
        )
        .unwrap();
        assert!(
            result.dependents.is_empty(),
            "seshat-graph/src/error.rs must have no dependents; \
             crate::error in seshat-cli refers to seshat-cli/src/error.rs, not this file. \
             Got: {:?}",
            result.dependents
        );

        // seshat-cli/src/error.rs must have db.rs as a dependent.
        let result = query_dependencies(
            &conn,
            "main",
            "crates/seshat-cli/src/error.rs",
            QueryDependenciesOptions::default(),
        )
        .unwrap();
        assert!(
            result
                .dependents
                .iter()
                .any(|d| d.file_path.contains("db.rs")),
            "seshat-cli/src/error.rs must have db.rs as dependent. Got: {:?}",
            result.dependents
        );
    }

    // ── compute_transitive_dependents tests ──────────────────

    /// Build a synthetic reverse-adjacency map from a simple list of
    /// `(target, dependent)` pairs. `import_names` and `line` are not
    /// asserted by these tests; they are populated only on direct edges
    /// at the API level, so we leave them empty/`1`.
    fn reverse_from(edges: &[(&str, &str)]) -> HashMap<String, Vec<ReverseEdge>> {
        let mut map: HashMap<String, Vec<ReverseEdge>> = HashMap::new();
        for (target, from) in edges {
            map.entry((*target).to_owned())
                .or_default()
                .push(ReverseEdge {
                    from: (*from).to_owned(),
                    import_names: Vec::new(),
                    line: 1,
                });
        }
        map
    }

    #[test]
    fn transitive_depth_2_includes_2nd_order() {
        // chain: a is the seed; b imports a (direct), c imports b (transitive).
        let reverse = reverse_from(&[("a.rs", "b.rs"), ("b.rs", "c.rs")]);

        let result = compute_transitive_dependents("a.rs", &reverse, 2);

        assert!(!result.truncated);
        assert_eq!(result.entries.len(), 2);

        let direct = &result.entries[0];
        assert_eq!(direct.file_path, "b.rs");
        assert_eq!(direct.depth, 1);
        assert!(direct.via.is_empty());

        let transitive = &result.entries[1];
        assert_eq!(transitive.file_path, "c.rs");
        assert_eq!(transitive.depth, 2);
        assert_eq!(transitive.via, vec!["b.rs".to_owned()]);
        // Transitive entries do not carry import metadata.
        assert!(transitive.import_names.is_empty());
        assert_eq!(transitive.line, 0);
    }

    #[test]
    fn transitive_depth_3_includes_3rd_order() {
        // chain: a → b → c → d (b direct, c depth-2, d depth-3).
        let reverse = reverse_from(&[("a.rs", "b.rs"), ("b.rs", "c.rs"), ("c.rs", "d.rs")]);

        let result = compute_transitive_dependents("a.rs", &reverse, 3);

        assert!(!result.truncated);
        assert_eq!(result.entries.len(), 3);
        let depths: Vec<u32> = result.entries.iter().map(|e| e.depth).collect();
        assert_eq!(depths, vec![1, 2, 3]);
        let depth3 = result.entries.iter().find(|e| e.depth == 3).unwrap();
        assert_eq!(depth3.file_path, "d.rs");
        assert_eq!(depth3.via, vec!["b.rs".to_owned(), "c.rs".to_owned()]);
    }

    #[test]
    fn transitive_cycle_a_b_a_terminates() {
        // a → b, b → a (cycle). From target=a: only b is a real dependent;
        // a itself must not be re-enqueued.
        let reverse = reverse_from(&[("a.rs", "b.rs"), ("b.rs", "a.rs")]);

        let result = compute_transitive_dependents("a.rs", &reverse, 5);

        assert!(!result.truncated);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].file_path, "b.rs");
        assert_eq!(result.entries[0].depth, 1);
    }

    #[test]
    fn transitive_diamond_visits_each_node_once() {
        // Diamond: target=d has two direct dependents (b, c), each of
        // which is depended on by `a`. `a` must appear exactly once.
        let reverse = reverse_from(&[
            ("d.rs", "b.rs"),
            ("d.rs", "c.rs"),
            ("b.rs", "a.rs"),
            ("c.rs", "a.rs"),
        ]);

        let result = compute_transitive_dependents("d.rs", &reverse, 3);

        assert!(!result.truncated);

        let occurrences = result
            .entries
            .iter()
            .filter(|e| e.file_path == "a.rs")
            .count();
        assert_eq!(occurrences, 1, "diamond apex must be enumerated once");

        // Lex tie-break: `b.rs` < `c.rs`, so `a.rs` is reached via `b.rs`.
        let a_entry = result
            .entries
            .iter()
            .find(|e| e.file_path == "a.rs")
            .unwrap();
        assert_eq!(a_entry.depth, 2);
        assert_eq!(a_entry.via, vec!["b.rs".to_owned()]);
    }

    /// Deep-diamond: PRD Q2 says the tie-break for the apex of a
    /// diamond is "lex on the joined `via` string", NOT per-hop lex on
    /// each parent path. The two yield the same answer for shallow
    /// diamonds (the existing `transitive_diamond_*` test) — they
    /// diverge here because the lex order of the FIRST hop pushes one
    /// way and the lex order of the JOINED CHAIN pushes the other.
    ///
    /// Setup (target = `z.rs`):
    /// ```text
    /// z imported by [c, d]   ← both directs
    /// c imported by [b]      ← b is depth 2 via c
    /// d imported by [a]      ← a is depth 2 via d
    /// a imported by [t]      ← t reachable via [d, a]   joined "d.rs/a.rs"
    /// b imported by [t]      ← t reachable via [c, b]   joined "c.rs/b.rs"
    /// ```
    ///
    /// Per-hop lex on parent path at depth=3 would visit
    /// `Node(a, [d, a])` before `Node(b, [c, b])` (since "a" < "b") and
    /// award `t.via = [d.rs, a.rs]`. Joined-string lex picks
    /// `"c.rs/b.rs" < "d.rs/a.rs"` and awards `t.via = [c.rs, b.rs]`.
    /// Locking the latter freezes the PRD-mandated semantics.
    #[test]
    fn transitive_diamond_via_uses_joined_string_lex_tiebreak() {
        let reverse = reverse_from(&[
            ("z.rs", "c.rs"),
            ("z.rs", "d.rs"),
            ("c.rs", "b.rs"),
            ("d.rs", "a.rs"),
            ("a.rs", "t.rs"),
            ("b.rs", "t.rs"),
        ]);

        let result = compute_transitive_dependents("z.rs", &reverse, 3);
        assert!(!result.truncated);

        let occurrences = result
            .entries
            .iter()
            .filter(|e| e.file_path == "t.rs")
            .count();
        assert_eq!(occurrences, 1, "diamond apex must be enumerated once");

        let t_entry = result
            .entries
            .iter()
            .find(|e| e.file_path == "t.rs")
            .expect("t.rs must appear in the result");
        assert_eq!(t_entry.depth, 3);
        // Joined-string lex: "c.rs/b.rs" < "d.rs/a.rs" → [c, b] wins.
        // A regression to per-hop lex on parent path would put "a" first
        // (since "a" < "b") and award via=[d.rs, a.rs] instead.
        assert_eq!(
            t_entry.via,
            vec!["c.rs".to_owned(), "b.rs".to_owned()],
            "joined-string tie-break must prefer the chain whose joined string is lex-smallest"
        );
    }

    #[test]
    fn transitive_truncation_caps_at_max_dependents() {
        // 600 direct dependents, each lex-ordered to keep survivors stable.
        let edges: Vec<ReverseEdge> = (0..600)
            .map(|i| ReverseEdge {
                from: format!("dep_{i:04}.rs"),
                import_names: Vec::new(),
                line: 1,
            })
            .collect();
        let mut reverse: HashMap<String, Vec<ReverseEdge>> = HashMap::new();
        reverse.insert("target.rs".to_owned(), edges);

        let result = compute_transitive_dependents("target.rs", &reverse, 1);

        assert!(
            result.truncated,
            "expected truncation when directs exceed cap"
        );
        assert_eq!(result.entries.len(), MAX_DEPENDENTS);
        // All preserved entries are direct (depth 1) — directs are enumerated
        // before any transitive expansion would push them out.
        assert!(result.entries.iter().all(|e| e.depth == 1));
    }

    /// Internal callers that bypass [`validate_depth`] still get caught
    /// by the debug-mode assertion in [`compute_transitive_dependents`].
    /// In release builds the assertion is compiled out and the BFS
    /// runs to completion against an empty reverse map (so this test
    /// only fires on the panic path under `cfg(debug_assertions)`).
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "exceeds MAX_TRANSITIVE_DEPTH")]
    fn transitive_debug_assert_rejects_depth_above_max() {
        let reverse: HashMap<String, Vec<ReverseEdge>> = HashMap::new();
        let _ = compute_transitive_dependents("target.rs", &reverse, MAX_TRANSITIVE_DEPTH + 1);
    }

    /// BFS truncation (`MAX_DEPENDENTS` cap hit) must surface as
    /// `DependencyData.truncated == true` end-to-end. `compute_transitive_dependents`
    /// already sets the flag on `TransitiveResult`; this test locks the
    /// plumbing through `query_dependencies`. Without it, BFS truncation
    /// is silently dropped.
    #[test]
    fn query_dependencies_propagates_bfs_truncation_flag() {
        let conn = test_conn();
        let branch = "main";

        // Single target file with 600 distinct importers. 600 > MAX_DEPENDENTS=500,
        // so the BFS must truncate. We use depth=2 to force the BFS path
        // (depth=1 uses the legacy `build_dependents` route which has no cap).
        let target = make_file(
            "src/target.ts",
            vec![],
            vec![Export {
                name: "Target".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            vec![],
        );
        insert_ir(&conn, branch, &target);

        for i in 0..(MAX_DEPENDENTS + 100) {
            let importer = make_file(
                &format!("src/importer_{i:04}.ts"),
                vec![Import {
                    module: "./target".to_owned(),
                    names: vec!["Target".to_owned()],
                    is_type_only: false,
                    line: 1,
                }],
                vec![],
                vec![],
            );
            insert_ir(&conn, branch, &importer);
        }

        let data = query_dependencies(
            &conn,
            branch,
            "src/target.ts",
            QueryDependenciesOptions { depth: 2 },
        )
        .expect("query_dependencies should succeed");

        assert!(
            data.truncated,
            "DependencyData.truncated must reflect BFS truncation when MAX_DEPENDENTS is hit",
        );
        assert!(
            data.dependents.len() <= MAX_DEPENDENTS,
            "dependents list must not exceed MAX_DEPENDENTS after truncation"
        );
    }

    /// `depth == 0` is treated as "no expansion" at runtime (early-return
    /// with an empty result). The debug assertion does not fire on zero,
    /// since callers may pass it explicitly to short-circuit.
    #[test]
    fn transitive_depth_zero_returns_empty_result() {
        let mut reverse: HashMap<String, Vec<ReverseEdge>> = HashMap::new();
        reverse.insert(
            "target.rs".to_owned(),
            vec![ReverseEdge {
                from: "dep.rs".to_owned(),
                import_names: vec!["foo".to_owned()],
                line: 1,
            }],
        );

        let result = compute_transitive_dependents("target.rs", &reverse, 0);

        assert!(result.entries.is_empty());
        assert!(!result.truncated);
    }

    /// `build_reverse_adjacency` deduplicates `import_names` per
    /// (file, target) pair. Locks the dedup correctness — two imports
    /// of the same target naming overlapping symbols must collapse to
    /// a single sorted union, not produce duplicate names.
    #[test]
    fn build_reverse_adjacency_dedupes_import_names_within_file() {
        let target = make_file(
            "src/target.ts",
            vec![],
            vec![
                Export {
                    name: "Foo".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 1,
                    end_line: 1,
                },
                Export {
                    name: "Bar".to_owned(),
                    is_default: false,
                    is_type_only: false,
                    line: 2,
                    end_line: 2,
                },
            ],
            vec![],
        );
        // Importer pulls Foo + Bar in two distinct import statements
        // (e.g. one type-only + one runtime), and Foo is repeated
        // across both. The reverse edge must collapse to {Bar, Foo}
        // (sorted by BTreeSet) — no duplicate "Foo".
        let importer = make_file(
            "src/importer.ts",
            vec![
                Import {
                    module: "./target".to_owned(),
                    names: vec!["Foo".to_owned(), "Bar".to_owned()],
                    is_type_only: true,
                    line: 1,
                },
                Import {
                    module: "./target".to_owned(),
                    names: vec!["Foo".to_owned()],
                    is_type_only: false,
                    line: 5,
                },
            ],
            vec![],
            vec![],
        );

        let files = vec![target, importer];
        let known_paths: HashSet<String> = files
            .iter()
            .map(|f| f.path.to_string_lossy().to_string())
            .collect();
        let suffix_index = SuffixIndex::build(&known_paths);
        let internal_names: Vec<String> = Vec::new();
        let reverse = build_reverse_adjacency(&files, &internal_names, &suffix_index);

        let edges = reverse
            .get("src/target.ts")
            .expect("target should have at least one reverse edge");
        assert_eq!(edges.len(), 1, "single importer must produce a single edge");
        let edge = &edges[0];
        assert_eq!(edge.from, "src/importer.ts");
        // Sorted union (BTreeSet ordering): {Bar, Foo}.
        assert_eq!(
            edge.import_names,
            vec!["Bar".to_owned(), "Foo".to_owned()],
            "import_names must be the deduplicated union, sorted",
        );
        // Smallest line across all imports of this target wins.
        assert_eq!(edge.line, 1);
    }

    /// Invariant locked by this test: `query_dependencies(depth=1)`
    /// must agree with the depth-1 subset of `query_dependencies(depth=2)`.
    ///
    /// The two paths use different machinery internally — depth=1 takes
    /// the legacy `build_dependents` fast path, depth>=2 builds a
    /// reverse adjacency map via `build_reverse_adjacency` and runs the
    /// BFS over it. If their import-resolution semantics ever drift
    /// (e.g. one resolves a re-export the other doesn't), users see
    /// inconsistent results between calls that differ only in `depth`.
    /// This test catches that class of regression.
    #[test]
    fn query_dependencies_depth_1_agrees_with_depth_2_direct_subset() {
        let conn = test_conn();
        let branch = "main";

        // Two-level chain: a target (`target.ts`) imported by a direct
        // (`direct_a.ts`, `direct_b.ts`) and a transitive importer
        // (`indirect.ts` imports `direct_a`).
        let target = make_file(
            "src/target.ts",
            vec![],
            vec![Export {
                name: "Target".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            vec![],
        );
        let direct_a = make_file(
            "src/direct_a.ts",
            vec![Import {
                module: "./target".to_owned(),
                names: vec!["Target".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![Export {
                name: "WrappedA".to_owned(),
                is_default: false,
                is_type_only: false,
                line: 1,
                end_line: 1,
            }],
            vec![],
        );
        let direct_b = make_file(
            "src/direct_b.ts",
            vec![Import {
                module: "./target".to_owned(),
                names: vec!["Target".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![],
            vec![],
        );
        let indirect = make_file(
            "src/indirect.ts",
            vec![Import {
                module: "./direct_a".to_owned(),
                names: vec!["WrappedA".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            vec![],
            vec![],
        );
        for file in [&target, &direct_a, &direct_b, &indirect] {
            insert_ir(&conn, branch, file);
        }

        let depth_1 = query_dependencies(
            &conn,
            branch,
            "src/target.ts",
            QueryDependenciesOptions { depth: 1 },
        )
        .expect("depth=1 query");
        let depth_2 = query_dependencies(
            &conn,
            branch,
            "src/target.ts",
            QueryDependenciesOptions { depth: 2 },
        )
        .expect("depth=2 query");

        // Project both to (file_path, depth) tuples and compare the
        // depth-1 subsets. They must be identical sets.
        let mut depth_1_directs: Vec<&str> = depth_1
            .dependents
            .iter()
            .filter(|e| e.depth == 1)
            .map(|e| e.file_path.as_str())
            .collect();
        depth_1_directs.sort();
        let mut depth_2_directs: Vec<&str> = depth_2
            .dependents
            .iter()
            .filter(|e| e.depth == 1)
            .map(|e| e.file_path.as_str())
            .collect();
        depth_2_directs.sort();

        assert_eq!(
            depth_1_directs, depth_2_directs,
            "depth=1 result must equal the depth==1 subset of depth=2 result"
        );

        // Sanity: the fixture should produce both directs and at least
        // one transitive at depth=2, otherwise this test would pass
        // vacuously even if the resolvers diverged on richer inputs.
        assert!(
            depth_2.dependents.iter().any(|e| e.depth >= 2),
            "fixture must include at least one transitive (depth>=2) dependent"
        );
        assert!(
            depth_1_directs.contains(&"src/direct_a.ts")
                && depth_1_directs.contains(&"src/direct_b.ts"),
            "fixture must surface both direct importers, got: {depth_1_directs:?}"
        );
    }
}
