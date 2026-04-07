//! Dependency analysis over deserialized IR.
//!
//! Provides `query_dependencies()` which builds a dependency index from IR
//! and returns direct dependents/dependencies with blast radius for any file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;

use crate::code_pattern::load_branch_ir;
use crate::error::GraphError;

// ── Constants ────────────────────────────────────────────────

/// Blast radius thresholds.
const BLAST_RADIUS_MEDIUM_THRESHOLD: usize = 3;
const BLAST_RADIUS_HIGH_THRESHOLD: usize = 10;

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
    pub blast_radius: String,
    /// Exact number of direct dependents.
    pub blast_radius_count: usize,
    /// Backward compatibility note, present when dependents exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backward_compatibility_note: Option<String>,
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
    let files = load_branch_ir(conn, branch_id)?;

    // Build a set of known file paths for resolution.
    let known_paths: HashSet<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    // Verify the target file exists in IR.
    let target_normalized = normalize_path(trimmed);
    let target_file = files
        .iter()
        .find(|f| normalize_path(&f.path.to_string_lossy()) == target_normalized);

    let Some(target_file) = target_file else {
        return Err(GraphError::NodeNotFound(format!(
            "File not found in IR: {trimmed}"
        )));
    };
    let target_path_str = target_file.path.to_string_lossy().to_string();

    // Build dependencies: files the target imports from.
    let dependencies = build_dependencies(target_file, &known_paths);

    // Build dependents: files that import from the target.
    let dependents = build_dependents(&target_path_str, &files);

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
    let blast_radius_count = dependents.len();
    let blast_radius = classify_blast_radius(blast_radius_count);

    // Backward compatibility note.
    let backward_compatibility_note = if blast_radius_count > 0 {
        Some(format!(
            "This file has {} direct dependent(s). Changes to its public API may require updates in those files.",
            blast_radius_count
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
        blast_radius_count,
        backward_compatibility_note,
    })
}

// ── Internal helpers ─────────────────────────────────────────

/// Common file extensions to try when resolving import paths.
const FILE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".rs", ".py"];

/// Index/module files to try when an import resolves to a directory.
const INDEX_FILES: &[&str] = &["/index.ts", "/index.js", "/mod.rs"];

/// Convert a module path (e.g. `crate::foo::bar`) to a path suffix (`foo/bar`).
///
/// Replaces `::` and `.` separators with `/`, then strips leading `crate/`,
/// `super/`, or `self/` prefixes.
fn module_to_path_suffix(module: &str) -> String {
    let path_part = module.replace("::", "/").replace('.', "/");
    path_part
        .strip_prefix("crate/")
        .or_else(|| path_part.strip_prefix("super/"))
        .or_else(|| path_part.strip_prefix("self/"))
        .unwrap_or(&path_part)
        .to_owned()
}

/// Check if `haystack` ends with `suffix` at a path component boundary
/// (preceded by `/` or the suffix is the entire string).
fn suffix_matches_at_boundary(haystack: &str, suffix: &str) -> bool {
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
) -> Vec<DependencyEntry> {
    let target_dir = Path::new(&target_file.path)
        .parent()
        .unwrap_or_else(|| Path::new(""));

    let mut deps: HashMap<String, DependencyEntry> = HashMap::new();

    for import in &target_file.imports {
        let resolved_path = resolve_import(&import.module, target_dir, known_paths);

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
                if is_likely_internal(&import.module) {
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
fn is_likely_internal(module: &str) -> bool {
    module.starts_with('.') // covers ./ and ../
        || module == "crate" || module.starts_with("crate::")
        || module == "super" || module.starts_with("super::")
        || module == "self" || module.starts_with("self::")
        || module.starts_with("src/")
        || module.starts_with("src.")
}

/// Resolve an import module path to a known file path.
///
/// - Relative imports (starting with `.` or `..`) are resolved against the
///   importing file's directory.
/// - Absolute-looking internal imports (starting with `crate`, `self`, `super`)
///   are matched by suffix against known paths.
/// - External imports return None.
fn resolve_import(
    module: &str,
    importing_dir: &Path,
    known_paths: &HashSet<String>,
) -> Option<String> {
    if module.starts_with('.') {
        // Relative import — resolve against importing directory.
        resolve_relative_import(module, importing_dir, known_paths)
    } else if module.starts_with("crate")
        || module.starts_with("super")
        || module.starts_with("self")
    {
        // Rust-style internal import — match by suffix.
        resolve_by_suffix(module, known_paths)
    } else if module.starts_with("src/") || module.starts_with("src.") {
        // Python-style absolute internal import.
        resolve_by_suffix(module, known_paths)
    } else {
        // External import — exclude.
        None
    }
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

/// Resolve an import by matching its module path as a suffix of known paths.
fn resolve_by_suffix(module: &str, known_paths: &HashSet<String>) -> Option<String> {
    let suffix = module_to_path_suffix(module);

    // Try to find a known path that ends with this suffix.
    for known in known_paths {
        let known_normalized = known.replace('\\', "/");
        if known_normalized.ends_with(&suffix) {
            // Check that the match is at a path boundary.
            let before = known_normalized.len() - suffix.len();
            if before == 0
                || known_normalized.as_bytes().get(before.saturating_sub(1)) == Some(&b'/')
            {
                return Some(known.clone());
            }
        }

        // Also try with common extensions (full set, not a subset).
        for ext in FILE_EXTENSIONS {
            let suffix_ext = format!("{suffix}{ext}");
            if known_normalized.ends_with(&suffix_ext) {
                let before = known_normalized.len() - suffix_ext.len();
                if before == 0
                    || known_normalized.as_bytes().get(before.saturating_sub(1)) == Some(&b'/')
                {
                    return Some(known.clone());
                }
            }
        }
    }

    None
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
fn build_dependents(target_path: &str, files: &[seshat_core::ProjectFile]) -> Vec<DependentEntry> {
    let target_normalized = normalize_path(target_path);
    let target_stem = Path::new(target_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
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
                &target_stem,
                &target_name_no_ext,
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
    target_stem: &str,
    target_name_no_ext: &str,
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
    } else if is_likely_internal(module) {
        // Absolute-style internal import — check suffix match at path boundary.
        let suffix = module_to_path_suffix(module);

        // Check if the target ends with this suffix (with or without extension),
        // ensuring the match is at a path component boundary.
        suffix_matches_at_boundary(target_normalized, &suffix)
            || suffix_matches_at_boundary(target_name_no_ext, &suffix)
            || target_stem == suffix
    } else {
        false
    }
}

/// Classify blast radius based on number of dependents.
fn classify_blast_radius(count: usize) -> String {
    if count > BLAST_RADIUS_HIGH_THRESHOLD {
        "high".to_owned()
    } else if count >= BLAST_RADIUS_MEDIUM_THRESHOLD {
        "medium".to_owned()
    } else {
        "low".to_owned()
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
                end_line: 10,
                parameters: Vec::new(),
            }],
            types: vec![TypeDef {
                name: "Config".to_owned(),
                kind: TypeDefKind::Interface,
                is_public: true,
                line: 12,
            }],
            dependencies_used: deps_used,
            language_ir: LanguageIR::Rust(RustIR::default()),
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
        assert_eq!(classify_blast_radius(0), "low");
        assert_eq!(classify_blast_radius(1), "low");
        assert_eq!(classify_blast_radius(2), "low");
        assert_eq!(classify_blast_radius(3), "medium");
        assert_eq!(classify_blast_radius(10), "medium");
        assert_eq!(classify_blast_radius(11), "high");
        assert_eq!(classify_blast_radius(100), "high");
    }

    #[test]
    fn blast_radius_from_query() {
        let conn = test_conn();
        setup_project(&conn);

        // utils.ts has 2 dependents → low.
        let result = query_dependencies(&conn, "main", "src/utils.ts").unwrap();
        assert_eq!(result.blast_radius, "low");
        assert_eq!(result.blast_radius_count, result.dependents.len());

        // app.ts has 0 dependents → low.
        let result = query_dependencies(&conn, "main", "src/app.ts").unwrap();
        assert_eq!(result.blast_radius, "low");
        assert_eq!(result.blast_radius_count, 0);
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

    #[test]
    fn normalize_pathbuf_resolves_dots() {
        let result = normalize_pathbuf(Path::new("src/models/../utils"));
        assert_eq!(result, PathBuf::from("src/utils"));

        let result = normalize_pathbuf(Path::new("src/./utils"));
        assert_eq!(result, PathBuf::from("src/utils"));
    }
}
