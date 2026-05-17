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
            _ => Vec::new(),
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
/// Reads `[package].name` and `[workspace].members` entries, normalises
/// hyphens to underscores, and returns the combined list.
///
/// For literal workspace members like `crates/seshat-core`, the inner
/// `Cargo.toml`'s `[package].name` is read when available; otherwise the
/// last path component is used as a fallback (preserves the historical
/// behaviour for callers that pass in-memory manifest content with no
/// matching directories on disk).
///
/// Glob patterns (e.g. `crates/*`) are expanded relative to the directory
/// containing this `Cargo.toml` via [`expand_glob_member`]. For each matched
/// dir the inner `Cargo.toml` is required — dirs that lack one (or whose
/// inner manifest cannot be read) are silently skipped, matching Cargo's own
/// behaviour.
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
            let is_glob = is_glob_pattern(member);
            let dirs: Vec<PathBuf> = if is_glob {
                expand_glob_member(manifest_dir, member)
            } else {
                vec![manifest_dir.join(member)]
            };
            for dir in dirs {
                let crate_name = match read_inner_crate_name(&dir.join("Cargo.toml")) {
                    Some(name) => name,
                    None if is_glob => {
                        // Glob-expanded dir without a readable Cargo.toml — skip
                        // silently, matching Cargo's own workspace-member semantics.
                        continue;
                    }
                    None => match dir.file_name().and_then(|n| n.to_str()) {
                        Some(name) if !name.is_empty() => name.to_owned(),
                        _ => continue,
                    },
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
/// Returns the matched directory paths (non-directory entries are filtered out,
/// mirroring Cargo's own glob semantics for workspace members). Invalid glob
/// patterns or non-UTF8 paths emit a `tracing::warn!` and yield an empty `Vec`
/// rather than panicking — this keeps a malformed `Cargo.toml` from poisoning
/// the whole scan.
fn expand_glob_member(manifest_dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let joined = manifest_dir.join(pattern);
    let Some(pattern_str) = joined.to_str() else {
        tracing::warn!(
            pattern = %pattern,
            manifest_dir = %manifest_dir.display(),
            "Non-UTF8 path while expanding workspace-member glob; skipping",
        );
        return Vec::new();
    };
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
    paths
        .filter_map(Result::ok)
        .filter(|p| p.is_dir())
        .collect()
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
        let content = r#"
[workspace]
members = ["crates/core", "crates/api"]
"#;
        let names = extract_crate_names(Path::new("Cargo.toml"), content);
        assert_eq!(names, vec!["core", "api"]);
    }

    #[test]
    fn extract_crate_names_workspace_and_root_package() {
        let content = r#"
[package]
name = "seshat-root"
version = "0.1.0"

[workspace]
members = ["crates/seshat-core", "crates/seshat-graph"]
"#;
        let names = extract_crate_names(Path::new("Cargo.toml"), content);
        // [package].name comes first, then workspace members
        assert!(names.contains(&"seshat_root".to_owned()));
        assert!(names.contains(&"seshat_core".to_owned()));
        assert!(names.contains(&"seshat_graph".to_owned()));
        assert_eq!(names.len(), 3);
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
        let mut names = extract_crate_names(&manifest_path, content);
        names.sort();
        assert_eq!(
            names,
            vec!["foo".to_owned(), "legacy_crate".to_owned()],
            "mixed literal + glob members should both resolve via the inner Cargo.toml",
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
        let paths = expand_glob_member(tmp.path(), "crates/[");
        assert!(
            paths.is_empty(),
            "invalid glob pattern must yield empty Vec, not panic",
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
    fn analyze_manifests_package_json_has_empty_internal_names() {
        let content = r#"{"name": "my-app", "dependencies": {}}"#;
        let manifests = vec![(
            PathBuf::from("package.json"),
            content.to_owned(),
            ManifestType::PackageJson,
        )];
        let results = analyze_manifests(&manifests, &[]).unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            results[0].internal_names.is_empty(),
            "package.json should yield no internal_names"
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
