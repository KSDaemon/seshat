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
        results.push(ManifestAnalysis {
            manifest_path: path.clone(),
            manifest_type: *manifest_type,
            dependencies: stats,
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
