//! Dependency manifest analysis.
//!
//! Parses `Cargo.toml`, `package.json`, and `pyproject.toml` files to extract
//! declared dependencies, cross-reference them with actual usage from parsed IR,
//! flag dead (unused) dependencies, and categorize dependencies by domain.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use seshat_core::ir::{Language, ProjectFile};
use seshat_core::{DependencyDomain, classify_domain};

use crate::error::ScanError;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The type of dependency manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestType {
    CargoToml,
    PackageJson,
    PyprojectToml,
}

impl ManifestType {
    /// Detect manifest type from file name (not full path — just the file stem).
    pub fn from_filename(name: &str) -> Option<Self> {
        match name {
            "Cargo.toml" => Some(Self::CargoToml),
            "package.json" => Some(Self::PackageJson),
            "pyproject.toml" => Some(Self::PyprojectToml),
            _ => None,
        }
    }

    /// All known manifest filenames for discovery purposes.
    pub fn all_filenames() -> &'static [&'static str] {
        &["Cargo.toml", "package.json", "pyproject.toml"]
    }
}

/// A single dependency declared in a manifest file.
#[derive(Debug, Clone)]
pub struct DeclaredDependency {
    pub name: String,
    pub version: String,
    pub is_dev: bool,
    pub category: DependencyDomain,
}

/// Per-dependency usage statistics after cross-referencing with parsed IR.
#[derive(Debug, Clone)]
pub struct DependencyUsageStats {
    pub dependency: DeclaredDependency,
    /// Number of files that import from this dependency.
    pub files_using: usize,
    /// Whether this dependency was never imported in any parsed file.
    pub is_dead: bool,
}

/// Full analysis result for a single manifest file.
#[derive(Debug, Clone)]
pub struct ManifestAnalysis {
    pub manifest_path: PathBuf,
    pub manifest_type: ManifestType,
    pub dependencies: Vec<DependencyUsageStats>,
    /// Auto-detected internal package/crate names (e.g. Rust crate names,
    /// Python package names) normalised with `-` → `_`.
    pub internal_names: Vec<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a single manifest file and return its declared dependencies.
///
/// Does **not** perform cross-referencing — call [`analyze_manifests`] for that.
pub fn parse_manifest(
    path: &Path,
    content: &str,
    manifest_type: ManifestType,
) -> Result<Vec<DeclaredDependency>, ScanError> {
    match manifest_type {
        ManifestType::CargoToml => parse_cargo_toml(path, content),
        ManifestType::PackageJson => parse_package_json(path, content),
        ManifestType::PyprojectToml => parse_pyproject_toml(path, content),
    }
}

/// Analyze manifests by cross-referencing declared dependencies against actual
/// import usage in the parsed IR of all project files.
///
/// For each manifest, every declared dependency is checked against all files'
/// imports. A dependency is flagged as **dead** when zero files import from it.
pub fn analyze_manifests(
    manifests: &[(PathBuf, String, ManifestType)],
    parsed_files: &[ProjectFile],
) -> Result<Vec<ManifestAnalysis>, ScanError> {
    let mut results = Vec::with_capacity(manifests.len());

    for (path, content, manifest_type) in manifests {
        let declared = parse_manifest(path, content, *manifest_type)?;
        let stats = cross_reference(&declared, parsed_files, *manifest_type);
        let internal_names = match manifest_type {
            ManifestType::CargoToml => extract_crate_names(path, content),
            ManifestType::PyprojectToml => extract_package_names(path, content),
            ManifestType::PackageJson => extract_js_package_names(path, content),
        };
        results.push(ManifestAnalysis {
            manifest_path: path.clone(),
            manifest_type: *manifest_type,
            dependencies: stats,
            internal_names,
        });
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Cargo.toml parsing
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CargoManifest {
    #[serde(default)]
    dependencies: HashMap<String, toml::Value>,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: HashMap<String, toml::Value>,
}

fn parse_cargo_toml(path: &Path, content: &str) -> Result<Vec<DeclaredDependency>, ScanError> {
    let manifest: CargoManifest =
        toml::from_str(content).map_err(|e| ScanError::ManifestError {
            path: path.to_path_buf(),
            reason: format!("invalid TOML: {e}"),
        })?;

    let mut deps = Vec::new();
    for (name, value) in &manifest.dependencies {
        let version = extract_cargo_version(value);
        deps.push(DeclaredDependency {
            name: name.clone(),
            version,
            is_dev: false,
            category: categorize_dependency(name, ManifestType::CargoToml),
        });
    }
    for (name, value) in &manifest.dev_dependencies {
        let version = extract_cargo_version(value);
        deps.push(DeclaredDependency {
            name: name.clone(),
            version,
            is_dev: true,
            category: categorize_dependency(name, ManifestType::CargoToml),
        });
    }
    Ok(deps)
}

/// Extract version string from Cargo.toml dependency value.
///
/// Handles both `dep = "1.0"` and `dep = { version = "1.0", ... }` forms,
/// as well as path/git-only dependencies.
fn extract_cargo_version(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(t) => t
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("*")
            .to_owned(),
        _ => "*".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Crate name extraction (auto-detection for import resolution)
// ---------------------------------------------------------------------------

/// Extract Rust crate names from a `Cargo.toml` for internal namespace detection.
///
/// Reads `[package].name` and resolves every `[workspace].members` entry to
/// the `[package].name` declared in its inner `Cargo.toml`. Hyphens are
/// normalised to underscores so the returned names line up with `use ...`
/// identifiers.
///
/// Glob patterns (e.g. `crates/*`) are expanded by [`expand_glob_member`].
/// Members — glob or literal — whose inner `Cargo.toml` cannot be read are
/// silently skipped. Cargo itself errors on missing literal members, but a
/// scan over an in-progress tree often hits half-applied changes; staying
/// quiet here keeps `workspace_crates` free of fake names synthesised from
/// directory basenames.
fn extract_crate_names(path: &Path, content: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct PackageInfo {
        #[serde(default)]
        name: Option<String>,
    }

    #[derive(Deserialize)]
    struct WorkspaceInfo {
        #[serde(default)]
        members: Vec<String>,
    }

    #[derive(Deserialize)]
    struct PartialCargoToml {
        package: Option<PackageInfo>,
        workspace: Option<WorkspaceInfo>,
    }

    let manifest: PartialCargoToml = match toml::from_str(content) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to parse Cargo.toml for crate name extraction");
            return Vec::new();
        }
    };

    let mut names = Vec::new();

    if let Some(ref pkg) = manifest.package {
        if let Some(ref name) = pkg.name {
            names.push(name.replace('-', "_"));
        }
    }

    if let Some(ws) = &manifest.workspace {
        let manifest_dir = path.parent().unwrap_or(Path::new("."));
        for member in &ws.members {
            let dirs: Vec<PathBuf> = if is_glob_pattern(member) {
                expand_glob_member(manifest_dir, member)
            } else {
                vec![manifest_dir.join(member)]
            };
            for dir in dirs {
                let Some(crate_name) = read_inner_crate_name(&dir.join("Cargo.toml")) else {
                    continue;
                };
                if !crate_name.is_empty() {
                    names.push(crate_name.replace('-', "_"));
                }
            }
        }
    }

    names
}

/// Return `true` if the string looks like a glob pattern (contains `*`, `?`, or `[`).
fn is_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Expand a `[workspace.members]` glob pattern (e.g. `crates/*`) relative to
/// the directory containing the workspace `Cargo.toml`.
///
/// Non-directory entries are filtered out, mirroring Cargo's own glob
/// semantics for workspace members. Absolute patterns, non-UTF8 paths,
/// invalid globs, and per-entry I/O errors emit a `tracing::warn!` and are
/// skipped — a malformed or hostile `Cargo.toml` must not poison the scan.
fn expand_glob_member(manifest_dir: &Path, pattern: &str) -> Vec<PathBuf> {
    // Absolute patterns would escape `manifest_dir` because `Path::join` keeps
    // them verbatim. Workspace globs must stay inside the workspace.
    if Path::new(pattern).is_absolute() {
        tracing::warn!(
            pattern = %pattern,
            "Absolute path in [workspace.members] glob; skipping",
        );
        return Vec::new();
    }

    let joined = manifest_dir.join(pattern);
    let Some(pattern_str) = joined.to_str() else {
        tracing::warn!(
            pattern = %pattern,
            manifest_dir = %manifest_dir.display(),
            "Non-UTF8 path while expanding workspace-member glob; skipping",
        );
        return Vec::new();
    };

    // `glob` requires `/` as separator on every platform; on Windows
    // `Path::join` introduces `\` which the crate won't match against.
    #[cfg(windows)]
    let pattern_owned = pattern_str.replace('\\', "/");
    #[cfg(windows)]
    let pattern_str: &str = pattern_owned.as_str();

    let paths = match glob::glob(pattern_str) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                pattern = %pattern_str,
                error = %e,
                "Invalid glob pattern in [workspace.members]; skipping",
            );
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in paths {
        match entry {
            Ok(path) if path.is_dir() => out.push(path),
            Ok(_) => {}
            Err(e) => tracing::warn!(
                pattern = %pattern_str,
                error = %e,
                "I/O error while expanding workspace-member glob entry; skipping",
            ),
        }
    }
    out
}

/// Return `true` if the workspace pattern stays inside its manifest directory.
///
/// Rejects absolute paths and any `..` component — both would let a manifest
/// reach outside the project root and pull `package.json` names from unrelated
/// directories on the host filesystem.
fn is_safe_workspace_pattern(pattern: &str) -> bool {
    let p = Path::new(pattern);
    if p.is_absolute() {
        return false;
    }
    p.components()
        .all(|c| !matches!(c, std::path::Component::ParentDir))
}

/// Strip a leading UTF-8 BOM (`U+FEFF`) if present.
///
/// `serde_json` and `serde_yml` both reject documents starting with a BOM, yet
/// many editors save manifests with one. Strip it before handing the content
/// to the deserialiser so a stray byte-order mark does not silently zero out
/// workspace extraction.
fn strip_utf8_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

/// Read `[package].name` from an inner workspace member `Cargo.toml`.
///
/// Returns `None` if the file cannot be read or lacks a package name.
fn read_inner_crate_name(path: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct InnerPackage {
        name: Option<String>,
    }
    #[derive(Deserialize)]
    struct InnerCargo {
        package: Option<InnerPackage>,
    }
    let content = std::fs::read_to_string(path).ok()?;
    let manifest: InnerCargo = toml::from_str(&content).ok()?;
    manifest.package?.name
}

// ---------------------------------------------------------------------------
// Package name extraction (auto-detection for import resolution)
// ---------------------------------------------------------------------------

/// Extract Python package names from a `pyproject.toml` for internal namespace detection.
///
/// Reads `[project].name` (PEP 621) first; falls back to `[tool.poetry].name`
/// when PEP 621 is absent. Normalises hyphens to underscores.
fn extract_package_names(path: &Path, content: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct Pep621Project {
        name: String,
    }

    #[derive(Deserialize)]
    struct PoetrySection {
        name: Option<String>,
    }

    #[derive(Deserialize)]
    struct ToolSection {
        poetry: Option<PoetrySection>,
    }

    #[derive(Deserialize)]
    struct PyprojectNames {
        project: Option<Pep621Project>,
        tool: Option<ToolSection>,
    }

    let manifest: PyprojectNames = match toml::from_str(content) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to parse pyproject.toml for package name extraction");
            return Vec::new();
        }
    };

    let mut names = Vec::new();

    if let Some(project) = manifest.project {
        names.push(project.name.replace('-', "_"));
        return names;
    }

    if let Some(tool) = manifest.tool {
        if let Some(poetry) = tool.poetry {
            if let Some(name) = poetry.name {
                names.push(name.replace('-', "_"));
            }
        }
    }

    names
}

// ---------------------------------------------------------------------------
// package.json parsing
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PackageJson {
    #[serde(default)]
    dependencies: HashMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: HashMap<String, String>,
}

fn parse_package_json(path: &Path, content: &str) -> Result<Vec<DeclaredDependency>, ScanError> {
    let manifest: PackageJson =
        serde_json::from_str(content).map_err(|e| ScanError::ManifestError {
            path: path.to_path_buf(),
            reason: format!("invalid JSON: {e}"),
        })?;

    let mut deps = Vec::new();
    for (name, version) in &manifest.dependencies {
        deps.push(DeclaredDependency {
            name: name.clone(),
            version: version.clone(),
            is_dev: false,
            category: categorize_dependency(name, ManifestType::PackageJson),
        });
    }
    for (name, version) in &manifest.dev_dependencies {
        deps.push(DeclaredDependency {
            name: name.clone(),
            version: version.clone(),
            is_dev: true,
            category: categorize_dependency(name, ManifestType::PackageJson),
        });
    }
    Ok(deps)
}

// ---------------------------------------------------------------------------
// JS/TS workspace package name extraction
// ---------------------------------------------------------------------------

/// Extract JS/TS workspace package names from a `package.json` for internal
/// namespace detection.
///
/// Parses the `"workspaces"` field in both supported shapes:
/// - Array form: `"workspaces": ["packages/*", "apps/*"]`
/// - Yarn-classic object form: `"workspaces": { "packages": ["packages/*"], "nohoist": [...] }`
///
/// Glob patterns are expanded against the directory containing `path`; each
/// matched directory's `package.json` is read for its `"name"` field. The
/// root `"name"` (if present) is also included so single-package projects
/// resolve self-imports correctly.
///
/// Parsing is tolerant: `null`, string, or non-array shapes for `"workspaces"`
/// are treated as "no workspaces" rather than aborting the whole extraction,
/// and non-string array elements are skipped individually. A leading UTF-8
/// BOM is stripped before parsing.
///
/// Names are returned verbatim, sorted, and deduplicated — scoped names like
/// `@myorg/shared` retain the `@scope/` prefix, and no hyphen-to-underscore
/// normalisation is applied (JS/TS imports use the literal `"name"` from
/// `package.json`).
fn extract_js_package_names(path: &Path, content: &str) -> Vec<String> {
    let manifest_dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => {
            tracing::warn!(
                path = %path.display(),
                "package.json path has no usable parent directory; skipping workspace extraction"
            );
            return Vec::new();
        }
    };

    let content = strip_utf8_bom(content);
    let value: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to parse package.json for workspace package name extraction");
            return Vec::new();
        }
    };

    let mut names = Vec::new();

    if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
        if !name.trim().is_empty() {
            names.push(name.to_owned());
        }
    }

    let patterns: Vec<String> = match value.get("workspaces") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Some(serde_json::Value::Object(obj)) => obj
            .get("packages")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    for pattern in &patterns {
        for dir in expand_js_workspace_pattern(manifest_dir, pattern) {
            if let Some(name) = read_inner_package_name(&dir.join("package.json")) {
                if !name.trim().is_empty() {
                    names.push(name);
                }
            }
        }
    }

    names.sort();
    names.dedup();
    names
}

/// Expand a workspace pattern (literal path or glob) into a list of matching
/// directories under `manifest_dir`.
///
/// Empty/whitespace patterns and patterns that escape the manifest directory
/// (absolute paths or `..` segments) are rejected with a warning. Literal
/// in-tree paths bypass the glob crate and resolve directly. Glob patterns
/// are joined with `manifest_dir`, normalised to forward slashes for
/// cross-platform `glob` crate compatibility, then expanded via [`glob::glob`].
/// Matches that are not directories are filtered out, and results are sorted
/// for deterministic ordering across runs.
fn expand_js_workspace_pattern(manifest_dir: &Path, pattern: &str) -> Vec<PathBuf> {
    if pattern.trim().is_empty() {
        return Vec::new();
    }
    if !is_safe_workspace_pattern(pattern) {
        tracing::warn!(
            pattern = %pattern,
            "rejecting unsafe workspace pattern (absolute path or `..` segment)"
        );
        return Vec::new();
    }

    if !is_glob_pattern(pattern) {
        let p = manifest_dir.join(pattern);
        return if p.is_dir() { vec![p] } else { Vec::new() };
    }

    let abs_pattern = manifest_dir.join(pattern);
    let abs_str = match abs_pattern.to_str() {
        Some(s) => s,
        None => {
            tracing::warn!(pattern = %pattern, "non-UTF8 workspace pattern; skipping");
            return Vec::new();
        }
    };
    // `glob` requires `/` as separator on every platform; on Windows
    // `Path::join` introduces `\` which the crate won't match against.
    #[cfg(windows)]
    let abs_owned = abs_str.replace('\\', "/");
    #[cfg(windows)]
    let abs_str: &str = abs_owned.as_str();

    match glob::glob(abs_str) {
        Ok(iter) => {
            let mut matches: Vec<PathBuf> =
                iter.filter_map(Result::ok).filter(|p| p.is_dir()).collect();
            matches.sort();
            matches
        }
        Err(e) => {
            tracing::warn!(pattern = %pattern, error = %e, "invalid workspace glob pattern");
            Vec::new()
        }
    }
}

/// Read `"name"` from a workspace member's `package.json`.
///
/// Returns `None` silently when the file does not exist (a normal case for
/// a matched directory that lacks a `package.json`). Other I/O errors and
/// JSON parse failures emit a `tracing::warn!` so misconfigured manifests
/// are diagnosable. A leading UTF-8 BOM is stripped before parsing.
fn read_inner_package_name(path: &Path) -> Option<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to read workspace package.json"
            );
            return None;
        }
    };
    let content = strip_utf8_bom(&content);
    let value: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to parse workspace package.json"
            );
            return None;
        }
    };
    value.get("name").and_then(|v| v.as_str()).map(String::from)
}

/// Extract JS/TS workspace package names from a sibling `pnpm-workspace.yaml`.
///
/// Parses the `packages:` list, expands each pattern relative to the YAML's
/// directory, and reads each matched directory's `package.json` `"name"`.
///
/// Parsing is tolerant: missing `packages:`, non-sequence shapes, and
/// non-string elements are all treated as "no patterns" rather than aborting.
/// A leading UTF-8 BOM is stripped before parsing.
///
/// Names are returned verbatim, sorted, and deduplicated — `@scope/name`
/// prefixes and hyphens are preserved, matching [`extract_js_package_names`].
///
/// Returns an empty `Vec` (with a `tracing::warn`) on any IO or parse failure
/// so a malformed YAML never aborts the surrounding scan.
// Not yet called from the orchestrator (pnpm wiring lands in a follow-up).
#[allow(dead_code)]
fn parse_pnpm_workspace_yaml(path: &Path) -> Vec<String> {
    let manifest_dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => {
            tracing::warn!(
                path = %path.display(),
                "pnpm-workspace.yaml path has no usable parent directory; skipping"
            );
            return Vec::new();
        }
    };

    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to read pnpm-workspace.yaml");
            return Vec::new();
        }
    };
    let content = strip_utf8_bom(&content);

    let value: serde_yml::Value = match serde_yml::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to parse pnpm-workspace.yaml");
            return Vec::new();
        }
    };

    let patterns: Vec<String> = value
        .get("packages")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut names = Vec::new();
    for pattern in &patterns {
        for dir in expand_js_workspace_pattern(manifest_dir, pattern) {
            if let Some(name) = read_inner_package_name(&dir.join("package.json")) {
                if !name.trim().is_empty() {
                    names.push(name);
                }
            }
        }
    }

    names.sort();
    names.dedup();
    names
}

// ---------------------------------------------------------------------------
// pyproject.toml parsing
// ---------------------------------------------------------------------------

/// PEP 621 project table.
#[derive(Deserialize)]
struct PyprojectToml {
    #[serde(default)]
    project: Option<PyprojectProject>,
}

#[derive(Deserialize)]
struct PyprojectProject {
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default, rename = "optional-dependencies")]
    optional_dependencies: HashMap<String, Vec<String>>,
}

fn parse_pyproject_toml(path: &Path, content: &str) -> Result<Vec<DeclaredDependency>, ScanError> {
    let manifest: PyprojectToml =
        toml::from_str(content).map_err(|e| ScanError::ManifestError {
            path: path.to_path_buf(),
            reason: format!("invalid TOML: {e}"),
        })?;

    let project = match manifest.project {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };

    let mut deps = Vec::new();
    for spec in &project.dependencies {
        let (name, version) = parse_pep508_name_version(spec);
        let category = categorize_dependency(&name, ManifestType::PyprojectToml);
        deps.push(DeclaredDependency {
            name,
            version,
            is_dev: false,
            category,
        });
    }

    // Treat optional-dependencies groups named "dev", "test", or "testing" as dev deps.
    let dev_group_names = ["dev", "test", "testing"];
    for (group, group_deps) in &project.optional_dependencies {
        let is_dev = dev_group_names.contains(&group.to_lowercase().as_str());
        for spec in group_deps {
            let (name, version) = parse_pep508_name_version(spec);
            deps.push(DeclaredDependency {
                name: name.clone(),
                version,
                is_dev,
                category: categorize_dependency(&name, ManifestType::PyprojectToml),
            });
        }
    }

    Ok(deps)
}

/// Extract package name and version constraint from a PEP 508 dependency
/// specifier (e.g. `requests>=2.28`).
fn parse_pep508_name_version(spec: &str) -> (String, String) {
    // Find the first character that is not part of the name
    // PEP 508 names: letters, digits, -, _, .
    let name_end = spec
        .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
        .unwrap_or(spec.len());
    let name = spec[..name_end].trim().to_lowercase().replace('-', "_");
    let version = spec[name_end..].trim().to_owned();
    let version = if version.is_empty() {
        "*".to_owned()
    } else {
        version
    };
    (name, version)
}

// ---------------------------------------------------------------------------
// Cross-referencing
// ---------------------------------------------------------------------------

/// For each declared dependency, count how many files import from it.
fn cross_reference(
    declared: &[DeclaredDependency],
    parsed_files: &[ProjectFile],
    manifest_type: ManifestType,
) -> Vec<DependencyUsageStats> {
    declared
        .iter()
        .map(|dep| {
            let files_using = count_files_importing(&dep.name, parsed_files, manifest_type);
            DependencyUsageStats {
                dependency: dep.clone(),
                files_using,
                is_dead: files_using == 0,
            }
        })
        .collect()
}

/// Count how many `ProjectFile`s import from a dependency.
///
/// Matching heuristics vary by manifest type:
/// - **Cargo.toml**: import module starts with `dep_name` (with `-` → `_` normalisation)
/// - **package.json**: import module starts with `dep_name` (or `@scope/dep_name`)
/// - **pyproject.toml**: import module starts with `dep_name` (with `-` → `_` normalisation)
fn count_files_importing(
    dep_name: &str,
    parsed_files: &[ProjectFile],
    manifest_type: ManifestType,
) -> usize {
    let normalised = dep_name.replace('-', "_");

    parsed_files
        .iter()
        .filter(|pf| {
            pf.imports.iter().any(|imp| {
                let module = &imp.module;
                match manifest_type {
                    ManifestType::CargoToml => {
                        let mod_normalised = module.replace('-', "_");
                        // `use serde::Serialize` → module = "serde" or "serde::Serialize"
                        mod_normalised == normalised
                            || mod_normalised.starts_with(&format!("{normalised}::"))
                    }
                    ManifestType::PackageJson => {
                        // Exact match or scoped sub-path: `react-dom/client`
                        module == dep_name || module.starts_with(&format!("{dep_name}/"))
                    }
                    ManifestType::PyprojectToml => {
                        let mod_normalised = module.replace('-', "_").to_lowercase();
                        mod_normalised == normalised
                            || mod_normalised.starts_with(&format!("{normalised}."))
                    }
                }
            })
        })
        .count()
}

// ---------------------------------------------------------------------------
// Dependency categorization (delegates to seshat_core::classify_domain)
// ---------------------------------------------------------------------------

/// Map a [`ManifestType`] to the corresponding [`Language`] for classification.
fn manifest_type_to_language(mt: ManifestType) -> Language {
    match mt {
        ManifestType::CargoToml => Language::Rust,
        ManifestType::PackageJson => Language::TypeScript,
        ManifestType::PyprojectToml => Language::Python,
    }
}

/// Categorize a dependency by name using the unified classification table
/// in `seshat_core`.
///
/// Returns [`DependencyDomain::Unknown`] when the package is not in any known
/// list. This is a thin wrapper around [`classify_domain`].
pub fn categorize_dependency(name: &str, manifest_type: ManifestType) -> DependencyDomain {
    classify_domain(name, manifest_type_to_language(manifest_type))
        .unwrap_or(DependencyDomain::Unknown)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::DependencyDomain;
    use seshat_core::ir::{Import, Language, LanguageIR, RustIR};
    use tempfile::tempdir;

    fn make_pf_with_imports(imports: Vec<Import>, language: Language) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("test.rs"),
            language,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: match language {
                Language::Rust => LanguageIR::Rust(RustIR::default()),
                Language::TypeScript => {
                    LanguageIR::TypeScript(seshat_core::ir::TypeScriptIR::default())
                }
                Language::JavaScript => {
                    LanguageIR::JavaScript(seshat_core::ir::JavaScriptIR::default())
                }
                Language::Python => LanguageIR::Python(seshat_core::ir::PythonIR::default()),
            },
            file_doc: None,
        }
    }

    fn make_import(module: &str) -> Import {
        Import {
            module: module.to_owned(),
            names: Vec::new(),
            is_type_only: false,
            line: 1,
        }
    }

    // -----------------------------------------------------------------------
    // ManifestType detection
    // -----------------------------------------------------------------------

    #[test]
    fn manifest_type_from_filename() {
        assert_eq!(
            ManifestType::from_filename("Cargo.toml"),
            Some(ManifestType::CargoToml)
        );
        assert_eq!(
            ManifestType::from_filename("package.json"),
            Some(ManifestType::PackageJson)
        );
        assert_eq!(
            ManifestType::from_filename("pyproject.toml"),
            Some(ManifestType::PyprojectToml)
        );
        assert_eq!(ManifestType::from_filename("Makefile"), None);
    }

    // -----------------------------------------------------------------------
    // Cargo.toml parsing
    // -----------------------------------------------------------------------

    #[test]
    fn cargo_toml_simple_version() {
        let content = r#"
[dependencies]
serde = "1.0"
tokio = { version = "1", features = ["full"] }

[dev-dependencies]
tempfile = "3"
"#;
        let deps = parse_cargo_toml(Path::new("Cargo.toml"), content).unwrap();
        assert_eq!(deps.len(), 3);

        let serde_dep = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde_dep.version, "1.0");
        assert!(!serde_dep.is_dev);

        let tokio_dep = deps.iter().find(|d| d.name == "tokio").unwrap();
        assert_eq!(tokio_dep.version, "1");
        assert!(!tokio_dep.is_dev);

        let tempfile_dep = deps.iter().find(|d| d.name == "tempfile").unwrap();
        assert_eq!(tempfile_dep.version, "3");
        assert!(tempfile_dep.is_dev);
    }

    #[test]
    fn cargo_toml_path_dependency() {
        let content = r#"
[dependencies]
my-crate = { path = "../my-crate" }
"#;
        let deps = parse_cargo_toml(Path::new("Cargo.toml"), content).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "my-crate");
        assert_eq!(deps[0].version, "*"); // No version for path deps
    }

    #[test]
    fn cargo_toml_workspace_dependency() {
        let content = r#"
[dependencies]
serde.workspace = true
"#;
        let deps = parse_cargo_toml(Path::new("Cargo.toml"), content).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");
        // workspace = true is a table with bool value, no version
        assert_eq!(deps[0].version, "*");
    }

    #[test]
    fn cargo_toml_empty() {
        let content = "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n";
        let deps = parse_cargo_toml(Path::new("Cargo.toml"), content).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn cargo_toml_invalid() {
        let content = "this is not valid toml {{{}";
        let result = parse_cargo_toml(Path::new("Cargo.toml"), content);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ScanError::ManifestError { .. }));
    }

    // -----------------------------------------------------------------------
    // package.json parsing
    // -----------------------------------------------------------------------

    #[test]
    fn package_json_basic() {
        let content = r#"{
  "dependencies": {
    "react": "^18.2.0",
    "axios": "^1.6.0"
  },
  "devDependencies": {
    "jest": "^29.0.0"
  }
}"#;
        let deps = parse_package_json(Path::new("package.json"), content).unwrap();
        assert_eq!(deps.len(), 3);

        let react = deps.iter().find(|d| d.name == "react").unwrap();
        assert_eq!(react.version, "^18.2.0");
        assert!(!react.is_dev);

        let jest = deps.iter().find(|d| d.name == "jest").unwrap();
        assert!(jest.is_dev);
    }

    #[test]
    fn package_json_no_deps() {
        let content = r#"{ "name": "my-pkg", "version": "1.0.0" }"#;
        let deps = parse_package_json(Path::new("package.json"), content).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn package_json_invalid() {
        let content = "not json {}}";
        let result = parse_package_json(Path::new("package.json"), content);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // pyproject.toml parsing
    // -----------------------------------------------------------------------

    #[test]
    fn pyproject_toml_basic() {
        let content = r#"
[project]
dependencies = [
    "requests>=2.28",
    "pydantic>=2.0",
]

[project.optional-dependencies]
dev = ["pytest>=7.0", "black"]
docs = ["sphinx"]
"#;
        let deps = parse_pyproject_toml(Path::new("pyproject.toml"), content).unwrap();
        assert_eq!(deps.len(), 5);

        let requests = deps.iter().find(|d| d.name == "requests").unwrap();
        assert_eq!(requests.version, ">=2.28");
        assert!(!requests.is_dev);

        let pytest = deps.iter().find(|d| d.name == "pytest").unwrap();
        assert!(pytest.is_dev); // "dev" group is treated as dev

        let sphinx = deps.iter().find(|d| d.name == "sphinx").unwrap();
        assert!(!sphinx.is_dev); // "docs" is not a dev group
    }

    #[test]
    fn pyproject_toml_no_project_table() {
        let content = r#"
[tool.poetry]
name = "my-pkg"
"#;
        let deps = parse_pyproject_toml(Path::new("pyproject.toml"), content).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn pyproject_toml_test_group_is_dev() {
        let content = r#"
[project]
dependencies = []

[project.optional-dependencies]
test = ["pytest"]
testing = ["hypothesis"]
"#;
        let deps = parse_pyproject_toml(Path::new("pyproject.toml"), content).unwrap();
        assert!(deps.iter().all(|d| d.is_dev));
    }

    #[test]
    fn pyproject_toml_invalid() {
        let content = "not valid [[[ toml";
        let result = parse_pyproject_toml(Path::new("pyproject.toml"), content);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // PEP 508 parsing
    // -----------------------------------------------------------------------

    #[test]
    fn pep508_simple_name() {
        let (name, version) = parse_pep508_name_version("requests");
        assert_eq!(name, "requests");
        assert_eq!(version, "*");
    }

    #[test]
    fn pep508_with_version() {
        let (name, version) = parse_pep508_name_version("requests>=2.28");
        assert_eq!(name, "requests");
        assert_eq!(version, ">=2.28");
    }

    #[test]
    fn pep508_with_extras() {
        let (name, version) = parse_pep508_name_version("uvicorn[standard]>=0.20");
        assert_eq!(name, "uvicorn");
        assert_eq!(version, "[standard]>=0.20");
    }

    #[test]
    fn pep508_normalises_hyphens() {
        let (name, _) = parse_pep508_name_version("my-cool-package>=1.0");
        assert_eq!(name, "my_cool_package");
    }

    // -----------------------------------------------------------------------
    // Cross-referencing
    // -----------------------------------------------------------------------

    #[test]
    fn cross_reference_finds_usage() {
        let declared = vec![DeclaredDependency {
            name: "serde".to_owned(),
            version: "1".to_owned(),
            is_dev: false,
            category: DependencyDomain::Serialization,
        }];

        let files = vec![
            make_pf_with_imports(vec![make_import("serde::Serialize")], Language::Rust),
            make_pf_with_imports(vec![make_import("serde")], Language::Rust),
            make_pf_with_imports(vec![make_import("tokio::spawn")], Language::Rust),
        ];

        let stats = cross_reference(&declared, &files, ManifestType::CargoToml);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].files_using, 2);
        assert!(!stats[0].is_dead);
    }

    #[test]
    fn cross_reference_dead_dependency() {
        let declared = vec![DeclaredDependency {
            name: "never-used".to_owned(),
            version: "1".to_owned(),
            is_dev: false,
            category: DependencyDomain::Unknown,
        }];

        let files = vec![make_pf_with_imports(
            vec![make_import("serde")],
            Language::Rust,
        )];

        let stats = cross_reference(&declared, &files, ManifestType::CargoToml);
        assert_eq!(stats[0].files_using, 0);
        assert!(stats[0].is_dead);
    }

    #[test]
    fn cross_reference_cargo_normalises_hyphens() {
        let declared = vec![DeclaredDependency {
            name: "serde-json".to_owned(),
            version: "1".to_owned(),
            is_dev: false,
            category: DependencyDomain::Serialization,
        }];

        let files = vec![make_pf_with_imports(
            vec![make_import("serde_json::Value")],
            Language::Rust,
        )];

        let stats = cross_reference(&declared, &files, ManifestType::CargoToml);
        assert_eq!(stats[0].files_using, 1);
        assert!(!stats[0].is_dead);
    }

    #[test]
    fn cross_reference_npm_scoped_package() {
        let declared = vec![DeclaredDependency {
            name: "@testing-library/react".to_owned(),
            version: "^14".to_owned(),
            is_dev: true,
            category: DependencyDomain::Testing,
        }];

        let files = vec![make_pf_with_imports(
            vec![make_import("@testing-library/react")],
            Language::TypeScript,
        )];

        let stats = cross_reference(&declared, &files, ManifestType::PackageJson);
        assert_eq!(stats[0].files_using, 1);
        assert!(!stats[0].is_dead);
    }

    #[test]
    fn cross_reference_npm_subpath() {
        let declared = vec![DeclaredDependency {
            name: "react-dom".to_owned(),
            version: "^18".to_owned(),
            is_dev: false,
            category: DependencyDomain::Unknown,
        }];

        let files = vec![make_pf_with_imports(
            vec![make_import("react-dom/client")],
            Language::TypeScript,
        )];

        let stats = cross_reference(&declared, &files, ManifestType::PackageJson);
        assert_eq!(stats[0].files_using, 1);
        assert!(!stats[0].is_dead);
    }

    #[test]
    fn cross_reference_python_normalises() {
        let declared = vec![DeclaredDependency {
            name: "my_package".to_owned(),
            version: ">=1.0".to_owned(),
            is_dev: false,
            category: DependencyDomain::Unknown,
        }];

        let files = vec![make_pf_with_imports(
            vec![make_import("my_package.utils")],
            Language::Python,
        )];

        let stats = cross_reference(&declared, &files, ManifestType::PyprojectToml);
        assert_eq!(stats[0].files_using, 1);
        assert!(!stats[0].is_dead);
    }

    // -----------------------------------------------------------------------
    // Categorization
    // -----------------------------------------------------------------------

    #[test]
    fn categorize_known_rust_deps() {
        assert_eq!(
            categorize_dependency("serde", ManifestType::CargoToml),
            DependencyDomain::Serialization
        );
        assert_eq!(
            categorize_dependency("tokio", ManifestType::CargoToml),
            DependencyDomain::AsyncRuntime
        );
        assert_eq!(
            categorize_dependency("axum", ManifestType::CargoToml),
            DependencyDomain::WebFramework
        );
        assert_eq!(
            categorize_dependency("tracing", ManifestType::CargoToml),
            DependencyDomain::Logging
        );
        assert_eq!(
            categorize_dependency("rusqlite", ManifestType::CargoToml),
            DependencyDomain::Database
        );
        assert_eq!(
            categorize_dependency("tempfile", ManifestType::CargoToml),
            DependencyDomain::Testing
        );
    }

    #[test]
    fn categorize_known_js_deps() {
        assert_eq!(
            categorize_dependency("react", ManifestType::PackageJson),
            DependencyDomain::WebFramework
        );
        assert_eq!(
            categorize_dependency("jest", ManifestType::PackageJson),
            DependencyDomain::Testing
        );
        assert_eq!(
            categorize_dependency("axios", ManifestType::PackageJson),
            DependencyDomain::Http
        );
    }

    #[test]
    fn categorize_known_python_deps() {
        assert_eq!(
            categorize_dependency("django", ManifestType::PyprojectToml),
            DependencyDomain::WebFramework
        );
        assert_eq!(
            categorize_dependency("pytest", ManifestType::PyprojectToml),
            DependencyDomain::Testing
        );
        assert_eq!(
            categorize_dependency("requests", ManifestType::PyprojectToml),
            DependencyDomain::Http
        );
    }

    #[test]
    fn categorize_unknown_dep() {
        assert_eq!(
            categorize_dependency("my-custom-lib", ManifestType::CargoToml),
            DependencyDomain::Unknown
        );
    }

    // -----------------------------------------------------------------------
    // extract_crate_names (Rust / Cargo.toml)
    // -----------------------------------------------------------------------

    #[test]
    fn extract_crate_names_single_package() {
        let content = r#"
[package]
name = "my-app"
version = "0.1.0"
"#;
        let names = extract_crate_names(Path::new("Cargo.toml"), content);
        assert_eq!(names, vec!["my_app"]);
    }

    #[test]
    fn extract_crate_names_workspace_members() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/core")).unwrap();
        std::fs::create_dir_all(root.join("crates/api")).unwrap();
        std::fs::write(
            root.join("crates/core/Cargo.toml"),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/api/Cargo.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let manifest_path = root.join("Cargo.toml");
        let content = r#"
[workspace]
members = ["crates/core", "crates/api"]
"#;
        let names = extract_crate_names(&manifest_path, content);
        assert_eq!(names, vec!["core", "api"]);
    }

    #[test]
    fn extract_crate_names_workspace_and_root_package() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/seshat-core")).unwrap();
        std::fs::create_dir_all(root.join("crates/seshat-graph")).unwrap();
        std::fs::write(
            root.join("crates/seshat-core/Cargo.toml"),
            "[package]\nname = \"seshat-core\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/seshat-graph/Cargo.toml"),
            "[package]\nname = \"seshat-graph\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let manifest_path = root.join("Cargo.toml");
        let content = r#"
[package]
name = "seshat-root"
version = "0.1.0"

[workspace]
members = ["crates/seshat-core", "crates/seshat-graph"]
"#;
        let names = extract_crate_names(&manifest_path, content);
        // [package].name comes first, then workspace members
        assert!(names.contains(&"seshat_root".to_owned()));
        assert!(names.contains(&"seshat_core".to_owned()));
        assert!(names.contains(&"seshat_graph".to_owned()));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn extract_crate_names_literal_member_without_cargo_toml_is_skipped() {
        // Unification AC: literal members whose dir lacks Cargo.toml are
        // silently skipped (no fake basename fallback into workspace_crates).
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("legacy")).unwrap();
        std::fs::write(
            root.join("legacy/Cargo.toml"),
            "[package]\nname = \"legacy\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        // `ghost` exists as a dir but has no Cargo.toml.
        std::fs::create_dir_all(root.join("ghost")).unwrap();

        let manifest_path = root.join("Cargo.toml");
        let content = r#"
[workspace]
members = ["legacy", "ghost"]
"#;
        let names = extract_crate_names(&manifest_path, content);
        assert_eq!(
            names,
            vec!["legacy".to_owned()],
            "literal member without Cargo.toml must be silently skipped",
        );
    }

    #[test]
    fn extract_crate_names_hyphen_normalisation() {
        let content = r#"
[package]
name = "my-crate"
version = "0.1.0"
"#;
        let names = extract_crate_names(Path::new("Cargo.toml"), content);
        assert_eq!(names, vec!["my_crate"]);
    }

    #[test]
    fn extract_crate_names_workspace_members_with_glob_expanded() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/foo")).unwrap();
        std::fs::create_dir_all(root.join("crates/bar")).unwrap();
        std::fs::write(
            root.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/bar/Cargo.toml"),
            "[package]\nname = \"bar\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let manifest_path = root.join("Cargo.toml");
        let content = r#"
[workspace]
members = ["crates/*"]
"#;
        let mut names = extract_crate_names(&manifest_path, content);
        names.sort();
        assert_eq!(
            names,
            vec!["bar".to_owned(), "foo".to_owned()],
            "glob `crates/*` should expand to every inner crate",
        );
    }

    #[test]
    fn extract_crate_names_workspace_glob_skips_dir_without_cargo_toml() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/foo")).unwrap();
        std::fs::create_dir_all(root.join("crates/empty")).unwrap();
        std::fs::write(
            root.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let manifest_path = root.join("Cargo.toml");
        let content = r#"
[workspace]
members = ["crates/*"]
"#;
        let names = extract_crate_names(&manifest_path, content);
        assert_eq!(
            names,
            vec!["foo".to_owned()],
            "glob-expanded dir without Cargo.toml must be silently skipped",
        );
    }

    #[test]
    fn extract_crate_names_workspace_mixed_literal_and_glob_members() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("legacy-crate")).unwrap();
        std::fs::create_dir_all(root.join("crates/foo")).unwrap();
        std::fs::write(
            root.join("legacy-crate/Cargo.toml"),
            "[package]\nname = \"legacy-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let manifest_path = root.join("Cargo.toml");
        let content = r#"
[workspace]
members = ["legacy-crate", "crates/*"]
"#;
        let names = extract_crate_names(&manifest_path, content);
        // Assert independent presence so a single broken branch can't be
        // masked by a sort-then-eq.
        assert!(
            names.contains(&"foo".to_owned()),
            "glob branch must resolve `foo` — got {names:?}",
        );
        assert!(
            names.contains(&"legacy_crate".to_owned()),
            "literal branch must resolve `legacy_crate` — got {names:?}",
        );
        assert_eq!(
            names.len(),
            2,
            "mixed members must not produce stray names — got {names:?}",
        );
    }

    #[test]
    fn extract_crate_names_workspace_invalid_glob_alongside_literal_member() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("legacy-crate")).unwrap();
        std::fs::write(
            root.join("legacy-crate/Cargo.toml"),
            "[package]\nname = \"legacy-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let manifest_path = root.join("Cargo.toml");
        let content = r#"
[workspace]
members = ["legacy-crate", "crates/["]
"#;
        let names = extract_crate_names(&manifest_path, content);
        assert_eq!(
            names,
            vec!["legacy_crate".to_owned()],
            "invalid glob must be silently dropped, literal members still resolve",
        );
    }

    #[test]
    fn extract_crate_names_workspace_package_name_optional() {
        // Cargo.toml with [package] but no name (e.g. workspace.package inheritance)
        // PackageInfo.name is Option<String> — missing name just produces no root name.
        let content = r#"
[package]
version = "0.1.0"
edition = "2021"
"#;
        let names = extract_crate_names(Path::new("Cargo.toml"), content);
        assert!(
            names.is_empty(),
            "missing [package].name must produce empty names"
        );
    }
    #[test]
    fn extract_crate_names_empty_workspace_members() {
        let content = r#"
[workspace]
members = []
"#;
        let names = extract_crate_names(Path::new("Cargo.toml"), content);
        assert!(names.is_empty());
    }

    #[test]
    fn extract_crate_names_invalid_toml_returns_empty() {
        let content = "not valid toml {{{ oops";
        let names = extract_crate_names(Path::new("Cargo.toml"), content);
        assert!(
            names.is_empty(),
            "should return empty list on parse error, not crash"
        );
    }

    // -----------------------------------------------------------------------
    // expand_glob_member (filesystem glob expansion for workspace members)
    // -----------------------------------------------------------------------

    #[test]
    fn expand_glob_member_happy_path_resolves_subdirs() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/foo")).unwrap();
        std::fs::create_dir_all(root.join("crates/bar")).unwrap();

        let mut paths = expand_glob_member(root, "crates/*");
        paths.sort();
        assert_eq!(
            paths,
            vec![root.join("crates/bar"), root.join("crates/foo")],
        );
    }

    #[test]
    fn expand_glob_member_invalid_pattern_returns_empty() {
        let tmp = tempdir().expect("tempdir");
        // Sanity: confirm the chosen pattern really IS rejected by `glob`
        // (otherwise the test would silently degrade into the no-matches case).
        assert!(
            glob::Pattern::new("crates/[").is_err(),
            "test premise: `crates/[` must be an invalid glob",
        );
        let paths = expand_glob_member(tmp.path(), "crates/[");
        assert!(
            paths.is_empty(),
            "invalid glob pattern must yield empty Vec, not panic",
        );
    }

    #[test]
    fn expand_glob_member_no_matches_returns_empty() {
        // Distinct from the invalid-pattern case: a *valid* pattern that
        // simply matches zero entries must also return an empty Vec.
        let tmp = tempdir().expect("tempdir");
        let paths = expand_glob_member(tmp.path(), "nonexistent/*");
        assert!(
            paths.is_empty(),
            "valid glob with no matches must yield empty Vec",
        );
    }

    #[test]
    fn expand_glob_member_absolute_pattern_is_rejected() {
        // `Path::join` keeps absolute paths verbatim, so without a guard
        // `members = ["/etc/*"]` would escape `manifest_dir`. The guard must
        // drop the pattern instead of letting it through.
        let tmp = tempdir().expect("tempdir");
        let paths = expand_glob_member(tmp.path(), "/etc/*");
        assert!(
            paths.is_empty(),
            "absolute glob pattern must be rejected, got {paths:?}",
        );
    }

    #[test]
    fn expand_glob_member_filters_non_directories() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/realdir")).unwrap();
        std::fs::write(root.join("crates/notadir.txt"), "hello").unwrap();

        let paths = expand_glob_member(root, "crates/*");
        assert_eq!(paths, vec![root.join("crates/realdir")]);
    }

    // -----------------------------------------------------------------------
    // extract_package_names (Python / pyproject.toml)
    // -----------------------------------------------------------------------

    #[test]
    fn extract_package_names_pep621() {
        let content = r#"
[project]
name = "my-package"
version = "1.0.0"
"#;
        let names = extract_package_names(Path::new("pyproject.toml"), content);
        assert_eq!(names, vec!["my_package"]);
    }

    #[test]
    fn extract_package_names_poetry_fallback() {
        let content = r#"
[tool.poetry]
name = "my-package"
version = "1.0.0"
"#;
        let names = extract_package_names(Path::new("pyproject.toml"), content);
        assert_eq!(names, vec!["my_package"]);
    }

    #[test]
    fn extract_package_names_pep621_takes_precedence_over_poetry() {
        let content = r#"
[project]
name = "pep621-name"

[tool.poetry]
name = "poetry-name"
"#;
        let names = extract_package_names(Path::new("pyproject.toml"), content);
        assert_eq!(names, vec!["pep621_name"]);
    }

    #[test]
    fn extract_package_names_invalid_toml_returns_empty() {
        let content = "not valid toml {{{ oops";
        let names = extract_package_names(Path::new("pyproject.toml"), content);
        assert!(
            names.is_empty(),
            "should return empty list on parse error, not crash"
        );
    }

    // -----------------------------------------------------------------------
    // extract_js_package_names (JavaScript / TypeScript / package.json)
    // -----------------------------------------------------------------------

    /// Build a JS workspace fixture under `root` with the given (relative-path,
    /// package.json-content) pairs. Each parent directory is created as needed.
    fn write_js_workspace_fixture(root: &Path, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let file_path = root.join(rel);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture dir");
            }
            std::fs::write(&file_path, content).expect("write fixture file");
        }
    }

    #[test]
    fn extract_js_names_workspaces_array_with_glob() {
        // Array form expands `packages/*` and collects child names.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "private": true, "workspaces": ["packages/*"] }"#,
                ),
                (
                    "packages/shared/package.json",
                    r#"{ "name": "@myorg/shared" }"#,
                ),
                ("packages/web/package.json", r#"{ "name": "my-web" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert_eq!(names.len(), 2, "got: {names:?}");
        assert!(names.contains(&"@myorg/shared".to_owned()));
        assert!(names.contains(&"my-web".to_owned()));
    }

    #[test]
    fn extract_js_names_root_name_only_no_workspaces() {
        // No workspaces field, only the root "name".
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = r#"{ "name": "my-app", "version": "1.0.0" }"#;
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert_eq!(names, vec!["my-app"]);
    }

    #[test]
    fn extract_js_names_no_workspaces_field_no_name() {
        // Neither a "name" nor a "workspaces" field → empty list.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = r#"{ "version": "1.0.0", "dependencies": {} }"#;
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert!(names.is_empty(), "got: {names:?}");
    }

    #[test]
    fn extract_js_names_invalid_json_returns_empty() {
        // Malformed JSON → empty list (no panic, warn-only).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = "{ not valid json ::: ";
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert!(names.is_empty());
    }

    #[test]
    fn extract_js_names_empty_workspaces_array() {
        // Empty array → no workspace members; root "name" still returned.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = r#"{ "name": "my-app", "workspaces": [] }"#;
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert_eq!(names, vec!["my-app"]);
    }

    #[test]
    fn extract_js_names_workspaces_yarn_classic_object_form() {
        // Yarn-classic object form { "packages": [...], "nohoist": [...] }
        // must be parsed equivalently to the array form.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{
                        "private": true,
                        "workspaces": {
                            "packages": ["packages/*"],
                            "nohoist": ["**/react-native"]
                        }
                    }"#,
                ),
                (
                    "packages/shared/package.json",
                    r#"{ "name": "@myorg/shared" }"#,
                ),
                ("packages/web/package.json", r#"{ "name": "@myorg/web" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert!(
            names.contains(&"@myorg/shared".to_owned()),
            "got: {names:?}"
        );
        assert!(names.contains(&"@myorg/web".to_owned()), "got: {names:?}");
    }

    #[test]
    fn extract_js_names_workspaces_multiple_patterns() {
        // Array form supports multiple patterns (e.g. packages/* + apps/*).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "workspaces": ["packages/*", "apps/*"] }"#,
                ),
                (
                    "packages/lib-a/package.json",
                    r#"{ "name": "@myorg/lib-a" }"#,
                ),
                ("apps/web/package.json", r#"{ "name": "web-app" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert!(names.contains(&"@myorg/lib-a".to_owned()), "got: {names:?}");
        assert!(names.contains(&"web-app".to_owned()), "got: {names:?}");
    }

    #[test]
    fn extract_js_names_root_name_and_workspaces_both_included() {
        // Single-package projects with a root "name" still get it included
        // even when workspaces are also defined.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "name": "monorepo-root", "workspaces": ["packages/*"] }"#,
                ),
                ("packages/lib/package.json", r#"{ "name": "@myorg/lib" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert!(
            names.contains(&"monorepo-root".to_owned()),
            "got: {names:?}"
        );
        assert!(names.contains(&"@myorg/lib".to_owned()), "got: {names:?}");
    }

    #[test]
    fn extract_js_names_preserves_scope_and_hyphens_verbatim() {
        // Scoped (@org/name) and unscoped (with-hyphens) names returned
        // verbatim — no normalisation that would lose the @scope or convert
        // hyphens to underscores.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                ("package.json", r#"{ "workspaces": ["packages/*"] }"#),
                ("packages/a/package.json", r#"{ "name": "@my-org/my-pkg" }"#),
                (
                    "packages/b/package.json",
                    r#"{ "name": "plain-hyphen-name" }"#,
                ),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert!(
            names.contains(&"@my-org/my-pkg".to_owned()),
            "scope/hyphen preserved: {names:?}"
        );
        assert!(
            names.contains(&"plain-hyphen-name".to_owned()),
            "hyphens preserved verbatim: {names:?}"
        );
        // Negative checks — the normalised forms must NOT appear.
        assert!(!names.contains(&"my_org/my_pkg".to_owned()));
        assert!(!names.contains(&"plain_hyphen_name".to_owned()));
    }

    #[test]
    fn extract_js_names_literal_workspace_path_no_glob() {
        // Literal (non-glob) workspace paths resolve directly.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "workspaces": ["packages/shared", "packages/web"] }"#,
                ),
                (
                    "packages/shared/package.json",
                    r#"{ "name": "@myorg/shared" }"#,
                ),
                ("packages/web/package.json", r#"{ "name": "@myorg/web" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert!(names.contains(&"@myorg/shared".to_owned()));
        assert!(names.contains(&"@myorg/web".to_owned()));
    }

    #[test]
    fn extract_js_names_workspace_without_package_json_skipped() {
        // A matched directory missing its package.json is silently skipped —
        // siblings still resolve.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                ("package.json", r#"{ "workspaces": ["packages/*"] }"#),
                (
                    "packages/has-pkg/package.json",
                    r#"{ "name": "@myorg/has-pkg" }"#,
                ),
            ],
        );
        // Create the empty dir with no package.json.
        std::fs::create_dir_all(root.join("packages").join("empty")).unwrap();

        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert_eq!(names, vec!["@myorg/has-pkg"], "got: {names:?}");
    }

    #[test]
    fn extract_js_names_workspace_package_json_without_name_skipped() {
        // A workspace package.json without a "name" field is silently skipped.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                ("package.json", r#"{ "workspaces": ["packages/*"] }"#),
                (
                    "packages/named/package.json",
                    r#"{ "name": "@myorg/named" }"#,
                ),
                (
                    "packages/nameless/package.json",
                    r#"{ "version": "1.0.0" }"#,
                ),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert_eq!(names, vec!["@myorg/named"], "got: {names:?}");
    }

    #[test]
    fn extract_js_names_handles_js_monorepo_fixture() {
        // The committed fixture under tests/fixtures/js_monorepo/ should be
        // discoverable end-to-end via extract_js_package_names.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("js_monorepo");
        let manifest_path = fixture.join("package.json");
        let content = std::fs::read_to_string(&manifest_path).expect("fixture exists");
        let names = extract_js_package_names(&manifest_path, &content);
        assert!(
            names.contains(&"@myorg/shared".to_owned()),
            "got: {names:?}"
        );
        assert!(names.contains(&"@myorg/web".to_owned()), "got: {names:?}");
    }

    // ── New behaviour tests (hardening) ────────────────────────────────

    #[test]
    fn extract_js_names_rejects_absolute_workspace_pattern() {
        // Absolute paths must not let a manifest escape its directory and
        // pull names from arbitrary filesystem locations.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A real package.json sitting somewhere the attacker would love to grab.
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::write(
            root.join("etc").join("package.json"),
            r#"{ "name": "should-not-leak" }"#,
        )
        .unwrap();

        let absolute = root.join("etc");
        let absolute_str = absolute.to_str().unwrap();
        let content = format!(r#"{{ "workspaces": ["{absolute_str}"] }}"#);
        let names = extract_js_package_names(&root.join("package.json"), &content);
        assert!(
            !names.contains(&"should-not-leak".to_owned()),
            "absolute pattern was honoured: {names:?}"
        );
        assert!(names.is_empty(), "got: {names:?}");
    }

    #[test]
    fn extract_js_names_rejects_parent_dir_escape() {
        // `..` segments must not allow the pattern to escape manifest_dir.
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        std::fs::create_dir_all(outer.join("sibling")).unwrap();
        std::fs::write(
            outer.join("sibling").join("package.json"),
            r#"{ "name": "outside-the-project" }"#,
        )
        .unwrap();
        let project = outer.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let manifest_path = project.join("package.json");
        let content = r#"{ "workspaces": ["../sibling"] }"#;
        let names = extract_js_package_names(&manifest_path, content);
        assert!(
            !names.contains(&"outside-the-project".to_owned()),
            "parent-dir escape honoured: {names:?}"
        );
        assert!(names.is_empty(), "got: {names:?}");
    }

    #[test]
    fn extract_js_names_skips_empty_pattern_no_duplicate_root() {
        // An empty pattern would otherwise resolve to manifest_dir itself
        // and re-read the root package.json, double-adding its "name".
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = r#"{ "name": "my-app", "workspaces": [""] }"#;
        std::fs::write(root.join("package.json"), content).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert_eq!(names, vec!["my-app"]);
    }

    #[test]
    fn extract_js_names_deduplicates_overlapping_patterns() {
        // ["packages/*", "packages/shared"] both match packages/shared;
        // the returned Vec must contain each name once.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "workspaces": ["packages/*", "packages/shared"] }"#,
                ),
                (
                    "packages/shared/package.json",
                    r#"{ "name": "@myorg/shared" }"#,
                ),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert_eq!(names, vec!["@myorg/shared"]);
    }

    #[test]
    fn extract_js_names_returned_list_is_sorted() {
        // Cross-pattern ordering is deterministic regardless of pattern order
        // in the manifest.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "name": "z-root", "workspaces": ["apps/*", "packages/*"] }"#,
                ),
                ("apps/web/package.json", r#"{ "name": "web-app" }"#),
                ("packages/lib/package.json", r#"{ "name": "@a/lib" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "names should be returned sorted: {names:?}");
    }

    #[test]
    fn extract_js_names_strips_utf8_bom() {
        // Editors that save with a BOM must not silently zero out extraction.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = "\u{FEFF}{ \"name\": \"bom-prefixed-app\" }";
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert_eq!(names, vec!["bom-prefixed-app"]);
    }

    #[test]
    fn extract_js_names_tolerates_null_workspaces() {
        // `"workspaces": null` should not abort the whole extraction —
        // the root "name" must still be returned.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = r#"{ "name": "my-app", "workspaces": null }"#;
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert_eq!(names, vec!["my-app"]);
    }

    #[test]
    fn extract_js_names_tolerates_string_workspaces() {
        // `"workspaces": "packages/*"` is invalid per the spec but should be
        // gracefully treated as "no workspaces" — root "name" preserved.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = r#"{ "name": "my-app", "workspaces": "packages/*" }"#;
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert_eq!(names, vec!["my-app"]);
    }

    #[test]
    fn extract_js_names_skips_non_string_workspace_elements() {
        // Mixed-type arrays: string elements are honoured, non-strings dropped.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "workspaces": [123, "packages/*", null] }"#,
                ),
                ("packages/lib/package.json", r#"{ "name": "@myorg/lib" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert_eq!(names, vec!["@myorg/lib"]);
    }

    #[test]
    fn extract_js_names_rejects_whitespace_only_name() {
        // A `"name": " "` value must not pollute workspace_crates with a
        // meaningless internal name.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = r#"{ "name": "   ", "version": "1.0.0" }"#;
        let names = extract_js_package_names(&root.join("package.json"), content);
        assert!(names.is_empty(), "got: {names:?}");
    }

    #[test]
    fn extract_js_names_rejects_non_string_root_name() {
        // `"name": 123` must not abort the whole extraction; the workspaces
        // list still needs to be parsed.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "package.json",
                    r#"{ "name": 123, "workspaces": ["packages/*"] }"#,
                ),
                ("packages/lib/package.json", r#"{ "name": "@myorg/lib" }"#),
            ],
        );
        let manifest = std::fs::read_to_string(root.join("package.json")).unwrap();
        let names = extract_js_package_names(&root.join("package.json"), &manifest);
        assert_eq!(names, vec!["@myorg/lib"]);
    }

    #[test]
    fn extract_js_names_no_parent_path_returns_empty() {
        // A bare "package.json" with no directory must NOT silently scan CWD.
        let content = r#"{ "name": "my-app", "workspaces": ["packages/*"] }"#;
        let names = extract_js_package_names(Path::new("package.json"), content);
        assert!(names.is_empty(), "got: {names:?}");
    }

    // -----------------------------------------------------------------------
    // parse_pnpm_workspace_yaml (pnpm monorepos)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_pnpm_yaml_typical_glob_layout() {
        // AC: a typical `packages: ["packages/*"]` layout collects each
        // workspace member's `"name"` verbatim.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "packages/shared/package.json",
                    r#"{ "name": "@myorg/shared" }"#,
                ),
                ("packages/web/package.json", r#"{ "name": "@myorg/web" }"#),
            ],
        );
        let yaml = r#"
packages:
  - "packages/*"
"#;
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert_eq!(names.len(), 2, "got: {names:?}");
        assert!(names.contains(&"@myorg/shared".to_owned()));
        assert!(names.contains(&"@myorg/web".to_owned()));
    }

    #[test]
    fn parse_pnpm_yaml_multiple_patterns() {
        // Multiple top-level entries in `packages:` are all expanded.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "packages/shared/package.json",
                    r#"{ "name": "@myorg/shared" }"#,
                ),
                ("apps/web/package.json", r#"{ "name": "@myorg/web" }"#),
            ],
        );
        let yaml = r#"
packages:
  - "packages/*"
  - "apps/*"
"#;
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert!(
            names.contains(&"@myorg/shared".to_owned()),
            "got: {names:?}"
        );
        assert!(names.contains(&"@myorg/web".to_owned()), "got: {names:?}");
    }

    #[test]
    fn parse_pnpm_yaml_literal_path_no_glob() {
        // Literal paths (no glob chars) resolve directly without invoking glob.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[(
                "packages/shared/package.json",
                r#"{ "name": "@myorg/shared" }"#,
            )],
        );
        let yaml = r#"
packages:
  - "packages/shared"
"#;
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert_eq!(names, vec!["@myorg/shared"]);
    }

    #[test]
    fn parse_pnpm_yaml_preserves_scope_and_hyphens_verbatim() {
        // Same verbatim-naming guarantee as extract_js_package_names: no
        // hyphen-to-underscore normalisation, no @scope stripping.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "packages/foo-bar/package.json",
                    r#"{ "name": "@my-org/foo-bar" }"#,
                ),
                ("packages/baz/package.json", r#"{ "name": "plain-name" }"#),
            ],
        );
        let yaml = r#"
packages:
  - "packages/*"
"#;
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert!(
            names.contains(&"@my-org/foo-bar".to_owned()),
            "got: {names:?}"
        );
        assert!(names.contains(&"plain-name".to_owned()), "got: {names:?}");
    }

    #[test]
    fn parse_pnpm_yaml_missing_file_returns_empty() {
        // No pnpm-workspace.yaml at the given path → empty list, no panic.
        let dir = tempfile::tempdir().unwrap();
        let yaml_path = dir.path().join("pnpm-workspace.yaml");
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert!(names.is_empty());
    }

    #[test]
    fn parse_pnpm_yaml_invalid_yaml_returns_empty() {
        // Malformed YAML → empty list, no panic (warn-only).
        let dir = tempfile::tempdir().unwrap();
        let yaml_path = dir.path().join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, ":\n  - not: [valid yaml: at all").unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert!(names.is_empty());
    }

    #[test]
    fn parse_pnpm_yaml_empty_packages_returns_empty() {
        // `packages: []` is well-formed but has no members.
        let dir = tempfile::tempdir().unwrap();
        let yaml_path = dir.path().join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, "packages: []\n").unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert!(names.is_empty());
    }

    #[test]
    fn parse_pnpm_yaml_member_without_name_skipped() {
        // A matched workspace member whose package.json lacks a `"name"`
        // field is silently skipped — other members still resolve.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[
                (
                    "packages/shared/package.json",
                    r#"{ "name": "@myorg/shared" }"#,
                ),
                ("packages/anon/package.json", r#"{ "version": "1.0.0" }"#),
            ],
        );
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, "packages:\n  - \"packages/*\"\n").unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert_eq!(names, vec!["@myorg/shared"]);
    }

    #[test]
    fn parse_pnpm_yaml_strips_utf8_bom() {
        // YAML editors sometimes save with a BOM; serde_yml rejects it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[(
                "packages/shared/package.json",
                r#"{ "name": "@myorg/shared" }"#,
            )],
        );
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, "\u{FEFF}packages:\n  - \"packages/*\"\n").unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert_eq!(names, vec!["@myorg/shared"]);
    }

    #[test]
    fn parse_pnpm_yaml_tolerates_missing_packages_key() {
        // Missing `packages:` → empty list, no warning, no panic.
        let dir = tempfile::tempdir().unwrap();
        let yaml_path = dir.path().join("pnpm-workspace.yaml");
        std::fs::write(&yaml_path, "shared-workspace-lockfile: true\n").unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert!(names.is_empty(), "got: {names:?}");
    }

    #[test]
    fn parse_pnpm_yaml_skips_non_string_package_elements() {
        // Non-string entries in `packages:` are skipped individually.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[("packages/lib/package.json", r#"{ "name": "@myorg/lib" }"#)],
        );
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(
            &yaml_path,
            "packages:\n  - 123\n  - \"packages/*\"\n  - null\n",
        )
        .unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert_eq!(names, vec!["@myorg/lib"]);
    }

    #[test]
    fn parse_pnpm_yaml_deduplicates_overlapping_patterns() {
        // Overlapping patterns matching the same dir produce a single name.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_js_workspace_fixture(
            root,
            &[(
                "packages/shared/package.json",
                r#"{ "name": "@myorg/shared" }"#,
            )],
        );
        let yaml_path = root.join("pnpm-workspace.yaml");
        std::fs::write(
            &yaml_path,
            "packages:\n  - \"packages/*\"\n  - \"packages/shared\"\n",
        )
        .unwrap();
        let names = parse_pnpm_workspace_yaml(&yaml_path);
        assert_eq!(names, vec!["@myorg/shared"]);
    }

    // -----------------------------------------------------------------------
    // internal_names via analyze_manifests (includes local_packages union)
    // -----------------------------------------------------------------------

    #[test]
    fn analyze_manifests_populates_internal_names_from_cargo() {
        let content = r#"
[package]
name = "my-crate"
version = "0.1.0"
"#;
        let manifests = vec![(
            PathBuf::from("Cargo.toml"),
            content.to_owned(),
            ManifestType::CargoToml,
        )];
        let results = analyze_manifests(&manifests, &[]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].internal_names, vec!["my_crate"]);
    }

    #[test]
    fn analyze_manifests_populates_internal_names_from_pyproject() {
        let content = r#"
[project]
name = "my-package"
"#;
        let manifests = vec![(
            PathBuf::from("pyproject.toml"),
            content.to_owned(),
            ManifestType::PyprojectToml,
        )];
        let results = analyze_manifests(&manifests, &[]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].internal_names, vec!["my_package"]);
    }

    #[test]
    fn analyze_manifests_package_json_has_populated_internal_names() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = dir.path();
        let shared_dir = root.join("packages").join("shared");
        let web_dir = root.join("packages").join("web");
        std::fs::create_dir_all(&shared_dir).unwrap();
        std::fs::create_dir_all(&web_dir).unwrap();
        std::fs::write(
            shared_dir.join("package.json"),
            r#"{ "name": "@myorg/shared", "version": "1.0.0" }"#,
        )
        .unwrap();
        std::fs::write(
            web_dir.join("package.json"),
            r#"{ "name": "my-web", "version": "1.0.0" }"#,
        )
        .unwrap();

        let root_content = r#"{ "private": true, "workspaces": ["packages/*"] }"#;
        let root_path = root.join("package.json");
        std::fs::write(&root_path, root_content).unwrap();

        let manifests = vec![(
            root_path,
            root_content.to_owned(),
            ManifestType::PackageJson,
        )];
        let results = analyze_manifests(&manifests, &[]).unwrap();
        assert_eq!(results.len(), 1);
        let names = &results[0].internal_names;
        assert!(
            names.contains(&"@myorg/shared".to_owned()),
            "expected @myorg/shared in {names:?}"
        );
        assert!(
            names.contains(&"my-web".to_owned()),
            "expected my-web in {names:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Full analyze_manifests flow
    // -----------------------------------------------------------------------

    #[test]
    fn analyze_manifests_end_to_end() {
        let cargo_content = r#"
[dependencies]
serde = "1"
tokio = "1"

[dev-dependencies]
tempfile = "3"
"#;

        let files = vec![
            make_pf_with_imports(
                vec![make_import("serde::Serialize"), make_import("tokio::spawn")],
                Language::Rust,
            ),
            make_pf_with_imports(vec![make_import("serde")], Language::Rust),
        ];

        let manifests = vec![(
            PathBuf::from("Cargo.toml"),
            cargo_content.to_owned(),
            ManifestType::CargoToml,
        )];

        let results = analyze_manifests(&manifests, &files).unwrap();
        assert_eq!(results.len(), 1);

        let analysis = &results[0];
        assert_eq!(analysis.manifest_type, ManifestType::CargoToml);
        assert_eq!(analysis.dependencies.len(), 3);

        let serde_stats = analysis
            .dependencies
            .iter()
            .find(|s| s.dependency.name == "serde")
            .unwrap();
        assert_eq!(serde_stats.files_using, 2);
        assert!(!serde_stats.is_dead);

        let tokio_stats = analysis
            .dependencies
            .iter()
            .find(|s| s.dependency.name == "tokio")
            .unwrap();
        assert_eq!(tokio_stats.files_using, 1);
        assert!(!tokio_stats.is_dead);

        let tempfile_stats = analysis
            .dependencies
            .iter()
            .find(|s| s.dependency.name == "tempfile")
            .unwrap();
        assert_eq!(tempfile_stats.files_using, 0);
        assert!(tempfile_stats.is_dead); // dev dep not used in code
    }

    #[test]
    fn analyze_manifests_multiple_manifest_types() {
        let cargo_content = "[dependencies]\nserde = \"1\"\n";
        let package_json = r#"{"dependencies": {"react": "^18"}}"#;

        let files = vec![
            make_pf_with_imports(vec![make_import("serde")], Language::Rust),
            make_pf_with_imports(vec![make_import("react")], Language::TypeScript),
        ];

        let manifests = vec![
            (
                PathBuf::from("Cargo.toml"),
                cargo_content.to_owned(),
                ManifestType::CargoToml,
            ),
            (
                PathBuf::from("package.json"),
                package_json.to_owned(),
                ManifestType::PackageJson,
            ),
        ];

        let results = analyze_manifests(&manifests, &files).unwrap();
        assert_eq!(results.len(), 2);

        let cargo_analysis = results
            .iter()
            .find(|r| r.manifest_type == ManifestType::CargoToml)
            .unwrap();
        assert!(!cargo_analysis.dependencies[0].is_dead);

        let npm_analysis = results
            .iter()
            .find(|r| r.manifest_type == ManifestType::PackageJson)
            .unwrap();
        assert!(!npm_analysis.dependencies[0].is_dead);
    }
}
