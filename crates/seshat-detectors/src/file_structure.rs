//! File structure detector — directory organization patterns.
//!
//! Detects directory organization conventions by analyzing file paths across the
//! project. Since the [`ConventionDetector`] trait operates per-file, this
//! detector classifies each file's path into structural categories and emits
//! findings that are meaningful when aggregated across the full file set.
//!
//! ## Design Decision (per-file trait vs project-wide analysis)
//!
//! The `ConventionDetector::detect()` method receives one file at a time, but
//! directory organization analysis inherently requires a project-wide view. We
//! chose **per-file classification with pipeline aggregation** rather than
//! interior mutability or context injection:
//!
//! 1. Each `detect()` call classifies the file's path and emits findings about
//!    which organizational pattern the path follows (by-feature, by-type,
//!    by-layer).
//! 2. The existing [`aggregate_findings`](crate::confidence::aggregate_findings)
//!    function groups these per-file findings by `(detector_name, description)`
//!    to compute project-wide adoption counts and confidence scores.
//! 3. This approach requires **no changes** to the core trait or pipeline, keeps
//!    the detector stateless and `Send + Sync`, and is fully compatible with
//!    parallel file processing via rayon.

use std::path::Path;

use seshat_core::{
    AnchorKind, CodeEvidence, ConventionFinding, FindingKind, KnowledgeNature, Language,
    ProjectFile,
};

use crate::trait_def::ConventionDetector;

const DETECTOR_NAME: &str = "file_structure";

/// Maximum number of evidence entries per finding.
const MAX_EVIDENCE: usize = 5;

// ---------------------------------------------------------------------------
// Well-known directory names for classification
// ---------------------------------------------------------------------------

/// Directories that signal "by type" organization (grouping by what things are).
const TYPE_DIRS: &[&str] = &[
    "models",
    "controllers",
    "services",
    "views",
    "handlers",
    "repositories",
    "middleware",
    "validators",
    "serializers",
    "schemas",
    "entities",
    "dtos",
    "mappers",
    "resolvers",
    "guards",
    "pipes",
    "interceptors",
    "decorators",
    "adapters",
    "providers",
    "factories",
    "strategies",
];

/// Directories that signal "by layer" / clean-architecture organization.
const LAYER_DIRS: &[&str] = &[
    "domain",
    "infrastructure",
    "application",
    "presentation",
    "core",
    "adapters",
    "ports",
    "usecases",
    "use_cases",
    "use-cases",
    "interfaces",
    "gateway",
    "gateways",
    "persistence",
    "delivery",
];

/// Well-known common directories that are nearly universal.
const COMMON_DIRS: &[&str] = &[
    "src", "lib", "tests", "test", "utils", "helpers", "types", "config", "scripts", "docs",
    "assets", "static", "public", "dist", "build", "bin", "examples", "benches", "fixtures",
];

/// File names / stems that indicate configuration files.
const CONFIG_FILE_PATTERNS: &[&str] = &[
    "tsconfig",
    "jest.config",
    "vitest.config",
    "webpack.config",
    "vite.config",
    "rollup.config",
    "babel.config",
    "eslint",
    ".eslintrc",
    "prettier",
    ".prettierrc",
    "cargo.toml",
    "pyproject.toml",
    "setup.cfg",
    "setup.py",
    "tox.ini",
    "mypy.ini",
    ".flake8",
    "ruff.toml",
    "package.json",
    "docker",
    "dockerfile",
    "makefile",
    ".env",
    ".gitignore",
    ".editorconfig",
];

/// Directories that are typically config-holding directories.
const CONFIG_DIRS: &[&str] = &["config", "configs", "configuration", ".config", "settings"];

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects directory organization patterns by analyzing file paths.
///
/// Patterns detected:
/// - **By feature**: directories named after business domains (users/, orders/)
/// - **By type**: directories named after architectural roles (models/, services/)
/// - **By layer**: directories following clean/hexagonal architecture (domain/, infrastructure/)
/// - **Common directories**: well-known directory conventions (src/, lib/, tests/)
/// - **Config placement**: root-level vs config-directory placement
pub struct FileStructureDetector;

impl ConventionDetector for FileStructureDetector {
    fn name(&self) -> &'static str {
        DETECTOR_NAME
    }

    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
        let mut findings = Vec::new();

        detect_organization_pattern(file, &mut findings);
        detect_common_directories(file, &mut findings);
        detect_config_placement(file, &mut findings);

        findings
    }

    fn supported_languages(&self) -> &[Language] {
        Language::all()
    }
}

// ---------------------------------------------------------------------------
// Organization pattern detection (by-feature / by-type / by-layer)
// ---------------------------------------------------------------------------

/// Classify which organizational pattern this file's path exhibits.
///
/// A file can only vote for one pattern: by-type, by-layer, or by-feature.
/// When both by-type and by-layer directories appear in the path, the one
/// that appears earliest (highest-level) wins. This correctly handles clean
/// architecture layouts like `src/domain/entities/` where `domain` (layer)
/// is the primary organizer and `entities` (type) is a subdivision.
fn detect_organization_pattern(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    let components = path_components(&file.path);
    if components.is_empty() {
        return;
    }

    // Find the earliest by-type and by-layer match positions.
    let type_match = find_matching_component_with_index(&components, TYPE_DIRS);
    let layer_match = find_matching_component_with_index(&components, LAYER_DIRS);

    // Pick the match that appears earliest in the path. If both appear at the
    // same index (shouldn't happen with disjoint lists), prefer by-layer.
    match (type_match, layer_match) {
        (Some((type_idx, type_dir)), Some((layer_idx, layer_dir))) => {
            if layer_idx <= type_idx {
                push_layer_finding(file, layer_dir, findings);
            } else {
                push_type_finding(file, type_dir, findings);
            }
        }
        (Some((_idx, type_dir)), None) => {
            push_type_finding(file, type_dir, findings);
        }
        (None, Some((_idx, layer_dir))) => {
            push_layer_finding(file, layer_dir, findings);
        }
        (None, None) => {
            // No type or layer match — check for by-feature directories.
            if let Some(feature_dir) = find_feature_directory(&components) {
                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: DETECTOR_NAME.to_owned(),
                    nature: KnowledgeNature::Convention,
                    description: "By-feature directory organization (domain-specific directories)"
                        .to_owned(),
                    evidence: vec![CodeEvidence {
                        file: file.path.clone(),
                        line: 0,
                        end_line: 0,
                        snippet: format!("in '{feature_dir}/'"),
                        snippet_start_line: 0,
                        anchor: AnchorKind::FileLevel,
                    }],
                    follows_convention: true,
                    kind: FindingKind::FileStructure,
                });
            }
        }
    }
}

fn push_type_finding(file: &ProjectFile, type_dir: &str, findings: &mut Vec<ConventionFinding>) {
    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: KnowledgeNature::Convention,
        description: "By-type directory organization (models/, controllers/, services/)".to_owned(),
        evidence: vec![CodeEvidence {
            file: file.path.clone(),
            line: 0,
            end_line: 0,
            snippet: format!("in '{type_dir}/'"),
            snippet_start_line: 0,
            anchor: AnchorKind::FileLevel,
        }],
        follows_convention: true,
        kind: FindingKind::FileStructure,
    });
}

fn push_layer_finding(file: &ProjectFile, layer_dir: &str, findings: &mut Vec<ConventionFinding>) {
    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: KnowledgeNature::Convention,
        description: "By-layer directory organization (domain/, infrastructure/, application/)"
            .to_owned(),
        evidence: vec![CodeEvidence {
            file: file.path.clone(),
            line: 0,
            end_line: 0,
            snippet: format!("in '{layer_dir}/'"),
            snippet_start_line: 0,
            anchor: AnchorKind::FileLevel,
        }],
        follows_convention: true,
        kind: FindingKind::FileStructure,
    });
}

/// Find the first path component that matches one of the given well-known names,
/// returning its index and value.
fn find_matching_component_with_index<'a>(
    components: &[&'a str],
    known: &[&str],
) -> Option<(usize, &'a str)> {
    components
        .iter()
        .enumerate()
        .find(|(_idx, c)| known.contains(&c.to_lowercase().as_str()))
        .map(|(idx, &c)| (idx, c))
}

/// Find a plausible feature directory — a non-common, non-type, non-layer
/// directory that sits under a source root (src/, lib/, app/, crate root).
fn find_feature_directory<'a>(components: &[&'a str]) -> Option<&'a str> {
    // Find the first meaningful directory after a source root.
    let source_roots = ["src", "lib", "app", "crates", "packages"];
    let mut after_root = false;

    for component in components {
        let lower = component.to_lowercase();
        let lower_str = lower.as_str();

        if source_roots.contains(&lower_str) {
            after_root = true;
            continue;
        }

        if after_root && is_potential_feature_dir(lower_str) {
            return Some(component);
        }
    }

    None
}

/// Check if a directory name looks like a feature/domain name (not a well-known
/// type, layer, common, or config directory).
fn is_potential_feature_dir(name: &str) -> bool {
    !TYPE_DIRS.contains(&name)
        && !LAYER_DIRS.contains(&name)
        && !COMMON_DIRS.contains(&name)
        && !CONFIG_DIRS.contains(&name)
        && !name.starts_with('.')
        && !name.starts_with('_')
        && name.len() > 1
}

// ---------------------------------------------------------------------------
// Common directory detection
// ---------------------------------------------------------------------------

/// Detect well-known directory conventions the file participates in.
///
/// Reports the most specific (deepest) common directory in the path, which
/// gives the most informative signal about the file's purpose (e.g. `utils/`
/// is more informative than `src/` for `src/utils/helpers.ts`).
fn detect_common_directories(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    let components = path_components(&file.path);

    // Find the deepest (last) common directory in the path.
    let deepest = components
        .iter()
        .rev()
        .find(|c| COMMON_DIRS.contains(&c.to_lowercase().as_str()));

    if let Some(&component) = deepest {
        let description = format!("Uses '{component}/' directory convention");
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description,
            evidence: path_evidence(file, MAX_EVIDENCE),
            follows_convention: true,
            kind: FindingKind::FileStructure,
        });
    }
}

// ---------------------------------------------------------------------------
// Configuration file placement
// ---------------------------------------------------------------------------

/// Detect whether configuration files live at the project root or in a config
/// directory.
fn detect_config_placement(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    let file_name = match file.path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_lowercase(),
        None => return,
    };

    let file_stem = file
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let is_config = CONFIG_FILE_PATTERNS
        .iter()
        .any(|p| file_name.starts_with(p) || file_stem.starts_with(p));

    if !is_config {
        return;
    }

    let components = path_components(&file.path);
    let in_config_dir = components
        .iter()
        .any(|c| CONFIG_DIRS.contains(&c.to_lowercase().as_str()));

    if in_config_dir {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Configuration files placed in config directory".to_owned(),
            evidence: path_evidence(file, MAX_EVIDENCE),
            follows_convention: true,
            kind: FindingKind::FileStructure,
        });
    } else if components.len() <= 1 {
        // File is at root level (no parent directories, or just one level).
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Configuration files placed at project root".to_owned(),
            evidence: path_evidence(file, MAX_EVIDENCE),
            follows_convention: true,
            kind: FindingKind::FileStructure,
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract directory components from a path (excluding the file name).
fn path_components(path: &Path) -> Vec<&str> {
    path.parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(os) => os.to_str(),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build evidence from the file path.
///
/// The composite snippet renderer prints the full file path on each row,
/// so this evidence carries no per-file descriptor — leaving `snippet`
/// empty avoids the `(Path: <same-path>)` duplication users see in TUI.
fn path_evidence(file: &ProjectFile, max: usize) -> Vec<CodeEvidence> {
    if max == 0 {
        return Vec::new();
    }
    vec![CodeEvidence {
        file: file.path.clone(),
        line: 0,
        end_line: 0,
        snippet: String::new(),
        snippet_start_line: 0,
        anchor: AnchorKind::FileLevel,
    }]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{JavaScriptIR, Language, PythonIR, RustIR, TypeScriptIR};
    use std::path::PathBuf;

    fn make_file(path: &str, language: Language) -> ProjectFile {
        let language_ir = match language {
            Language::Rust => LanguageIR::Rust(RustIR::default()),
            Language::TypeScript => LanguageIR::TypeScript(TypeScriptIR::default()),
            Language::JavaScript => LanguageIR::JavaScript(JavaScriptIR::default()),
            Language::Python => LanguageIR::Python(PythonIR::default()),
        };
        ProjectFile {
            path: PathBuf::from(path),
            language,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir,
            file_doc: None,
        }
    }

    fn make_rust_file(path: &str) -> ProjectFile {
        make_file(path, Language::Rust)
    }

    fn make_ts_file(path: &str) -> ProjectFile {
        make_file(path, Language::TypeScript)
    }

    fn make_js_file(path: &str) -> ProjectFile {
        make_file(path, Language::JavaScript)
    }

    fn make_py_file(path: &str) -> ProjectFile {
        make_file(path, Language::Python)
    }

    // --- Trait basics ---

    #[test]
    fn detector_name() {
        let d = FileStructureDetector;
        assert_eq!(d.name(), "file_structure");
    }

    #[test]
    fn supports_all_languages() {
        let d = FileStructureDetector;
        assert_eq!(d.supported_languages().len(), 4);
    }

    #[test]
    fn empty_path_no_findings() {
        let d = FileStructureDetector;
        let file = make_rust_file("main.rs");
        let findings = d.detect(&file);
        // A root-level file with no directory components produces no org pattern
        // findings and no common dir findings.
        assert!(
            findings.is_empty()
                || findings
                    .iter()
                    .all(|f| f.description.contains("Configuration")),
            "root-level non-config file should produce minimal findings"
        );
    }

    // --- By-type organization ---

    #[test]
    fn detects_by_type_models_dir() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/models/user.ts");
        let findings = d.detect(&file);
        let org_finding = findings.iter().find(|f| f.description.contains("By-type"));
        assert!(org_finding.is_some(), "should detect by-type pattern");
        assert!(org_finding.unwrap().follows_convention);
        assert_eq!(org_finding.unwrap().nature, KnowledgeNature::Convention);
    }

    #[test]
    fn detects_by_type_controllers_dir() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/controllers/auth_controller.ts");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-type")));
    }

    #[test]
    fn detects_by_type_services_dir() {
        let d = FileStructureDetector;
        let file = make_rust_file("src/services/auth.rs");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-type")));
    }

    #[test]
    fn detects_by_type_handlers_dir() {
        let d = FileStructureDetector;
        let file = make_py_file("src/handlers/webhook.py");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-type")));
    }

    #[test]
    fn detects_by_type_middleware_dir() {
        let d = FileStructureDetector;
        let file = make_js_file("src/middleware/auth.js");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-type")));
    }

    // --- By-layer organization ---

    #[test]
    fn detects_by_layer_domain_dir() {
        let d = FileStructureDetector;
        let file = make_rust_file("src/domain/entities/user.rs");
        let findings = d.detect(&file);
        let org_finding = findings.iter().find(|f| f.description.contains("By-layer"));
        assert!(org_finding.is_some(), "should detect by-layer pattern");
        assert!(org_finding.unwrap().follows_convention);
    }

    #[test]
    fn detects_by_layer_infrastructure_dir() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/infrastructure/database/pg_client.ts");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-layer")));
    }

    #[test]
    fn detects_by_layer_application_dir() {
        let d = FileStructureDetector;
        let file = make_py_file("src/application/use_cases/create_user.py");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-layer")));
    }

    #[test]
    fn detects_by_layer_ports_dir() {
        let d = FileStructureDetector;
        let file = make_rust_file("src/ports/http.rs");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-layer")));
    }

    // --- By-feature organization ---

    #[test]
    fn detects_by_feature_users_dir() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/users/user_service.ts");
        let findings = d.detect(&file);
        let org_finding = findings
            .iter()
            .find(|f| f.description.contains("By-feature"));
        assert!(org_finding.is_some(), "should detect by-feature pattern");
        assert!(org_finding.unwrap().follows_convention);
    }

    #[test]
    fn detects_by_feature_orders_dir() {
        let d = FileStructureDetector;
        let file = make_py_file("src/orders/create_order.py");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("By-feature")),
            "should detect by-feature for 'orders' dir"
        );
    }

    #[test]
    fn detects_by_feature_under_lib() {
        let d = FileStructureDetector;
        let file = make_js_file("lib/payments/stripe.js");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("By-feature")),
            "should detect by-feature for dirs under lib/"
        );
    }

    #[test]
    fn no_feature_for_hidden_dir() {
        let d = FileStructureDetector;
        // .hidden dirs should not be classified as feature dirs
        let file = make_rust_file("src/.internal/secret.rs");
        let findings = d.detect(&file);
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("By-feature")),
            "hidden directories should not be classified as features"
        );
    }

    // --- Mutual exclusivity: by-type wins over by-feature ---

    #[test]
    fn by_type_wins_over_by_feature() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/services/user_service.ts");
        let findings = d.detect(&file);
        // Should detect by-type (services) but NOT by-feature
        assert!(findings.iter().any(|f| f.description.contains("By-type")));
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("By-feature"))
        );
    }

    #[test]
    fn by_layer_wins_over_by_feature() {
        let d = FileStructureDetector;
        let file = make_rust_file("src/domain/user.rs");
        let findings = d.detect(&file);
        assert!(findings.iter().any(|f| f.description.contains("By-layer")));
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("By-feature"))
        );
    }

    // --- Common directories ---

    #[test]
    fn detects_src_directory() {
        let d = FileStructureDetector;
        let file = make_rust_file("src/main.rs");
        let findings = d.detect(&file);
        let common = findings
            .iter()
            .find(|f| f.description.contains("'src/' directory"));
        assert!(common.is_some(), "should detect src/ common directory");
        assert_eq!(common.unwrap().nature, KnowledgeNature::Observation);
    }

    #[test]
    fn detects_tests_directory() {
        let d = FileStructureDetector;
        let file = make_py_file("tests/test_auth.py");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("'tests/' directory")),
            "should detect tests/ common directory"
        );
    }

    #[test]
    fn detects_utils_directory() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/utils/helpers.ts");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("'utils/' directory")),
            "should detect utils/ common directory"
        );
    }

    #[test]
    fn detects_lib_directory() {
        let d = FileStructureDetector;
        let file = make_js_file("lib/core.js");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("'lib/' directory")),
            "should detect lib/ common directory"
        );
    }

    // --- Configuration file placement ---

    #[test]
    fn config_at_root() {
        let d = FileStructureDetector;
        let file = make_ts_file("tsconfig.json");
        let findings = d.detect(&file);
        let config_finding = findings
            .iter()
            .find(|f| f.description.contains("project root"));
        assert!(
            config_finding.is_some(),
            "should detect config at project root"
        );
    }

    #[test]
    fn config_in_config_dir() {
        let d = FileStructureDetector;
        let file = make_ts_file("config/tsconfig.json");
        let findings = d.detect(&file);
        let config_finding = findings
            .iter()
            .find(|f| f.description.contains("config directory"));
        assert!(
            config_finding.is_some(),
            "should detect config in config directory"
        );
    }

    #[test]
    fn cargo_toml_at_root() {
        let d = FileStructureDetector;
        let file = make_rust_file("Cargo.toml");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("project root")),
            "Cargo.toml at root should be detected"
        );
    }

    #[test]
    fn package_json_at_root() {
        let d = FileStructureDetector;
        let file = make_js_file("package.json");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("project root")),
            "package.json at root should be detected"
        );
    }

    #[test]
    fn eslint_config_at_root() {
        let d = FileStructureDetector;
        let file = make_js_file(".eslintrc.json");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("project root")),
            ".eslintrc at root should be detected"
        );
    }

    #[test]
    fn pyproject_toml_at_root() {
        let d = FileStructureDetector;
        let file = make_py_file("pyproject.toml");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("project root")),
            "pyproject.toml at root should be detected"
        );
    }

    #[test]
    fn non_config_file_no_config_finding() {
        let d = FileStructureDetector;
        let file = make_rust_file("src/main.rs");
        let findings = d.detect(&file);
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("Configuration")),
            "non-config file should not produce config placement finding"
        );
    }

    #[test]
    fn dockerfile_at_root() {
        let d = FileStructureDetector;
        let file = make_rust_file("Dockerfile");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("project root")),
            "Dockerfile at root should be detected"
        );
    }

    #[test]
    fn makefile_at_root() {
        let d = FileStructureDetector;
        let file = make_py_file("Makefile");
        let findings = d.detect(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("project root")),
            "Makefile at root should be detected"
        );
    }

    // --- Cross-language dispatch ---

    #[test]
    fn works_for_all_languages() {
        let d = FileStructureDetector;
        let files = vec![
            make_rust_file("src/services/auth.rs"),
            make_ts_file("src/services/auth.ts"),
            make_js_file("src/services/auth.js"),
            make_py_file("src/services/auth.py"),
        ];
        for file in &files {
            let findings = d.detect(file);
            assert!(
                findings.iter().any(|f| f.description.contains("By-type")),
                "should detect by-type for {:?}",
                file.path
            );
        }
    }

    // --- Path component extraction ---

    #[test]
    fn path_components_simple() {
        let comps = path_components(Path::new("src/models/user.rs"));
        assert_eq!(comps, vec!["src", "models"]);
    }

    #[test]
    fn path_components_deep() {
        let comps = path_components(Path::new("src/domain/entities/user.rs"));
        assert_eq!(comps, vec!["src", "domain", "entities"]);
    }

    #[test]
    fn path_components_root_file() {
        let comps = path_components(Path::new("main.rs"));
        assert!(comps.is_empty());
    }

    // --- Edge cases ---

    #[test]
    fn deeply_nested_by_type() {
        let d = FileStructureDetector;
        let file = make_ts_file("packages/api/src/models/user/index.ts");
        let findings = d.detect(&file);
        assert!(
            findings.iter().any(|f| f.description.contains("By-type")),
            "should detect by-type even in deeply nested paths"
        );
    }

    #[test]
    fn config_nested_deeply_not_at_root_or_config_dir() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/modules/auth/tsconfig.json");
        let findings = d.detect(&file);
        // Not at root and not in a config directory — no config placement finding
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("Configuration")),
            "config file deeply nested should not trigger config placement"
        );
    }

    #[test]
    fn crates_directory_with_feature() {
        let d = FileStructureDetector;
        let file = make_rust_file("crates/auth/src/lib.rs");
        let findings = d.detect(&file);
        // "auth" under "crates" should be detected as by-feature
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("By-feature")),
            "should detect by-feature for crate name under crates/"
        );
    }

    #[test]
    fn evidence_has_file_path() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/models/user.ts");
        let findings = d.detect(&file);
        let org_finding = findings
            .iter()
            .find(|f| f.description.contains("By-type"))
            .expect("should have by-type finding");
        assert!(
            !org_finding.evidence.is_empty(),
            "finding should have evidence"
        );
        assert!(
            org_finding.evidence[0].snippet.contains("models"),
            "evidence should mention the directory"
        );
    }

    #[test]
    fn all_findings_have_correct_detector_name() {
        let d = FileStructureDetector;
        let file = make_ts_file("src/models/user.ts");
        let findings = d.detect(&file);
        for finding in &findings {
            assert_eq!(finding.detector_name, DETECTOR_NAME);
        }
    }

    #[test]
    fn detect_with_source_sets_real_snippet() {
        let d = FileStructureDetector;
        // All file_structure evidence has line:0, so detect_with_source
        // delegates to detect() and snippets remain as-is (path-based descriptions).
        let file = make_rust_file("src/models/user.rs");
        let source = "// some Rust source\npub struct User {}\n";

        let findings = d.detect_with_source(&file, source);

        // detect_with_source produces the same results as detect() for this detector.
        let findings_ir_only = d.detect(&file);
        assert_eq!(
            findings.len(),
            findings_ir_only.len(),
            "detect_with_source should return same findings as detect() for file_structure"
        );

        assert!(!findings.is_empty(), "should have at least one finding");
        for finding in &findings {
            assert_eq!(finding.file_path, file.path);
            for ev in &finding.evidence {
                // All evidence has line: 0 (file-level signal, no source line).
                assert_eq!(ev.line, 0, "file_structure evidence should have line:0");
                assert_eq!(ev.file, file.path);
                // Snippet for file_structure is path-based (contains the file path component)
                // or empty — never a synthetic "Custom ..." string.
                assert!(
                    !ev.snippet.starts_with("Custom "),
                    "snippet must not be a synthetic format string, got: {:?}",
                    ev.snippet
                );
                // If non-empty, snippet must reference the file path or a directory component.
                if !ev.snippet.is_empty() {
                    assert!(
                        ev.snippet.contains("src")
                            || ev.snippet.contains("models")
                            || ev.snippet.contains("user"),
                        "non-empty file_structure snippet must reference path components, got: {:?}",
                        ev.snippet
                    );
                }
            }
        }
    }
}
