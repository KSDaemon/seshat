//! Dependency usage detector — most-used library per domain.
//!
//! Analyzes [`DependencyUsage`] entries from parsed IR to identify which
//! library is most used for each functional domain (HTTP, logging, testing,
//! etc.) across the project.
//!
//! Produces one **Convention** finding per domain per file:
//! `"Canonical {domain} library: {package}"`. These are aggregated at the
//! project level by `query_project_context` to build the dependency summary.
//!
//! **Cross-file analysis:** The `detect_cross_file` method performs import
//! graph analysis to detect wrapper/facade patterns. When a single internal
//! module wraps an external dependency and most consumers use the wrapper
//! rather than the raw dependency, direct usage of the external dependency
//! is flagged as a convention violation.
//!
//! Domains are classified via a curated mapping of known crate/package names.
//! The detector supports all four languages (Rust, TypeScript, JavaScript,
//! Python).

use std::collections::{HashMap, HashSet};

use seshat_core::{
    CodeEvidence, ConventionFinding, DependencyDomain, DependencyUsage, KnowledgeNature, Language,
    ProjectFile, classify_domain,
};

use crate::trait_def::ConventionDetector;
use crate::usage_evidence::find_usage_evidence_for_file_scoped;

// ---------------------------------------------------------------------------
// Name-based heuristic classification
// ---------------------------------------------------------------------------

/// Keyword-to-domain mapping for heuristic classification of unrecognized
/// packages.
///
/// When a package is not in the known list, we check if its name contains
/// any of these keywords to infer a likely domain. Heuristic findings use
/// [`KnowledgeNature::Observation`] (never Convention) for lower confidence.
const HEURISTIC_DOMAIN_KEYWORDS: &[(&[&str], DependencyDomain)] = &[
    (&["test", "mock"], DependencyDomain::Testing),
    (&["log", "trace"], DependencyDomain::Logging),
    (&["http", "web", "api", "rest"], DependencyDomain::Http),
    (&["sql", "db", "orm"], DependencyDomain::Database),
    (&["cli", "command", "arg"], DependencyDomain::Cli),
    (
        &["serial", "json", "yaml", "proto"],
        DependencyDomain::Serialization,
    ),
    (&["valid", "schema"], DependencyDomain::Validation),
];

/// Attempt to classify an unrecognized package name by keyword heuristic.
///
/// Returns `None` if no keyword matches or if the package is already
/// classified by the known-library list.
///
/// Package names are split on `_` and `-` delimiters, then each component
/// is matched as a whole word against the keyword list. This prevents
/// substring false positives like `format` matching `orm` or
/// `ir_serialization` matching `serial`.
fn classify_heuristic_domain(package: &str, language: Language) -> Option<DependencyDomain> {
    // Skip if it's already a known package
    if classify_domain(package, language).is_some() {
        return None;
    }

    let lower = package.to_lowercase();

    for (keywords, domain) in HEURISTIC_DOMAIN_KEYWORDS {
        for kw in *keywords {
            // Word-boundary match: kw must start at a word boundary.
            // Boundaries: start of string, after _ or -, or at a camelCase
            // transition (lowercase → uppercase).  This prevents substring
            // false positives like "format" matching "orm".
            let mut search_start = 0usize;
            while let Some(pos) = lower[search_start..].find(kw) {
                let abs_pos = search_start + pos;
                let is_boundary = abs_pos == 0
                    || package
                        .as_bytes()
                        .get(abs_pos.wrapping_sub(1))
                        .is_none_or(|&b| b == b'_' || b == b'-')
                    || {
                        // camelCase word boundary: previous char is lowercase,
                        // current char (in original casing) is uppercase.
                        let prev_lower = package
                            .as_bytes()
                            .get(abs_pos.wrapping_sub(1))
                            .is_some_and(|&b| b.is_ascii_lowercase());
                        let curr_upper = package
                            .as_bytes()
                            .get(abs_pos)
                            .is_some_and(|&b| b.is_ascii_uppercase());
                        prev_lower && curr_upper
                    };
                if is_boundary {
                    return Some(*domain);
                }
                search_start = abs_pos + 1;
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Maximum call-site evidence entries per finding.
const MAX_EVIDENCE: usize = 5;

/// Detects the most-used library per functional domain.
///
/// Produces:
/// - **Convention** findings for the most-used library per domain in this file.
/// - **Observation** findings for heuristic domain classification of unknown packages.
///
/// Note: "conflicting library" and "dead dependency" observations have been
/// removed. Per-file conflicts are not meaningful (a file legitimately imports
/// both `sqlalchemy` and `alembic`). Dead dependency detection belongs to
/// static analysis tools (ruff, clippy, eslint), not seshat.
pub struct DependencyUsageDetector;

impl ConventionDetector for DependencyUsageDetector {
    fn name(&self) -> &'static str {
        "dependency_usage"
    }

    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
        if file.dependencies_used.is_empty() {
            return Vec::new();
        }

        let mut findings = Vec::new();

        // Group dependencies by domain.
        let mut domain_packages: HashMap<DependencyDomain, HashMap<&str, Vec<&DependencyUsage>>> =
            HashMap::new();

        for dep in &file.dependencies_used {
            if let Some(domain) = classify_domain(&dep.package, file.language) {
                domain_packages
                    .entry(domain)
                    .or_default()
                    .entry(&dep.package)
                    .or_default()
                    .push(dep);
            }
        }

        // For each domain, identify the canonical library and flag conflicts.
        for (domain, packages) in &domain_packages {
            let domain_name = domain.as_str();

            // Find the most-used package in this domain (by import count).
            let Some((canonical_pkg, _)) = packages.iter().max_by_key(|(_, usages)| usages.len())
            else {
                continue;
            };

            // Scope call-site evidence to only this dependency's imports.
            let call_sites =
                find_usage_evidence_for_file_scoped(file, &[canonical_pkg], MAX_EVIDENCE);

            // For Rust: also check derive macros (e.g. #[derive(Serialize)])
            // which don't appear as function calls but are real usage sites.
            let derive_evidence: Vec<CodeEvidence> =
                if let seshat_core::LanguageIR::Rust(ref ir) = file.language_ir {
                    let canonical_names: Vec<&str> = file
                        .imports
                        .iter()
                        .filter(|imp| {
                            let imp_top = imp.module.split("::").next().unwrap_or(&imp.module);
                            imp_top == *canonical_pkg
                        })
                        .flat_map(|imp| imp.names.iter().map(|n| n.as_str()))
                        .collect();

                    ir.derive_macros
                        .iter()
                        .filter(|d| {
                            d.derives
                                .iter()
                                .any(|dname| canonical_names.contains(&dname.as_str()))
                        })
                        .take(MAX_EVIDENCE)
                        .map(|d| CodeEvidence {
                            file: file.path.clone(),
                            line: d.line,
                            end_line: d.line,
                            snippet: String::new(),
                            snippet_start_line: 0,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

            let evidence: Vec<CodeEvidence> = if !call_sites.is_empty() {
                call_sites
            } else if !derive_evidence.is_empty() {
                derive_evidence
            } else {
                Vec::new()
            };

            // Convention: canonical library for this domain.
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "dependency_usage".to_owned(),
                nature: KnowledgeNature::Convention,
                description: format!("Canonical {domain_name} library: {canonical_pkg}",),
                evidence,
                follows_convention: true,
            });
        }

        // --- Heuristic domain classification for unrecognized packages ---
        // Only for packages not already classified by the known-library list.
        let classified_packages: HashSet<&str> = domain_packages
            .values()
            .flat_map(|pkgs| pkgs.keys().copied())
            .collect();

        for dep in &file.dependencies_used {
            if classified_packages.contains(dep.package.as_str()) {
                continue;
            }
            if let Some(heuristic_domain) = classify_heuristic_domain(&dep.package, file.language) {
                let heuristic_call_sites =
                    find_usage_evidence_for_file_scoped(file, &[&dep.package], MAX_EVIDENCE);
                let heuristic_evidence = if !heuristic_call_sites.is_empty() {
                    heuristic_call_sites
                } else {
                    Vec::new()
                };

                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "dependency_usage".to_owned(),
                    nature: KnowledgeNature::Observation,
                    description: format!(
                        "Likely {} library (heuristic): {}",
                        heuristic_domain.as_str(),
                        dep.package
                    ),
                    evidence: heuristic_evidence,
                    follows_convention: true,
                });
            }
        }

        findings
    }

    #[tracing::instrument(skip_all)]
    fn detect_cross_file(&self, files: &[ProjectFile]) -> Vec<ConventionFinding> {
        detect_wrapper_facades(files)
    }

    fn supported_languages(&self) -> &[Language] {
        Language::all()
    }
}

// ---------------------------------------------------------------------------
// Wrapper / facade detection (cross-file)
// ---------------------------------------------------------------------------

/// Returns `true` when `module` looks like a reference to a project-internal
/// module for the given language.
///
/// Heuristics (no hardcoded directory names):
/// - **Rust**: starts with `crate::`, `super::`, or `self::`
/// - **TypeScript / JavaScript**: starts with `./` or `../`
/// - **Python**: the module path, converted to a file path, matches at least
///   one file in the `project_files` set.
fn is_internal_import(module: &str, language: Language, project_files: &HashSet<&str>) -> bool {
    match language {
        Language::Rust => {
            module.starts_with("crate::")
                || module.starts_with("super::")
                || module.starts_with("self::")
        }
        Language::TypeScript | Language::JavaScript => {
            module.starts_with("./") || module.starts_with("../")
        }
        Language::Python => {
            // Relative imports (from . import X)
            if module.starts_with('.') {
                return true;
            }
            // Absolute internal: module path matches a project file when
            // converted to a path (e.g. "myapp.utils" → "myapp/utils.py"
            // or "myapp/utils/__init__.py").
            let as_path = module.replace('.', "/");
            project_files.iter().any(|fp| {
                let fp_no_ext = fp
                    .strip_suffix(".py")
                    .or_else(|| fp.strip_suffix("/__init__.py"))
                    .unwrap_or(fp);
                fp_no_ext == as_path || fp_no_ext.ends_with(&format!("/{as_path}"))
            })
        }
    }
}

/// Derive a canonical internal "module path" for a project file so that
/// other files' internal imports can be matched against it.
///
/// - **Rust**: `src/http/client.rs` → `crate::http::client`; `src/http/mod.rs` → `crate::http`
/// - **TS/JS**: relative path without extension, e.g. `src/utils/http.ts` → `src/utils/http`
/// - **Python**: `src/myapp/utils.py` → `myapp.utils`; `myapp/__init__.py` → `myapp`
fn file_module_path(file: &ProjectFile) -> Option<String> {
    let path_str = file.path.to_str()?;
    match file.language {
        Language::Rust => {
            let trimmed = path_str.strip_prefix("src/").unwrap_or(path_str);
            let without_ext = trimmed.strip_suffix(".rs")?;
            let module = if without_ext == "lib" || without_ext == "main" {
                "crate".to_owned()
            } else {
                let cleaned = without_ext.strip_suffix("/mod").unwrap_or(without_ext);
                format!("crate::{}", cleaned.replace('/', "::"))
            };
            Some(module)
        }
        Language::TypeScript | Language::JavaScript => {
            // Strip common extensions; keep the relative path for matching.
            let without_ext = path_str
                .strip_suffix(".ts")
                .or_else(|| path_str.strip_suffix(".tsx"))
                .or_else(|| path_str.strip_suffix(".js"))
                .or_else(|| path_str.strip_suffix(".jsx"))
                .or_else(|| path_str.strip_suffix(".mjs"))
                .or_else(|| path_str.strip_suffix(".cjs"))?;
            // Strip /index suffix so `src/utils/index` → `src/utils`
            let cleaned = without_ext.strip_suffix("/index").unwrap_or(without_ext);
            Some(cleaned.to_owned())
        }
        Language::Python => {
            let without_ext = path_str.strip_suffix(".py")?;
            let cleaned = without_ext.strip_suffix("/__init__").unwrap_or(without_ext);
            Some(cleaned.replace('/', "."))
        }
    }
}

/// Check whether `importer`'s internal import for `import_module` resolves
/// to `target_module_path` (the canonical module path of a potential wrapper).
fn import_resolves_to(
    import_module: &str,
    importer: &ProjectFile,
    target_module_path: &str,
    language: Language,
) -> bool {
    match language {
        Language::Rust => {
            // Direct match: `use crate::http::client;`
            if import_module == target_module_path
                || import_module.starts_with(&format!("{target_module_path}::"))
            {
                return true;
            }
            // super:: resolution: resolve relative to importer's parent module.
            if let Some(suffix) = import_module.strip_prefix("super::") {
                if let Some(parent) = file_module_path(importer) {
                    // Strip the last segment from parent to get grandparent.
                    if let Some(base) = parent.rsplit_once("::").map(|(b, _)| b) {
                        let resolved = format!("{base}::{suffix}");
                        if resolved == target_module_path
                            || resolved.starts_with(&format!("{target_module_path}::"))
                        {
                            return true;
                        }
                    }
                }
            }
            false
        }
        Language::TypeScript | Language::JavaScript => {
            // Resolve relative path against the importer's directory.
            if let Some(importer_dir) = importer.path.parent() {
                let resolved =
                    normalize_relative_path(&importer_dir.join(import_module).to_string_lossy());
                // Check exact match or with /index suffix stripped.
                resolved == target_module_path
                    || resolved.strip_suffix("/index").unwrap_or(&resolved) == target_module_path
                    || target_module_path
                        .strip_suffix("/index")
                        .unwrap_or(target_module_path)
                        == resolved
            } else {
                false
            }
        }
        Language::Python => {
            // Absolute import: "myapp.utils" == target "myapp.utils"
            if import_module == target_module_path
                || import_module.starts_with(&format!("{target_module_path}."))
            {
                return true;
            }
            // Relative imports starting with "." — resolve against importer's module.
            if import_module.starts_with('.') {
                if let Some(importer_mod) = file_module_path(importer) {
                    let dots = import_module.chars().take_while(|&c| c == '.').count();
                    let suffix = &import_module[dots..];
                    // Go up `dots - 1` levels from the importer's module.
                    let mut base = importer_mod.as_str();
                    for _ in 0..dots {
                        if let Some((parent, _)) = base.rsplit_once('.') {
                            base = parent;
                        } else {
                            return false;
                        }
                    }
                    let resolved = if suffix.is_empty() {
                        base.to_owned()
                    } else {
                        format!("{base}.{suffix}")
                    };
                    return resolved == target_module_path
                        || resolved.starts_with(&format!("{target_module_path}."));
                }
            }
            false
        }
    }
}

/// Normalize a relative path by resolving `..` and `.` components.
fn normalize_relative_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

/// Core wrapper/facade detection algorithm.
///
/// For each external dependency used across the project:
/// 1. Find all files that import it directly ("direct importers").
/// 2. Among those, find "wrapper candidates" — files that import the external
///    dep AND are themselves imported by other project files.
/// 3. Count how many files consume the wrapper vs. use the external dep
///    directly.
/// 4. If wrapper consumers > direct users (excluding the wrapper) — i.e.
///    the majority uses the wrapper — establish a wrapper convention.
/// 5. Files that import the external dep directly (but are NOT the wrapper)
///    are flagged as convention violations.
#[tracing::instrument(skip_all, fields(file_count = files.len()))]
fn detect_wrapper_facades(files: &[ProjectFile]) -> Vec<ConventionFinding> {
    if files.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();

    // Build a set of project file path strings for Python internal import
    // resolution.
    let project_file_paths: HashSet<&str> = files.iter().filter_map(|f| f.path.to_str()).collect();

    // Build module path → file index mapping.
    let mut module_to_file_idx: HashMap<String, usize> = HashMap::new();
    for (idx, file) in files.iter().enumerate() {
        if let Some(mod_path) = file_module_path(file) {
            module_to_file_idx.insert(mod_path, idx);
        }
    }

    // Step 1: Identify all external dependencies and which files import them.
    // external_dep → set of file indices that import it directly.
    let mut ext_dep_importers: HashMap<&str, HashSet<usize>> = HashMap::new();
    for (idx, file) in files.iter().enumerate() {
        for imp in &file.imports {
            if !is_internal_import(&imp.module, file.language, &project_file_paths) {
                ext_dep_importers
                    .entry(root_package(&imp.module, file.language))
                    .or_default()
                    .insert(idx);
            }
        }
    }

    // Step 2: Build internal import graph — who imports whom.
    // file_idx → set of file indices that import this file.
    let mut consumers_of: HashMap<usize, HashSet<usize>> = HashMap::new();
    for (idx, file) in files.iter().enumerate() {
        for imp in &file.imports {
            if is_internal_import(&imp.module, file.language, &project_file_paths) {
                // Resolve which project file this import points to.
                for (mod_path, &target_idx) in &module_to_file_idx {
                    if import_resolves_to(&imp.module, file, mod_path, file.language) {
                        consumers_of.entry(target_idx).or_default().insert(idx);
                    }
                }
            }
        }
    }

    // Step 3: For each external dep, check for wrapper pattern.
    for (ext_dep, direct_importers) in &ext_dep_importers {
        if direct_importers.len() < 2 {
            // Need at least 2 files touching this dep to detect a wrapper pattern.
            continue;
        }

        // Find wrapper candidates: files that import the ext dep AND are
        // consumed by at least one other file.
        let mut best_wrapper: Option<(usize, usize)> = None; // (file_idx, consumer_count)
        for &file_idx in direct_importers {
            if let Some(consumers) = consumers_of.get(&file_idx) {
                let consumer_count = consumers.len();
                if consumer_count > 0 && best_wrapper.is_none_or(|(_, best)| consumer_count > best)
                {
                    best_wrapper = Some((file_idx, consumer_count));
                }
            }
        }

        let Some((wrapper_idx, wrapper_consumer_count)) = best_wrapper else {
            continue; // No file wraps this dependency.
        };

        // Direct users = files that import the ext dep directly, excluding the wrapper.
        let direct_user_count = direct_importers.len() - 1; // subtract wrapper itself

        // Convention established: wrapper consumers > direct users (>50% use wrapper).
        if wrapper_consumer_count <= direct_user_count {
            continue;
        }

        let wrapper_file = &files[wrapper_idx];
        let wrapper_path = wrapper_file.path.display().to_string();

        // Emit convention finding for the wrapper itself.
        findings.push(ConventionFinding {
            file_path: wrapper_file.path.clone(),
            detector_name: "dependency_usage".to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Wrapper module for {ext_dep}: {wrapper_path}",),
            evidence: wrapper_file
                .imports
                .iter()
                .filter(|imp| root_package(&imp.module, wrapper_file.language) == *ext_dep)
                .take(3)
                .map(|imp| CodeEvidence {
                    file: wrapper_file.path.clone(),
                    line: imp.line,
                    end_line: imp.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                })
                .collect(),
            follows_convention: true,
        });

        // Emit violation findings for direct users that bypass the wrapper.
        for &file_idx in direct_importers {
            if file_idx == wrapper_idx {
                continue; // Wrapper itself is not a violator.
            }

            let violating_file = &files[file_idx];
            let violating_imports: Vec<&seshat_core::ir::Import> = violating_file
                .imports
                .iter()
                .filter(|imp| root_package(&imp.module, violating_file.language) == *ext_dep)
                .collect();

            if violating_imports.is_empty() {
                continue;
            }

            findings.push(ConventionFinding {
                file_path: violating_file.path.clone(),
                detector_name: "dependency_usage".to_owned(),
                nature: KnowledgeNature::Convention,
                description: format!("Use {wrapper_path} for {ext_dep} operations",),
                evidence: violating_imports
                    .iter()
                    .take(3)
                    .map(|imp| CodeEvidence {
                        file: violating_file.path.clone(),
                        line: imp.line,
                        end_line: imp.line,
                        snippet: String::new(),
                        snippet_start_line: 0,
                    })
                    .collect(),
                follows_convention: false,
            });
        }
    }

    findings
}

/// Extract the root package name from an import module path.
///
/// - Rust: `"reqwest::Client"` → `"reqwest"`, `"tokio::fs"` → `"tokio"`
/// - JS/TS: `"express"` → `"express"`, `"@prisma/client"` → `"@prisma/client"`
/// - Python: `"flask.Flask"` → `"flask"`, `"sqlalchemy.orm"` → `"sqlalchemy"`
fn root_package(module: &str, language: Language) -> &str {
    match language {
        Language::Rust => module.split("::").next().unwrap_or(module),
        Language::TypeScript | Language::JavaScript => {
            if module.starts_with('@') {
                // Scoped package: @scope/name
                match module.find('/') {
                    Some(slash_pos) => match module[slash_pos + 1..].find('/') {
                        Some(second_slash) => &module[..slash_pos + 1 + second_slash],
                        None => module,
                    },
                    None => module,
                }
            } else {
                module.split('/').next().unwrap_or(module)
            }
        }
        Language::Python => module.split('.').next().unwrap_or(module),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::{Import, LanguageIR};
    use seshat_core::{Language, RustIR, TypeScriptIR};
    use std::path::{Path, PathBuf};

    fn make_rust_file_with_deps(deps: Vec<DependencyUsage>, imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: deps,
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    fn make_ts_file_with_deps(deps: Vec<DependencyUsage>, imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/index.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: deps,
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        }
    }

    fn dep(package: &str, import_path: &str, line: usize) -> DependencyUsage {
        DependencyUsage {
            package: package.to_owned(),
            import_path: import_path.to_owned(),
            line,
        }
    }

    fn import(module: &str, names: &[&str]) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| (*s).to_owned()).collect(),
            is_type_only: false,
            line: 1,
        }
    }

    #[test]
    fn detector_name() {
        let detector = DependencyUsageDetector;
        assert_eq!(detector.name(), "dependency_usage");
    }

    #[test]
    fn supports_all_languages() {
        let detector = DependencyUsageDetector;
        assert_eq!(detector.supported_languages(), Language::all());
    }

    #[test]
    fn empty_dependencies_produces_no_findings() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    #[test]
    fn single_rust_http_library_is_canonical() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("reqwest", "reqwest::get", 10),
            ],
            vec![import("reqwest", &["Client", "get"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should have a convention finding");
        assert!(convention.description.contains("reqwest"));
        assert!(convention.description.contains("HTTP"));
        assert!(convention.follows_convention);
    }

    #[test]
    fn two_http_libs_in_same_file_no_conflict_observation() {
        // Importing reqwest + hyper in the same file is valid — no conflict observation
        // should be produced. Only Convention findings (one per domain) are emitted.
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("hyper", "hyper::Server", 10),
            ],
            vec![import("reqwest", &["Client"]), import("hyper", &["Server"])],
        );
        let findings = detector.detect(&file);

        let conflicts: Vec<_> = findings
            .iter()
            .filter(|f| f.description.contains("Conflicting"))
            .collect();
        assert!(
            conflicts.is_empty(),
            "no Conflicting observations should be produced"
        );

        // Still produces exactly one Convention for the HTTP domain (the most-used one)
        let http_conventions: Vec<_> = findings
            .iter()
            .filter(|f| f.nature == KnowledgeNature::Convention && f.description.contains("HTTP"))
            .collect();
        assert_eq!(http_conventions.len(), 1);
    }

    #[test]
    fn canonical_is_most_used_in_domain() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("reqwest", "reqwest::get", 10),
                dep("reqwest", "reqwest::Url", 15),
                dep("hyper", "hyper::Server", 20),
            ],
            vec![
                import("reqwest", &["Client", "get", "Url"]),
                import("hyper", &["Server"]),
            ],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("HTTP"))
            .expect("should have HTTP convention");
        assert!(
            convention.description.contains("reqwest"),
            "reqwest should be canonical (3 usages vs 1)"
        );
    }

    #[test]
    fn typescript_testing_library_detected() {
        let detector = DependencyUsageDetector;
        let file = make_ts_file_with_deps(
            vec![dep("jest", "jest", 1), dep("jest", "jest", 5)],
            vec![import("jest", &["describe", "it"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("testing"))
            .expect("should detect jest as testing library");
        assert!(convention.description.contains("jest"));
    }

    #[test]
    fn no_dead_dependency_observations_produced() {
        // Dead dependency detection is out of scope for seshat — belongs to
        // linters (clippy, ruff, eslint). Verify no such findings are emitted.
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![dep("serde", "serde::Serialize", 1)],
            Vec::new(), // No imports at all
        );
        let findings = detector.detect(&file);

        let dead: Vec<_> = findings
            .iter()
            .filter(|f| {
                f.description.contains("dead dependency") || f.description.contains("not imported")
            })
            .collect();
        assert!(
            dead.is_empty(),
            "no dead dependency observations should be produced"
        );
    }

    #[test]
    fn multiple_domains_detected_independently() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 5),
                dep("tracing", "tracing::info", 10),
                dep("clap", "clap::Parser", 15),
            ],
            vec![
                import("reqwest", &["Client"]),
                import("tracing", &["info"]),
                import("clap", &["Parser"]),
            ],
        );
        let findings = detector.detect(&file);

        let conventions: Vec<&ConventionFinding> = findings
            .iter()
            .filter(|f| f.nature == KnowledgeNature::Convention)
            .collect();

        assert_eq!(conventions.len(), 3, "HTTP, logging, CLI");
        assert!(conventions.iter().any(|f| f.description.contains("HTTP")));
        assert!(
            conventions
                .iter()
                .any(|f| f.description.contains("logging"))
        );
        assert!(conventions.iter().any(|f| f.description.contains("CLI")));
    }

    #[test]
    fn unknown_package_produces_no_domain_finding() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![dep("my-internal-crate", "my_internal_crate::Foo", 1)],
            vec![import("my-internal-crate", &["Foo"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention);
        assert!(
            convention.is_none(),
            "unknown packages should not produce domain findings"
        );
    }

    #[test]
    fn python_domain_classification() {
        let detector = DependencyUsageDetector;
        let file = ProjectFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            content_hash: String::new(),
            imports: vec![import("fastapi", &["FastAPI"])],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: vec![dep("fastapi", "fastapi.FastAPI", 1)],
            language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
            file_doc: None,
        };
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should detect fastapi as web framework");
        assert!(convention.description.contains("fastapi"));
        assert!(convention.description.contains("web framework"));
    }

    #[test]
    fn javascript_domain_classification() {
        let detector = DependencyUsageDetector;
        let file = ProjectFile {
            path: PathBuf::from("server.js"),
            language: Language::JavaScript,
            content_hash: String::new(),
            imports: vec![import("express", &["express"])],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: vec![dep("express", "express", 1)],
            language_ir: LanguageIR::JavaScript(seshat_core::JavaScriptIR::default()),
            file_doc: None,
        };
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should detect express as web framework");
        assert!(convention.description.contains("express"));
        assert!(convention.description.contains("web framework"));
    }

    #[test]
    fn evidence_includes_import_paths() {
        // When call_sites and derive_evidence are both empty, the evidence
        // fallback returns Vec::new() — empty evidence is acceptable for
        // file-level conventions.  The convention finding still exists with
        // the correct description.
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![
                dep("tracing", "tracing::info", 5),
                dep("tracing", "tracing::warn", 10),
            ],
            vec![import("tracing", &["info", "warn"])],
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should have logging convention");
        assert!(convention.description.contains("Canonical"));
        assert!(convention.description.contains("tracing"));
    }

    // --- Domain classification unit tests ---

    #[test]
    fn classify_domain_rust_http() {
        assert_eq!(
            classify_domain("reqwest", Language::Rust),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_domain("axum", Language::Rust),
            Some(DependencyDomain::WebFramework)
        );
    }

    #[test]
    fn classify_domain_rust_logging() {
        assert_eq!(
            classify_domain("tracing", Language::Rust),
            Some(DependencyDomain::Logging)
        );
        assert_eq!(
            classify_domain("log", Language::Rust),
            Some(DependencyDomain::Logging)
        );
    }

    #[test]
    fn classify_domain_ts_testing() {
        assert_eq!(
            classify_domain("jest", Language::TypeScript),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_domain("vitest", Language::TypeScript),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn classify_domain_python_database() {
        assert_eq!(
            classify_domain("sqlalchemy", Language::Python),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_domain("asyncpg", Language::Python),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn classify_domain_unknown_returns_none() {
        assert_eq!(classify_domain("my-custom-lib", Language::Rust), None);
        assert_eq!(
            classify_domain("internal-utils", Language::TypeScript),
            None
        );
        assert_eq!(classify_domain("my_app", Language::Python), None);
    }

    // -----------------------------------------------------------------------
    // Wrapper / facade detection tests (cross-file)
    // -----------------------------------------------------------------------

    fn make_python_file(path: &str, imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Python,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
            file_doc: None,
        }
    }

    fn make_ts_file_at(path: &str, imports: Vec<Import>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        }
    }

    /// Python wrapper: `myapp/http_client.py` wraps `requests`.
    /// 5 consumers use the wrapper, 2 files import `requests` directly.
    /// → 2 violation findings.
    #[test]
    fn python_wrapper_pattern_detected() {
        let wrapper = make_python_file(
            "myapp/http_client.py",
            vec![import("requests", &["get", "post"])],
        );
        // 5 consumer files that import the wrapper (internal import).
        let consumer1 = make_python_file(
            "myapp/service_a.py",
            vec![import("myapp.http_client", &["get"])],
        );
        let consumer2 = make_python_file(
            "myapp/service_b.py",
            vec![import("myapp.http_client", &["post"])],
        );
        let consumer3 = make_python_file(
            "myapp/service_c.py",
            vec![import("myapp.http_client", &["get"])],
        );
        let consumer4 = make_python_file(
            "myapp/service_d.py",
            vec![import("myapp.http_client", &["post"])],
        );
        let consumer5 = make_python_file(
            "myapp/service_e.py",
            vec![import("myapp.http_client", &["get"])],
        );
        // 2 direct importers that bypass the wrapper.
        let direct1 = make_python_file("myapp/legacy_a.py", vec![import("requests", &["get"])]);
        let direct2 = make_python_file("myapp/legacy_b.py", vec![import("requests", &["post"])]);

        let files = vec![
            wrapper, consumer1, consumer2, consumer3, consumer4, consumer5, direct1, direct2,
        ];
        let findings = detect_wrapper_facades(&files);

        // Should have: 1 wrapper convention + 2 violation findings.
        let wrapper_finding = findings
            .iter()
            .find(|f| f.follows_convention && f.description.contains("Wrapper module"))
            .expect("should detect wrapper module");
        assert!(wrapper_finding.description.contains("requests"));
        assert!(wrapper_finding.description.contains("http_client"));

        let violations: Vec<&ConventionFinding> = findings
            .iter()
            .filter(|f| !f.follows_convention && f.description.contains("requests"))
            .collect();
        assert_eq!(violations.len(), 2, "should flag 2 direct users");
        assert!(
            violations
                .iter()
                .any(|v| v.file_path.as_path() == Path::new("myapp/legacy_a.py"))
        );
        assert!(
            violations
                .iter()
                .any(|v| v.file_path.as_path() == Path::new("myapp/legacy_b.py"))
        );
    }

    /// TypeScript wrapper: `src/lib/http.ts` wraps `axios`.
    /// 4 consumers use the wrapper, 1 file imports `axios` directly.
    /// → 1 violation finding.
    #[test]
    fn typescript_wrapper_pattern_detected() {
        let wrapper = make_ts_file_at("src/lib/http.ts", vec![import("axios", &["default"])]);
        let consumer1 = make_ts_file_at(
            "src/features/users.ts",
            vec![import("../lib/http", &["get"])],
        );
        let consumer2 = make_ts_file_at(
            "src/features/orders.ts",
            vec![import("../lib/http", &["post"])],
        );
        let consumer3 = make_ts_file_at(
            "src/features/products.ts",
            vec![import("../lib/http", &["get"])],
        );
        let consumer4 = make_ts_file_at(
            "src/features/auth.ts",
            vec![import("../lib/http", &["post"])],
        );
        // 1 direct importer that bypasses the wrapper.
        let direct = make_ts_file_at(
            "src/features/legacy.ts",
            vec![import("axios", &["default"])],
        );

        let files = vec![wrapper, consumer1, consumer2, consumer3, consumer4, direct];
        let findings = detect_wrapper_facades(&files);

        let wrapper_finding = findings
            .iter()
            .find(|f| f.follows_convention && f.description.contains("Wrapper module"))
            .expect("should detect wrapper module");
        assert!(wrapper_finding.description.contains("axios"));
        assert!(wrapper_finding.description.contains("http"));

        let violations: Vec<&ConventionFinding> = findings
            .iter()
            .filter(|f| !f.follows_convention && f.description.contains("axios"))
            .collect();
        assert_eq!(violations.len(), 1, "should flag 1 direct user");
        assert_eq!(
            violations[0].file_path.as_path(),
            Path::new("src/features/legacy.ts")
        );
    }

    /// No wrapper exists — all files import the external dep directly.
    /// → No wrapper convention detected.
    #[test]
    fn no_wrapper_no_convention() {
        let file1 = make_python_file("myapp/service_a.py", vec![import("requests", &["get"])]);
        let file2 = make_python_file("myapp/service_b.py", vec![import("requests", &["post"])]);
        let file3 = make_python_file("myapp/service_c.py", vec![import("requests", &["get"])]);

        let files = vec![file1, file2, file3];
        let findings = detect_wrapper_facades(&files);

        // No file is imported by others, so no wrapper detected.
        assert!(
            findings.is_empty(),
            "should have no wrapper findings when no file wraps the dependency"
        );
    }

    /// Wrapper used by minority (<50%) → no convention established.
    /// Wrapper has 2 consumers, but 3 files import directly.
    #[test]
    fn wrapper_minority_no_convention() {
        let wrapper = make_python_file("myapp/http_client.py", vec![import("requests", &["get"])]);
        // 2 consumers use the wrapper.
        let consumer1 = make_python_file(
            "myapp/service_a.py",
            vec![import("myapp.http_client", &["get"])],
        );
        let consumer2 = make_python_file(
            "myapp/service_b.py",
            vec![import("myapp.http_client", &["post"])],
        );
        // 3 direct importers (majority).
        let direct1 = make_python_file("myapp/direct_a.py", vec![import("requests", &["get"])]);
        let direct2 = make_python_file("myapp/direct_b.py", vec![import("requests", &["post"])]);
        let direct3 = make_python_file("myapp/direct_c.py", vec![import("requests", &["get"])]);

        let files = vec![wrapper, consumer1, consumer2, direct1, direct2, direct3];
        let findings = detect_wrapper_facades(&files);

        // wrapper_consumer_count=2, direct_user_count=3 (4 direct importers - 1 wrapper)
        // 2 <= 3 → no convention established
        assert!(
            findings.is_empty(),
            "should not establish wrapper convention when minority uses wrapper"
        );
    }

    /// Single file uses a dependency → no wrapper detection possible.
    #[test]
    fn single_file_no_wrapper_detection() {
        let file = make_python_file("myapp/app.py", vec![import("requests", &["get"])]);

        let files = vec![file];
        let findings = detect_wrapper_facades(&files);
        assert!(findings.is_empty());
    }

    /// Wrapper file itself is NOT flagged as violating the convention.
    #[test]
    fn wrapper_file_not_flagged_as_violator() {
        let wrapper = make_ts_file_at("src/lib/http.ts", vec![import("axios", &["default"])]);
        let consumer1 = make_ts_file_at("src/a.ts", vec![import("./lib/http", &["get"])]);
        let consumer2 = make_ts_file_at("src/b.ts", vec![import("./lib/http", &["post"])]);
        let consumer3 = make_ts_file_at("src/c.ts", vec![import("./lib/http", &["get"])]);

        let files = vec![wrapper, consumer1, consumer2, consumer3];
        let findings = detect_wrapper_facades(&files);

        // No violations — only the wrapper imports axios, and it's not flagged.
        let violations: Vec<&ConventionFinding> =
            findings.iter().filter(|f| !f.follows_convention).collect();
        assert!(
            violations.is_empty(),
            "wrapper file should not be flagged as violator"
        );
    }

    // --- Helper function unit tests ---

    #[test]
    fn root_package_rust() {
        assert_eq!(root_package("reqwest::Client", Language::Rust), "reqwest");
        assert_eq!(root_package("tokio::fs", Language::Rust), "tokio");
        assert_eq!(root_package("serde", Language::Rust), "serde");
    }

    #[test]
    fn root_package_js_ts() {
        assert_eq!(root_package("express", Language::JavaScript), "express");
        assert_eq!(
            root_package("@prisma/client", Language::TypeScript),
            "@prisma/client"
        );
        assert_eq!(
            root_package("@prisma/client/runtime", Language::TypeScript),
            "@prisma/client"
        );
        assert_eq!(root_package("axios", Language::TypeScript), "axios");
    }

    #[test]
    fn root_package_python() {
        assert_eq!(root_package("flask.Flask", Language::Python), "flask");
        assert_eq!(
            root_package("sqlalchemy.orm", Language::Python),
            "sqlalchemy"
        );
        assert_eq!(root_package("requests", Language::Python), "requests");
    }

    #[test]
    fn is_internal_rust() {
        let empty: HashSet<&str> = HashSet::new();
        assert!(is_internal_import("crate::utils", Language::Rust, &empty));
        assert!(is_internal_import("super::config", Language::Rust, &empty));
        assert!(is_internal_import("self::helpers", Language::Rust, &empty));
        assert!(!is_internal_import("reqwest", Language::Rust, &empty));
        assert!(!is_internal_import("tokio::fs", Language::Rust, &empty));
    }

    #[test]
    fn is_internal_ts_js() {
        let empty: HashSet<&str> = HashSet::new();
        assert!(is_internal_import("./utils", Language::TypeScript, &empty));
        assert!(is_internal_import(
            "../config",
            Language::JavaScript,
            &empty
        ));
        assert!(!is_internal_import("express", Language::JavaScript, &empty));
        assert!(!is_internal_import("axios", Language::TypeScript, &empty));
    }

    #[test]
    fn is_internal_python() {
        let project_files: HashSet<&str> =
            ["myapp/utils.py", "myapp/__init__.py", "myapp/core/db.py"]
                .iter()
                .copied()
                .collect();
        assert!(is_internal_import(
            "myapp.utils",
            Language::Python,
            &project_files
        ));
        assert!(is_internal_import(
            ".utils",
            Language::Python,
            &project_files
        ));
        assert!(!is_internal_import(
            "requests",
            Language::Python,
            &project_files
        ));
    }

    #[test]
    fn file_module_path_rust() {
        let file = ProjectFile {
            path: PathBuf::from("src/http/client.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };
        assert_eq!(
            file_module_path(&file),
            Some("crate::http::client".to_owned())
        );

        let mod_file = ProjectFile {
            path: PathBuf::from("src/http/mod.rs"),
            language: Language::Rust,
            ..file.clone()
        };
        assert_eq!(file_module_path(&mod_file), Some("crate::http".to_owned()));
    }

    #[test]
    fn file_module_path_typescript() {
        let file = ProjectFile {
            path: PathBuf::from("src/lib/http.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        };
        assert_eq!(file_module_path(&file), Some("src/lib/http".to_owned()));

        let index_file = ProjectFile {
            path: PathBuf::from("src/utils/index.ts"),
            language: Language::TypeScript,
            ..file.clone()
        };
        assert_eq!(file_module_path(&index_file), Some("src/utils".to_owned()));
    }

    #[test]
    fn file_module_path_python() {
        let file = ProjectFile {
            path: PathBuf::from("myapp/http_client.py"),
            language: Language::Python,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
            file_doc: None,
        };
        assert_eq!(
            file_module_path(&file),
            Some("myapp.http_client".to_owned())
        );

        let init_file = ProjectFile {
            path: PathBuf::from("myapp/__init__.py"),
            language: Language::Python,
            ..file.clone()
        };
        assert_eq!(file_module_path(&init_file), Some("myapp".to_owned()));
    }

    #[test]
    fn normalize_path_resolves_dot_dot() {
        assert_eq!(
            normalize_relative_path("src/features/../lib/http"),
            "src/lib/http"
        );
        assert_eq!(normalize_relative_path("a/b/c/../../d"), "a/d");
        assert_eq!(normalize_relative_path("./a/b"), "a/b");
    }

    #[test]
    fn cross_file_empty_files() {
        let findings = detect_wrapper_facades(&[]);
        assert!(findings.is_empty());
    }

    // -----------------------------------------------------------------------
    // Heuristic domain classification tests (US-011)
    // -----------------------------------------------------------------------

    #[test]
    fn heuristic_test_dep_by_name() {
        assert_eq!(
            classify_heuristic_domain("my-test-utils", Language::Rust),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_heuristic_domain("mockserver", Language::Python),
            Some(DependencyDomain::Testing)
        );
    }

    #[test]
    fn heuristic_log_dep_by_name() {
        assert_eq!(
            classify_heuristic_domain("custom-logger", Language::TypeScript),
            Some(DependencyDomain::Logging)
        );
        assert_eq!(
            classify_heuristic_domain("my-trace-lib", Language::Rust),
            Some(DependencyDomain::Logging)
        );
    }

    #[test]
    fn heuristic_http_dep_by_name() {
        assert_eq!(
            classify_heuristic_domain("my-http-client", Language::Python),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_heuristic_domain("web-utils", Language::TypeScript),
            Some(DependencyDomain::Http)
        );
        assert_eq!(
            classify_heuristic_domain("rest-api-sdk", Language::JavaScript),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn heuristic_db_dep_by_name() {
        assert_eq!(
            classify_heuristic_domain("my-sql-driver", Language::Rust),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_heuristic_domain("db-connector", Language::Python),
            Some(DependencyDomain::Database)
        );
        assert_eq!(
            classify_heuristic_domain("simple-orm", Language::TypeScript),
            Some(DependencyDomain::Database)
        );
    }

    #[test]
    fn heuristic_cli_dep_by_name() {
        assert_eq!(
            classify_heuristic_domain("my-cli-tool", Language::Rust),
            Some(DependencyDomain::Cli)
        );
        assert_eq!(
            classify_heuristic_domain("command-parser", Language::Python),
            Some(DependencyDomain::Cli)
        );
    }

    #[test]
    fn heuristic_serialization_dep_by_name() {
        assert_eq!(
            classify_heuristic_domain("json-schema-validator", Language::TypeScript),
            Some(DependencyDomain::Serialization)
        );
        assert_eq!(
            classify_heuristic_domain("yaml-parser", Language::Python),
            Some(DependencyDomain::Serialization)
        );
        assert_eq!(
            classify_heuristic_domain("proto-gen", Language::Rust),
            Some(DependencyDomain::Serialization)
        );
    }

    #[test]
    fn heuristic_validation_dep_by_name() {
        assert_eq!(
            classify_heuristic_domain("my-validator", Language::Python),
            Some(DependencyDomain::Validation)
        );
        assert_eq!(
            classify_heuristic_domain("schema-utils", Language::TypeScript),
            Some(DependencyDomain::Validation)
        );
    }

    #[test]
    fn heuristic_known_package_returns_none() {
        // Known packages should NOT be classified by heuristic
        assert_eq!(classify_heuristic_domain("reqwest", Language::Rust), None);
        assert_eq!(
            classify_heuristic_domain("jest", Language::TypeScript),
            None
        );
        assert_eq!(classify_heuristic_domain("pytest", Language::Python), None);
        assert_eq!(classify_heuristic_domain("tracing", Language::Rust), None);
    }

    #[test]
    fn heuristic_unrelated_package_returns_none() {
        assert_eq!(
            classify_heuristic_domain("lodash", Language::TypeScript),
            None
        );
        assert_eq!(
            classify_heuristic_domain("my-custom-lib", Language::Rust),
            None
        );
        assert_eq!(classify_heuristic_domain("utils", Language::Python), None);
    }

    #[test]
    fn heuristic_finding_uses_observation_nature() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![dep("my-test-helper", "my_test_helper::setup", 5)],
            vec![import("my_test_helper", &["setup"])],
        );
        let findings = detector.detect(&file);

        let heuristic = findings
            .iter()
            .find(|f| f.description.contains("heuristic"))
            .expect("should have heuristic finding for test-related dep");
        assert_eq!(
            heuristic.nature,
            KnowledgeNature::Observation,
            "heuristic findings must use Observation nature"
        );
        assert!(heuristic.description.contains("testing"));
        assert!(heuristic.description.contains("my-test-helper"));
    }

    #[test]
    fn heuristic_not_emitted_for_known_package() {
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![dep("tracing", "tracing::info", 5)],
            vec![import("tracing", &["info"])],
        );
        let findings = detector.detect(&file);

        let heuristic = findings
            .iter()
            .find(|f| f.description.contains("heuristic"));
        assert!(
            heuristic.is_none(),
            "known package 'tracing' should NOT get heuristic finding"
        );
        // But should have a Convention finding from known classification
        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention);
        assert!(convention.is_some());
    }

    #[test]
    fn heuristic_multiple_unknown_deps() {
        let detector = DependencyUsageDetector;
        let file = make_ts_file_with_deps(
            vec![
                dep("my-http-wrapper", "my-http-wrapper", 1),
                dep("custom-logger", "custom-logger", 5),
            ],
            vec![
                import("my-http-wrapper", &["fetch"]),
                import("custom-logger", &["log"]),
            ],
        );
        let findings = detector.detect(&file);

        let http_heuristic = findings
            .iter()
            .find(|f| f.description.contains("heuristic") && f.description.contains("HTTP"));
        assert!(
            http_heuristic.is_some(),
            "should detect HTTP heuristic for my-http-wrapper"
        );

        let log_heuristic = findings
            .iter()
            .find(|f| f.description.contains("heuristic") && f.description.contains("logging"));
        assert!(
            log_heuristic.is_some(),
            "should detect logging heuristic for custom-logger"
        );
    }

    #[test]
    fn heuristic_case_insensitive() {
        // Heuristic should work case-insensitively
        assert_eq!(
            classify_heuristic_domain("MyTestLib", Language::TypeScript),
            Some(DependencyDomain::Testing)
        );
        assert_eq!(
            classify_heuristic_domain("HTTP-Client", Language::Rust),
            Some(DependencyDomain::Http)
        );
    }

    #[test]
    fn reqwest_import_shows_call_site_evidence() {
        use seshat_core::ir::FunctionCall;
        let detector = DependencyUsageDetector;
        let mut file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 1),
                dep("reqwest", "reqwest::Response", 2),
            ],
            vec![import("reqwest", &["Client", "Response"])],
        );
        // Populate function_calls with actual API call sites.
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.function_calls = vec![
                FunctionCall {
                    callee: "Client::new".to_owned(),
                    line: 15,
                    end_line: 15,
                    snippet: "let client = Client::new();".to_owned(),
                },
                FunctionCall {
                    callee: "client.get".to_owned(),
                    line: 20,
                    end_line: 22,
                    snippet: "client.get(url).send().await?".to_owned(),
                },
            ];
        }
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("HTTP"))
            .expect("should have HTTP convention for reqwest");
        assert!(convention.description.contains("reqwest"));

        // Evidence should come from call sites, not import lines.
        assert!(
            !convention.evidence.is_empty(),
            "should have call-site evidence"
        );
        // At least one evidence item should be at line 15 or 20 (call sites, not import lines 1/2).
        assert!(
            convention
                .evidence
                .iter()
                .any(|e| e.line == 15 || e.line == 20),
            "evidence should point to call sites (lines 15 or 20), not import lines (1/2), got: {:?}",
            convention
                .evidence
                .iter()
                .map(|e| e.line)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn detect_with_source_sets_real_snippet() {
        let detector = DependencyUsageDetector;
        // TypeScript file with a react dependency import at line 1.
        let file = ProjectFile {
            path: PathBuf::from("src/app.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports: vec![Import {
                module: "react".to_owned(),
                names: vec!["useState".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: vec![DependencyUsage {
                package: "react".to_owned(),
                import_path: "react".to_owned(),
                line: 1,
            }],
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        };
        let source = "import { useState } from 'react';\n";

        let findings = detector.detect_with_source(&file, source);

        assert!(!findings.is_empty(), "should have at least one finding");
        let finding = &findings[0];
        // No function calls in IR to match → evidence is empty.
        // Convention finding still exists with correct description.
        assert!(
            finding.description.contains("react"),
            "convention should mention react in description"
        );
    }

    // -----------------------------------------------------------------------
    // BUG: dependency_usage fallback evidence gets zeroed by snippet filter
    // -----------------------------------------------------------------------

    #[test]
    fn fallback_evidence_zeroed_by_snippet_contains_filter() {
        // When call_sites and derive_evidence are both empty, evidence is
        // Vec::new().  The convention finding still exists with the correct
        // description — no meaningless import-line fallback evidence.
        let detector = DependencyUsageDetector;
        let file = make_rust_file_with_deps(
            vec![dep("reqwest", "reqwest::Client", 5)],
            vec![import("reqwest", &["Client"])],
        );

        let findings = detector.detect(&file);
        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("HTTP"))
            .expect("should have HTTP convention finding");

        // Convention finding exists with correct description.
        assert!(
            convention.description.contains("reqwest"),
            "convention should mention reqwest in description"
        );
    }

    // -----------------------------------------------------------------------
    // BUG: dependency_usage unscoped call sites cross-contaminate deps
    // -----------------------------------------------------------------------

    #[test]
    fn unscoped_call_sites_contaminate_dependency_findings() {
        // File has reqwest (HTTP) AND tracing (logging) imports.
        // The "Canonical HTTP library: reqwest" finding should only have
        // reqwest-related evidence, not tracing macro calls.
        let detector = DependencyUsageDetector;
        let mut file = make_rust_file_with_deps(
            vec![
                dep("reqwest", "reqwest::Client", 1),
                dep("tracing", "tracing", 2),
            ],
            vec![
                import("reqwest", &["Client"]),
                import("tracing", &["info", "warn"]),
            ],
        );
        // File has both reqwest API calls AND tracing macro calls.
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.function_calls = vec![seshat_core::FunctionCall {
                callee: "Client::new".to_owned(),
                line: 15,
                end_line: 15,
                snippet: "Client::new()".to_owned(),
            }];
            ir.macro_calls = vec![
                seshat_core::MacroCall {
                    name: "info".to_owned(),
                    line: 20,
                },
                seshat_core::MacroCall {
                    name: "warn".to_owned(),
                    line: 30,
                },
            ];
        }

        let findings = detector.detect(&file);
        // Find the HTTP convention finding (about reqwest)
        let http_finding = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("HTTP"))
            .expect("should have HTTP convention");

        // After fix: HTTP finding should only have reqwest evidence (line 15),
        // tracing macros (lines 20, 30) should NOT appear.
        let evidence_lines: Vec<usize> = http_finding.evidence.iter().map(|e| e.line).collect();
        assert!(
            !evidence_lines.contains(&20) && !evidence_lines.contains(&30),
            "HTTP finding should NOT contain tracing call sites, got: {:?}",
            evidence_lines
        );
        assert!(
            evidence_lines.contains(&15),
            "HTTP finding should contain reqwest call site (line 15), got: {:?}",
            evidence_lines
        );
    }

    // -----------------------------------------------------------------------
    // Integration: call-site evidence gets snippets with context
    // -----------------------------------------------------------------------

    #[test]
    fn dependency_usage_detector_produces_call_site_snippets_with_context() {
        use seshat_core::ir::FunctionCall;
        // Rust file with reqwest: function call at line 8 with an empty snippet.
        // detect_with_source should fill the snippet from source AND include 2
        // lines of leading context (lines 6-7), setting snippet_start_line = 6.
        let detector = DependencyUsageDetector;
        let mut file = make_rust_file_with_deps(
            vec![dep("reqwest", "reqwest::Client", 1)],
            vec![import("reqwest", &["Client"])],
        );
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.function_calls = vec![FunctionCall {
                callee: "Client::new".to_owned(),
                line: 8,
                end_line: 8,
                // Empty snippet — detect_with_source should fill it from source.
                snippet: String::new(),
            }];
        }

        // Source: 12 lines; line 8 is the call site.
        let source_lines: Vec<String> = (1..=12).map(|i| format!("source_line_{i}")).collect();
        let source = source_lines.join("\n");

        let findings = detector.detect_with_source(&file, &source);
        let convention = findings
            .iter()
            .find(|f| f.description.contains("HTTP"))
            .expect("should have HTTP convention finding");

        let ev = convention
            .evidence
            .iter()
            .find(|e| e.line == 8)
            .expect("should have call-site evidence at line 8");

        assert!(
            !ev.snippet.is_empty(),
            "call-site snippet must be populated, got empty"
        );
        assert!(
            ev.snippet_start_line > 0 && ev.snippet_start_line < 8,
            "snippet_start_line should be before line 8, got: {}",
            ev.snippet_start_line
        );
        assert!(
            ev.snippet.contains("source_line_6"),
            "snippet should include context 2 lines before (source_line_6), got: {:?}",
            ev.snippet
        );
        assert!(
            ev.snippet.contains("source_line_8"),
            "snippet should include the call site line (source_line_8), got: {:?}",
            ev.snippet
        );
    }

    #[test]
    fn serde_derive_macro_used_as_evidence() {
        let detector = DependencyUsageDetector;
        let mut file = make_rust_file_with_deps(
            vec![dep("serde", "serde::Serialize", 3)],
            vec![import("serde", &["Serialize"])],
        );
        if let seshat_core::LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.derive_macros.push(seshat_core::DeriveUsage {
                type_name: "AppConfig".to_owned(),
                derives: vec!["Serialize".to_owned(), "Deserialize".to_owned()],
                line: 15,
            });
        }

        let findings = detector.detect(&file);
        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention)
            .expect("should have a serialization convention");

        let has_derive_evidence = convention.evidence.iter().any(|e| e.line == 15);

        assert!(
            has_derive_evidence,
            "should have derive macro evidence at line 15 for Serialize, got evidence: {:?}",
            convention.evidence
        );
    }
}
