//! Test patterns detector — framework, file placement, naming conventions.
//!
//! Identifies the testing framework in use, detects test file placement
//! conventions (co-located vs separate `tests/` directory), test naming
//! patterns, and setup/teardown approaches. Uses [`DependencyUsage`] and
//! [`Import`] entries to identify test framework imports, and function/type
//! names to identify test structure.
//!
//! **Heuristic fallbacks** (added in US-011):
//! - **Config file detection**: Recognizes `jest.config.*`, `vitest.config.*`,
//!   `[tool.pytest]` in `pyproject.toml` to infer framework without imports.
//! - **Unknown framework fallback**: Files in test directories with test-prefixed
//!   functions but no identifiable framework → Observation finding.
//! - **Dependency name heuristic**: Dependency names containing `test`, `mock`,
//!   `assert`, or `spec` → Observation finding for testing-related dependency.
//!
//! Supported languages: Rust, TypeScript, JavaScript, Python.

use std::collections::HashSet;
use std::path::Path;

use seshat_core::{
    AnchorKind, CodeEvidence, ConventionFinding, DependencyUsage, FindingKind, Function, Import,
    KnowledgeNature, Language, LanguageIR, ProjectFile, PythonIR,
};

use crate::trait_def::ConventionDetector;
use crate::usage_evidence::find_usage_evidence_for_file_scoped;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DETECTOR_NAME: &str = "test_patterns";

/// Maximum number of evidence entries per finding.
const MAX_EVIDENCE: usize = 5;

// ---------------------------------------------------------------------------
// Test framework classification
// ---------------------------------------------------------------------------

/// Known testing framework family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TestFramework {
    // Rust
    RustBuiltin,
    // JS/TS
    Jest,
    Vitest,
    Mocha,
    // Python
    Pytest,
    Unittest,
}

impl TestFramework {
    /// Human-readable name for finding descriptions.
    fn as_str(self) -> &'static str {
        match self {
            Self::RustBuiltin => "built-in #[test]",
            Self::Jest => "Jest",
            Self::Vitest => "Vitest",
            Self::Mocha => "Mocha",
            Self::Pytest => "pytest",
            Self::Unittest => "unittest",
        }
    }
}

/// Classify a JS/TS package as a test framework.
fn classify_js_ts_test_framework(package: &str) -> Option<TestFramework> {
    match package {
        "jest"
        | "@jest/core"
        | "@jest/globals"
        | "ts-jest"
        | "jest-cli"
        | "@testing-library/react"
        | "@testing-library/jest-dom"
        | "@testing-library/vue"
        | "@testing-library/angular" => Some(TestFramework::Jest),
        "vitest" | "@vitest/runner" | "@vitest/expect" => Some(TestFramework::Vitest),
        "mocha" | "@types/mocha" | "chai" | "@types/chai" => Some(TestFramework::Mocha),
        _ => None,
    }
}

/// Classify a Python package as a test framework.
fn classify_python_test_framework(package: &str) -> Option<TestFramework> {
    match package {
        "pytest" | "pytest-asyncio" | "pytest-cov" | "pytest-mock" | "pytest-xdist"
        | "pytest-timeout" | "pytest-benchmark" => Some(TestFramework::Pytest),
        "unittest" => Some(TestFramework::Unittest),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Config file detection heuristic
// ---------------------------------------------------------------------------

/// Test framework configuration file patterns.
///
/// Used to infer testing framework from config files in the project, even if
/// no framework-specific imports are present in the current file.
const JEST_CONFIG_PREFIXES: &[&str] = &["jest.config."];
const VITEST_CONFIG_PREFIXES: &[&str] = &["vitest.config."];

/// Check if a file path looks like a testing framework config file.
/// Returns the inferred [`TestFramework`] if matched.
fn detect_config_file_framework(path: &str) -> Option<TestFramework> {
    let filename = path.rsplit('/').next().unwrap_or(path);

    // Jest config: jest.config.js, jest.config.ts, jest.config.mjs, etc.
    if JEST_CONFIG_PREFIXES
        .iter()
        .any(|prefix| filename.starts_with(prefix))
    {
        return Some(TestFramework::Jest);
    }

    // Vitest config: vitest.config.ts, vitest.config.js, etc.
    if VITEST_CONFIG_PREFIXES
        .iter()
        .any(|prefix| filename.starts_with(prefix))
    {
        return Some(TestFramework::Vitest);
    }

    // pytest: pyproject.toml is handled separately (needs content analysis),
    // but the file name "conftest.py" is a strong pytest indicator.
    if filename == "conftest.py" {
        return Some(TestFramework::Pytest);
    }

    None
}

// ---------------------------------------------------------------------------
// Dependency name heuristic
// ---------------------------------------------------------------------------

/// Keywords in dependency/import names that suggest a testing-related package.
const TEST_DEP_HINTS: &[&str] = &["test", "mock", "assert", "spec"];

/// Check if a dependency or import name looks like a testing-related package
/// based on keyword heuristics.
///
/// Returns `true` if the name contains any testing keyword AT A WORD
/// BOUNDARY AND is NOT already classified as a known test framework.
/// Word-boundary matching prevents substring false positives — e.g.
/// `inspect` no longer matches "spec", `request_id` no longer matches
/// "test", `timestamp` no longer matches "test". Matches the same
/// boundary rules used by `dependency_usage::classify_heuristic_domain`:
/// start-of-string, after `_` / `-`, or at a camelCase transition.
fn is_heuristic_test_dep(package: &str, language: Language) -> bool {
    // Skip if it's already a known framework
    match language {
        Language::Rust => {
            // Rust built-in test is not a dependency, skip known Rust test crates
            if is_known_rust_test_dep(package) {
                return false;
            }
        }
        Language::TypeScript | Language::JavaScript => {
            if classify_js_ts_test_framework(package).is_some() {
                return false;
            }
        }
        Language::Python => {
            if classify_python_test_framework(package).is_some() {
                return false;
            }
        }
    }

    keyword_at_word_boundary(package, TEST_DEP_HINTS)
}

/// True when any of `keywords` appears in `package` at a word boundary.
///
/// Boundaries: start-of-string, the byte after `_` / `-`, or a
/// camelCase transition (lowercase byte → uppercase byte). ASCII-only
/// — non-ASCII bytes degrade gracefully (their boundary checks return
/// false, so we never panic on UTF-8 byte-index drift).
fn keyword_at_word_boundary(package: &str, keywords: &[&str]) -> bool {
    let lower = package.to_ascii_lowercase();
    let bytes = package.as_bytes();
    for kw in keywords {
        let mut search_start = 0usize;
        while let Some(pos) = lower[search_start..].find(kw) {
            let abs_pos = search_start + pos;
            let prev = abs_pos.checked_sub(1).and_then(|i| bytes.get(i)).copied();
            let curr = bytes.get(abs_pos).copied();
            let is_boundary = abs_pos == 0
                || prev.is_some_and(|b| b == b'_' || b == b'-')
                || (prev.is_some_and(|b| b.is_ascii_lowercase())
                    && curr.is_some_and(|b| b.is_ascii_uppercase()));
            if is_boundary {
                return true;
            }
            search_start = abs_pos + 1;
        }
    }
    false
}

/// Check if a Rust dependency is a known testing crate.
fn is_known_rust_test_dep(package: &str) -> bool {
    matches!(
        package,
        "proptest"
            | "quickcheck"
            | "rstest"
            | "criterion"
            | "test-case"
            | "mockall"
            | "wiremock"
            | "assert_cmd"
            | "assert_fs"
            | "insta"
    )
}

// ---------------------------------------------------------------------------
// Test file detection heuristics
// ---------------------------------------------------------------------------

/// Test file placement category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestPlacement {
    /// Co-located: test file sits next to its source (e.g., `foo.test.ts`,
    /// Rust inline `#[cfg(test)] mod tests`).
    CoLocated,
    /// Separate: test file lives in a dedicated `tests/` or `__tests__/` directory.
    Separate,
}

/// Determine whether a file path indicates a test file, and if so, its placement.
fn detect_test_file_placement(path: &str, language: Language) -> Option<TestPlacement> {
    let normalized = path.replace('\\', "/");

    match language {
        Language::Rust => {
            // `tests/` directory at any level → separate
            if normalized.contains("/tests/") || normalized.starts_with("tests/") {
                return Some(TestPlacement::Separate);
            }
            // Files ending in `_test.rs` are co-located test files
            if normalized.ends_with("_test.rs") {
                return Some(TestPlacement::CoLocated);
            }
            // Inline `#[cfg(test)] mod tests` in a source file → co-located
            // (detected via function analysis, not path — handled separately)
            None
        }
        Language::TypeScript | Language::JavaScript => {
            // `__tests__/` directory → separate
            if normalized.contains("/__tests__/") || normalized.starts_with("__tests__/") {
                return Some(TestPlacement::Separate);
            }
            // `tests/` or `test/` directory → separate
            if normalized.contains("/tests/")
                || normalized.contains("/test/")
                || normalized.starts_with("tests/")
                || normalized.starts_with("test/")
            {
                return Some(TestPlacement::Separate);
            }
            // Co-located patterns: *.test.ts, *.spec.ts, *.test.js, *.spec.js
            let stem = normalized.rsplit('/').next().unwrap_or(&normalized);
            if stem.contains(".test.") || stem.contains(".spec.") {
                return Some(TestPlacement::CoLocated);
            }
            None
        }
        Language::Python => {
            // `tests/` directory → separate
            if normalized.contains("/tests/") || normalized.starts_with("tests/") {
                return Some(TestPlacement::Separate);
            }
            // `test/` directory → separate
            if normalized.contains("/test/") || normalized.starts_with("test/") {
                return Some(TestPlacement::Separate);
            }
            // Co-located: test_*.py or *_test.py in same directory as source
            let stem = normalized.rsplit('/').next().unwrap_or(&normalized);
            if stem.starts_with("test_") || stem.ends_with("_test.py") {
                // If not under a tests/ directory, it's co-located
                return Some(TestPlacement::CoLocated);
            }
            None
        }
    }
}

/// Check whether a Rust file has inline test modules by looking at functions
/// named with `test` prefix and the `#[cfg(test)]` mod pattern.
fn has_rust_inline_tests(file: &ProjectFile) -> bool {
    // Rust inline tests typically have functions starting with "test_"
    // within a `mod tests` block.
    if let LanguageIR::Rust(ref ir) = file.language_ir {
        // Accept both `mod tests` (plural, most common) and `mod test` (singular).
        if ir
            .mod_declarations
            .iter()
            .any(|m| m.name == "tests" || m.name == "test")
        {
            return true;
        }
    }

    // Also check if there are test-like functions in the file.
    file.functions.iter().any(|f| f.name.starts_with("test_"))
}

// ---------------------------------------------------------------------------
// Test naming pattern detection
// ---------------------------------------------------------------------------

/// Detected test naming style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestNamingStyle {
    /// `test_something` (Rust, Python)
    TestPrefix,
    /// `describe`/`it` block style (Jest, Mocha, Vitest)
    DescribeIt,
    /// `test('name', ...)` top-level test function (Jest, Vitest)
    TestFunction,
    /// `class TestFoo` pattern (Python unittest)
    TestClass,
}

impl TestNamingStyle {
    fn as_str(self) -> &'static str {
        match self {
            Self::TestPrefix => "test_* prefix",
            Self::DescribeIt => "describe/it blocks",
            Self::TestFunction => "test() function calls",
            Self::TestClass => "TestClass classes",
        }
    }
}

/// Detect test naming patterns from function names.
fn detect_test_naming(functions: &[Function], language: Language) -> Vec<(TestNamingStyle, usize)> {
    let mut styles: Vec<(TestNamingStyle, usize)> = Vec::new();

    match language {
        Language::Rust => {
            let test_prefix_count = functions
                .iter()
                .filter(|f| f.name.starts_with("test_"))
                .count();
            if test_prefix_count > 0 {
                styles.push((TestNamingStyle::TestPrefix, test_prefix_count));
            }
        }
        Language::TypeScript | Language::JavaScript => {
            let describe_count = functions.iter().filter(|f| f.name == "describe").count();
            let it_count = functions
                .iter()
                .filter(|f| f.name == "it" || f.name == "should")
                .count();
            let test_fn_count = functions.iter().filter(|f| f.name == "test").count();

            if describe_count > 0 || it_count > 0 {
                styles.push((TestNamingStyle::DescribeIt, describe_count + it_count));
            }
            if test_fn_count > 0 {
                styles.push((TestNamingStyle::TestFunction, test_fn_count));
            }
        }
        Language::Python => {
            let test_prefix_count = functions
                .iter()
                .filter(|f| f.name.starts_with("test_"))
                .count();
            if test_prefix_count > 0 {
                styles.push((TestNamingStyle::TestPrefix, test_prefix_count));
            }
        }
    }

    styles
}

// ---------------------------------------------------------------------------
// Setup/teardown pattern detection
// ---------------------------------------------------------------------------

/// Detected setup/teardown pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupPattern {
    /// `beforeEach`/`afterEach`/`beforeAll`/`afterAll` (Jest, Mocha, Vitest)
    Hooks,
    /// `@pytest.fixture` (Python)
    PytestFixtures,
    /// `setUp`/`tearDown` methods (Python unittest)
    SetUpTearDown,
    /// Builder/factory pattern for test data
    TestBuilder,
}

impl SetupPattern {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hooks => "beforeEach/afterEach hooks",
            Self::PytestFixtures => "@pytest.fixture decorators",
            Self::SetUpTearDown => "setUp/tearDown methods",
            Self::TestBuilder => "test builder/factory pattern",
        }
    }
}

/// Detect setup/teardown patterns from functions and IR.
fn detect_setup_patterns(file: &ProjectFile) -> Vec<SetupPattern> {
    let mut patterns = Vec::new();

    match file.language {
        Language::Rust => {
            // Rust doesn't have built-in setup/teardown. Check for builder patterns.
            let has_builders = file
                .functions
                .iter()
                .any(|f| f.name.starts_with("setup") || f.name.starts_with("make_"));
            if has_builders {
                patterns.push(SetupPattern::TestBuilder);
            }
        }
        Language::TypeScript | Language::JavaScript => {
            let hook_names = ["beforeEach", "afterEach", "beforeAll", "afterAll"];
            let has_hooks = file
                .functions
                .iter()
                .any(|f| hook_names.contains(&f.name.as_str()));
            if has_hooks {
                patterns.push(SetupPattern::Hooks);
            }

            // Check for factory/builder functions.
            let has_builders = file
                .functions
                .iter()
                .any(|f| f.name.starts_with("create") || f.name.starts_with("make"));
            if has_builders {
                patterns.push(SetupPattern::TestBuilder);
            }
        }
        Language::Python => {
            // Check for pytest fixtures via decorators in PythonIR.
            if let LanguageIR::Python(ref ir) = file.language_ir {
                let has_fixtures = ir
                    .decorators
                    .iter()
                    .any(|d| d.starts_with("pytest.fixture"));
                if has_fixtures {
                    patterns.push(SetupPattern::PytestFixtures);
                }
            }

            // Check for unittest setUp/tearDown.
            let has_setup_teardown = file
                .functions
                .iter()
                .any(|f| f.name == "setUp" || f.name == "tearDown");
            if has_setup_teardown {
                patterns.push(SetupPattern::SetUpTearDown);
            }
        }
    }

    patterns
}

// ---------------------------------------------------------------------------
// Evidence helpers
// ---------------------------------------------------------------------------

/// Build evidence from function references.
fn function_evidence(functions: &[&Function], max: usize, file_path: &Path) -> Vec<CodeEvidence> {
    functions
        .iter()
        .take(max)
        .map(|f| CodeEvidence {
            file: file_path.to_path_buf(),
            line: f.line,
            end_line: f.end_line,
            snippet: String::new(),
            snippet_start_line: 0,
            anchor: AnchorKind::CallSite,
        })
        .collect()
}

/// Build evidence from import references.
fn import_evidence(imports: &[&Import], max: usize, file_path: &Path) -> Vec<CodeEvidence> {
    imports
        .iter()
        .take(max)
        .map(|imp| CodeEvidence {
            file: file_path.to_path_buf(),
            line: imp.line,
            end_line: imp.line,
            snippet: String::new(),
            snippet_start_line: 0,
            anchor: AnchorKind::CallSite,
        })
        .collect()
}

/// Build evidence from dependency references.
fn dep_evidence(deps: &[&DependencyUsage], max: usize, file_path: &Path) -> Vec<CodeEvidence> {
    deps.iter()
        .take(max)
        .map(|d| CodeEvidence {
            file: file_path.to_path_buf(),
            line: d.line,
            end_line: d.line,
            snippet: String::new(),
            snippet_start_line: 0,
            anchor: AnchorKind::CallSite,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Heuristic fallback detection
// ---------------------------------------------------------------------------

/// Detect unknown test framework heuristically.
///
/// When a file is in a test directory and has test-prefixed functions but
/// no identifiable framework, emit an Observation finding.
fn detect_unknown_framework_fallback(file: &ProjectFile) -> Option<ConventionFinding> {
    let path_str = file.path.to_string_lossy();
    let placement = detect_test_file_placement(&path_str, file.language);

    // Must be a test file (by placement or naming), but framework is unknown
    placement?;

    // Collect test-like functions across all languages.
    // For JS/TS, also check for test_ prefix (not just describe/it/test)
    // since some codebases use that pattern without a framework.
    let test_functions: Vec<&Function> = file
        .functions
        .iter()
        .filter(|f| {
            f.name.starts_with("test_")
                || f.name == "describe"
                || f.name == "it"
                || f.name == "test"
        })
        .collect();

    if test_functions.is_empty() {
        return None;
    }

    Some(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: KnowledgeNature::Observation,
        description: "Uses testing (framework unknown)".to_owned(),
        evidence: function_evidence(&test_functions, MAX_EVIDENCE, &file.path),
        follows_convention: true,
        kind: FindingKind::Testing,
    })
}

/// Detect testing-related dependencies via name heuristic.
///
/// Emits Observation findings for dependencies whose names contain testing
/// keywords (test, mock, assert, spec) but are not already classified as
/// known test frameworks.
fn detect_heuristic_test_deps(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    // Collect heuristic test deps from dependencies_used
    let heuristic_deps: Vec<&DependencyUsage> = file
        .dependencies_used
        .iter()
        .filter(|d| is_heuristic_test_dep(&d.package, file.language))
        .collect();

    // Also check imports for testing-related modules
    let heuristic_imports: Vec<&Import> = file
        .imports
        .iter()
        .filter(|i| is_heuristic_test_dep(&i.module, file.language))
        .collect();

    // Deduplicate: collect unique package names
    let mut seen_packages: HashSet<&str> = HashSet::new();

    for dep in &heuristic_deps {
        if seen_packages.insert(&dep.package) {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Observation,
                description: format!("Testing-related dependency (heuristic): {}", dep.package),
                evidence: vec![CodeEvidence {
                    file: file.path.clone(),
                    line: dep.line,
                    end_line: dep.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                }],
                follows_convention: true,
                kind: FindingKind::Heuristic,
            });
        }
    }

    for imp in &heuristic_imports {
        let root = imp.module.split('/').next().unwrap_or(&imp.module);
        let root = root.split("::").next().unwrap_or(root);
        let root = root.split('.').next().unwrap_or(root);
        if seen_packages.insert(root) {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Observation,
                description: format!("Testing-related import (heuristic): {}", imp.module),
                evidence: vec![CodeEvidence {
                    file: file.path.clone(),
                    line: imp.line,
                    end_line: imp.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                }],
                follows_convention: true,
                kind: FindingKind::Heuristic,
            });
        }
    }

    findings
}

// ---------------------------------------------------------------------------
// Per-language detection
// ---------------------------------------------------------------------------

/// Detect test patterns in a Rust file.
fn detect_rust(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();
    let path_str = file.path.to_string_lossy();

    // --- Framework detection ---
    // Rust built-in `#[test]` is the standard; check for test functions.
    let test_functions: Vec<&Function> = file
        .functions
        .iter()
        .filter(|f| f.name.starts_with("test_"))
        .collect();

    let has_inline_tests = has_rust_inline_tests(file);
    let placement = detect_test_file_placement(&path_str, Language::Rust);

    let is_test_file = !test_functions.is_empty() || has_inline_tests || placement.is_some();

    if !is_test_file {
        return findings;
    }

    // Framework finding.
    // For Rust built-in #[test], look for assert!/assert_eq!/assert_ne! macro
    // call sites as evidence. These are built-in macros not tied to any import,
    // so scoped usage-evidence doesn't work. Instead, scan macro_calls directly.
    let assert_evidence: Vec<CodeEvidence> = if let LanguageIR::Rust(ref ir) = file.language_ir {
        ir.macro_calls
            .iter()
            .filter(|mc| {
                mc.name == "assert"
                    || mc.name == "assert_eq"
                    || mc.name == "assert_ne"
                    || mc.name == "assert_matches"
            })
            .take(MAX_EVIDENCE)
            .map(|mc| CodeEvidence {
                file: file.path.clone(),
                line: mc.line,
                end_line: mc.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect()
    } else {
        Vec::new()
    };

    let evidence = if !assert_evidence.is_empty() {
        assert_evidence
    } else if !test_functions.is_empty() {
        function_evidence(&test_functions, MAX_EVIDENCE, &file.path)
    } else if let LanguageIR::Rust(ref ir) = file.language_ir {
        ir.mod_declarations
            .iter()
            .filter(|m| m.name == "tests" || m.name == "test")
            .map(|m| CodeEvidence {
                file: file.path.clone(),
                line: m.line,
                end_line: m.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect()
    } else {
        vec![]
    };
    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: KnowledgeNature::Convention,
        description: format!(
            "Tests written with {} framework",
            TestFramework::RustBuiltin.as_str()
        ),
        evidence,
        follows_convention: true,
        kind: FindingKind::Testing,
    });

    // --- Test file placement ---
    if let Some(place) = placement {
        let desc = match place {
            TestPlacement::Separate => "Test file placement: separate tests/ directory".to_owned(),
            TestPlacement::CoLocated => "Test file placement: co-located test file".to_owned(),
        };
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: desc,
            evidence: vec![CodeEvidence {
                file: file.path.clone(),
                line: 0, // file-level signal, no single source line
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    } else if has_inline_tests {
        // Build evidence from the test module declaration line so detect_with_source
        // can show the opening of the test module (10 lines of context).
        // Accept both `mod tests` (plural) and `mod test` (singular).
        let inline_evidence: Vec<CodeEvidence> = if let LanguageIR::Rust(ref ir) = file.language_ir
        {
            ir.mod_declarations
                .iter()
                .filter(|m| m.name == "tests" || m.name == "test")
                .map(|m| CodeEvidence {
                    file: file.path.clone(),
                    line: m.line,
                    end_line: m.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect()
        } else {
            vec![CodeEvidence {
                file: file.path.clone(),
                line: 0,
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }]
        };
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Test file placement: inline #[cfg(test)] mod tests".to_owned(),
            evidence: inline_evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Test naming convention ---
    let naming_styles = detect_test_naming(&file.functions, Language::Rust);
    for (style, _count) in &naming_styles {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Test naming convention: {} (Rust)", style.as_str(),),
            evidence: function_evidence(&test_functions, MAX_EVIDENCE, &file.path),
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Setup/teardown patterns ---
    let setup_patterns = detect_setup_patterns(file);
    for pattern in &setup_patterns {
        let evidence = file
            .functions
            .iter()
            .filter(|f| f.name.starts_with("setup") || f.name.starts_with("make_"))
            .take(MAX_EVIDENCE)
            .map(|f| CodeEvidence {
                file: file.path.clone(),
                line: f.line,
                end_line: f.end_line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Test setup pattern: {}", pattern.as_str()),
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Heuristic: testing-related dependencies ---
    findings.extend(detect_heuristic_test_deps(file));

    findings
}

/// Detect test patterns in a TypeScript file.
fn detect_typescript(file: &ProjectFile) -> Vec<ConventionFinding> {
    detect_js_ts(file)
}

/// Detect test patterns in a JavaScript file.
fn detect_javascript(file: &ProjectFile) -> Vec<ConventionFinding> {
    detect_js_ts(file)
}

/// Shared JS/TS test pattern detection.
fn detect_js_ts(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();
    let path_str = file.path.to_string_lossy();

    // --- Framework detection via imports and dependencies ---
    let framework = detect_js_ts_framework(file);
    let placement = detect_test_file_placement(&path_str, file.language);

    // --- Heuristic: config file detection ---
    let config_framework = detect_config_file_framework(&path_str);

    // Check if this is a test file at all.
    let has_test_functions = file
        .functions
        .iter()
        .any(|f| f.name == "describe" || f.name == "it" || f.name == "test");
    let is_test_file = framework.is_some()
        || placement.is_some()
        || has_test_functions
        || config_framework.is_some();

    if !is_test_file {
        // Even non-test files might have testing-related dependencies
        let dep_findings = detect_heuristic_test_deps(file);
        if !dep_findings.is_empty() {
            return dep_findings;
        }
        return findings;
    }

    // Framework finding.
    if let Some(fw) = framework {
        // Prefer call-site evidence (actual expect(...).toBe(...) calls) over
        // import-line evidence. Fall back to import/dep evidence if no call
        // sites were found.
        let test_modules: Vec<&str> = file
            .imports
            .iter()
            .filter(|i| classify_js_ts_test_framework(&i.module).is_some())
            .map(|i| i.module.as_str())
            .collect();
        let call_sites = find_usage_evidence_for_file_scoped(file, &test_modules, MAX_EVIDENCE);
        let evidence = if !call_sites.is_empty() {
            call_sites
        } else {
            // Evidence from imports.
            let fw_imports: Vec<&Import> = file
                .imports
                .iter()
                .filter(|i| classify_js_ts_test_framework(&i.module).is_some())
                .collect();
            let mut ev = import_evidence(&fw_imports, MAX_EVIDENCE, &file.path);

            // Evidence from deps if imports didn't provide enough.
            if ev.len() < MAX_EVIDENCE {
                let fw_deps: Vec<&DependencyUsage> = file
                    .dependencies_used
                    .iter()
                    .filter(|d| classify_js_ts_test_framework(&d.package).is_some())
                    .collect();
                ev.extend(dep_evidence(&fw_deps, MAX_EVIDENCE - ev.len(), &file.path));
            }
            ev
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Tests written with {} framework", fw.as_str()),
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    } else if let Some(cfg_fw) = config_framework {
        // Heuristic: framework inferred from config file name
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!("Testing framework (from config file): {}", cfg_fw.as_str()),
            evidence: vec![CodeEvidence {
                file: file.path.clone(),
                line: 0, // file-level signal, no single source line
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    } else if placement.is_some() {
        // Heuristic: unknown framework fallback — test file with test-like functions
        // but no identifiable framework
        if let Some(fallback) = detect_unknown_framework_fallback(file) {
            findings.push(fallback);
        }
    }

    // --- Test file placement ---
    if let Some(place) = placement {
        let desc = match place {
            TestPlacement::Separate => "Test file placement: separate tests/ directory".to_owned(),
            TestPlacement::CoLocated => "Test file placement: co-located test file".to_owned(),
        };
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: desc,
            evidence: vec![CodeEvidence {
                file: file.path.clone(),
                line: 0, // file-level signal, no single source line
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Test naming convention ---
    let naming_styles = detect_test_naming(&file.functions, file.language);
    for (style, _count) in &naming_styles {
        let relevant_fns: Vec<&Function> = match style {
            TestNamingStyle::DescribeIt => file
                .functions
                .iter()
                .filter(|f| f.name == "describe" || f.name == "it")
                .collect(),
            TestNamingStyle::TestFunction => {
                file.functions.iter().filter(|f| f.name == "test").collect()
            }
            _ => Vec::new(),
        };
        let evidence = function_evidence(&relevant_fns, MAX_EVIDENCE, &file.path);

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Test naming convention: {} ({})",
                style.as_str(),
                file.language,
            ),
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Setup/teardown patterns ---
    let setup_patterns = detect_setup_patterns(file);
    for pattern in &setup_patterns {
        let relevant_fns: Vec<CodeEvidence> = match pattern {
            SetupPattern::Hooks => file
                .functions
                .iter()
                .filter(|f| {
                    matches!(
                        f.name.as_str(),
                        "beforeEach" | "afterEach" | "beforeAll" | "afterAll"
                    )
                })
                .take(MAX_EVIDENCE)
                .map(|f| CodeEvidence {
                    file: file.path.clone(),
                    line: f.line,
                    end_line: f.end_line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect(),
            SetupPattern::TestBuilder => file
                .functions
                .iter()
                .filter(|f| f.name.starts_with("create") || f.name.starts_with("make"))
                .take(MAX_EVIDENCE)
                .map(|f| CodeEvidence {
                    file: file.path.clone(),
                    line: f.line,
                    end_line: f.end_line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect(),
            _ => Vec::new(),
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Test setup pattern: {}", pattern.as_str()),
            evidence: relevant_fns,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Heuristic: testing-related dependencies ---
    findings.extend(detect_heuristic_test_deps(file));

    findings
}

/// Detect the JS/TS test framework from imports and dependencies.
fn detect_js_ts_framework(file: &ProjectFile) -> Option<TestFramework> {
    // Check dependencies first.
    for dep in &file.dependencies_used {
        if let Some(fw) = classify_js_ts_test_framework(&dep.package) {
            return Some(fw);
        }
    }

    // Check imports.
    for imp in &file.imports {
        if let Some(fw) = classify_js_ts_test_framework(&imp.module) {
            return Some(fw);
        }
    }

    // Infer from function names if no explicit imports (Jest globals).
    let has_describe = file.functions.iter().any(|f| f.name == "describe");
    let has_it = file.functions.iter().any(|f| f.name == "it");
    let has_test = file.functions.iter().any(|f| f.name == "test");

    if has_describe || has_it || has_test {
        // Can't distinguish Jest from Vitest from globals alone — default to Jest
        // as the most common.
        return Some(TestFramework::Jest);
    }

    None
}

/// Detect test patterns in a Python file.
fn detect_python(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();
    let path_str = file.path.to_string_lossy();

    // --- Framework detection ---
    let framework = detect_python_framework(file);
    let placement = detect_test_file_placement(&path_str, Language::Python);

    // --- Heuristic: config file detection ---
    let config_framework = detect_config_file_framework(&path_str);

    // Check for test functions/classes.
    let test_functions: Vec<&Function> = file
        .functions
        .iter()
        .filter(|f| f.name.starts_with("test_"))
        .collect();
    let test_classes: Vec<&seshat_core::TypeDef> = file
        .types
        .iter()
        .filter(|t| t.name.starts_with("Test"))
        .collect();

    let is_test_file = framework.is_some()
        || placement.is_some()
        || !test_functions.is_empty()
        || !test_classes.is_empty()
        || config_framework.is_some();

    if !is_test_file {
        // Even non-test files might have testing-related dependencies
        let dep_findings = detect_heuristic_test_deps(file);
        if !dep_findings.is_empty() {
            return dep_findings;
        }
        return findings;
    }

    // Framework finding.
    if let Some(fw) = framework {
        // Prefer call-site evidence over import-line evidence.
        let test_modules: Vec<&str> = file
            .imports
            .iter()
            .filter(|i| classify_python_test_framework(&i.module).is_some())
            .map(|i| i.module.as_str())
            .collect();
        let call_sites = find_usage_evidence_for_file_scoped(file, &test_modules, MAX_EVIDENCE);
        let evidence = if !call_sites.is_empty() {
            call_sites
        } else {
            // Evidence from imports.
            let fw_imports: Vec<&Import> = file
                .imports
                .iter()
                .filter(|i| classify_python_test_framework(&i.module).is_some())
                .collect();
            let mut ev = import_evidence(&fw_imports, MAX_EVIDENCE, &file.path);

            // Evidence from deps.
            if ev.len() < MAX_EVIDENCE {
                let fw_deps: Vec<&DependencyUsage> = file
                    .dependencies_used
                    .iter()
                    .filter(|d| classify_python_test_framework(&d.package).is_some())
                    .collect();
                ev.extend(dep_evidence(&fw_deps, MAX_EVIDENCE - ev.len(), &file.path));
            }
            ev
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Tests written with {} framework", fw.as_str()),
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    } else if let Some(cfg_fw) = config_framework {
        // Heuristic: framework inferred from config file name (conftest.py → pytest)
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!("Testing framework (from config file): {}", cfg_fw.as_str()),
            evidence: vec![CodeEvidence {
                file: file.path.clone(),
                line: 0, // file-level signal, no single source line
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    } else if (placement.is_some() || !test_classes.is_empty())
        && !test_functions.is_empty()
        && framework.is_none()
    {
        // Heuristic: unknown framework fallback
        if let Some(fallback) = detect_unknown_framework_fallback(file) {
            findings.push(fallback);
        }
    }

    // --- Test file placement ---
    if let Some(place) = placement {
        let desc = match place {
            TestPlacement::Separate => "Test file placement: separate tests/ directory".to_owned(),
            TestPlacement::CoLocated => "Test file placement: co-located test file".to_owned(),
        };
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: desc,
            evidence: vec![CodeEvidence {
                file: file.path.clone(),
                line: 0, // file-level signal, no single source line
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Test naming convention ---
    let naming_styles = detect_test_naming(&file.functions, Language::Python);
    for (style, _count) in &naming_styles {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Test naming convention: {} (Python)", style.as_str(),),
            evidence: function_evidence(&test_functions, MAX_EVIDENCE, &file.path),
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // Test class naming.
    if !test_classes.is_empty() {
        let evidence: Vec<CodeEvidence> = test_classes
            .iter()
            .take(MAX_EVIDENCE)
            .map(|t| CodeEvidence {
                file: file.path.clone(),
                line: t.line,
                end_line: t.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Test naming convention: {} (Python)",
                TestNamingStyle::TestClass.as_str(),
            ),
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Setup/teardown patterns ---
    let setup_patterns = detect_setup_patterns(file);
    for pattern in &setup_patterns {
        let evidence: Vec<CodeEvidence> = match pattern {
            SetupPattern::PytestFixtures => {
                if let LanguageIR::Python(ref ir) = file.language_ir {
                    // pytest.fixture decorators don't carry line numbers in
                    // the IR — use line:0 so detect_with_source skips
                    // extraction rather than extracting wrong lines.
                    ir.decorators
                        .iter()
                        .filter(|d| d.starts_with("pytest.fixture"))
                        .take(MAX_EVIDENCE)
                        .map(|d| CodeEvidence {
                            file: file.path.clone(),
                            line: 0,
                            end_line: 0,
                            snippet: d.to_string(),
                            snippet_start_line: 0,
                            anchor: AnchorKind::FileLevel,
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
            SetupPattern::SetUpTearDown => file
                .functions
                .iter()
                .filter(|f| f.name == "setUp" || f.name == "tearDown")
                .take(MAX_EVIDENCE)
                .map(|f| CodeEvidence {
                    file: file.path.clone(),
                    line: f.line,
                    end_line: f.end_line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect(),
            _ => Vec::new(),
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Test setup pattern: {}", pattern.as_str()),
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }

    // --- Pytest-specific patterns ---
    detect_pytest_patterns(file, &mut findings);

    // --- Heuristic: testing-related dependencies ---
    findings.extend(detect_heuristic_test_deps(file));

    findings
}

/// Detect pytest-specific patterns: parametrize, markers, fixtures.
fn detect_pytest_patterns(file: &ProjectFile, findings: &mut Vec<ConventionFinding>) {
    let ir = match &file.language_ir {
        LanguageIR::Python(ir) => ir,
        _ => return,
    };

    detect_pytest_parametrize(file, ir, findings);
    detect_pytest_markers(file, ir, findings);
}

/// Detect `@pytest.mark.parametrize` usage.
fn detect_pytest_parametrize(
    file: &ProjectFile,
    ir: &PythonIR,
    findings: &mut Vec<ConventionFinding>,
) {
    let parametrize_decorators: Vec<&String> = ir
        .decorators
        .iter()
        .filter(|d| d.starts_with("pytest.mark.parametrize"))
        .collect();

    if !parametrize_decorators.is_empty() {
        // parametrize decorators don't carry line numbers in the IR — use
        // line:0 so detect_with_source skips extraction rather than
        // extracting wrong lines.
        let evidence: Vec<CodeEvidence> = parametrize_decorators
            .iter()
            .take(MAX_EVIDENCE)
            .map(|d| CodeEvidence {
                file: file.path.clone(),
                line: 0,
                end_line: 0,
                snippet: d.to_string(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Pytest parametrize pattern".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }
}

/// Detect pytest markers (e.g., `@pytest.mark.slow`).
fn detect_pytest_markers(file: &ProjectFile, ir: &PythonIR, findings: &mut Vec<ConventionFinding>) {
    let markers: Vec<&String> = ir
        .decorators
        .iter()
        .filter(|d| d.starts_with("pytest.mark.") && !d.starts_with("pytest.mark.parametrize"))
        .collect();

    if !markers.is_empty() {
        // pytest markers don't carry line numbers in the IR — use line:0
        // so detect_with_source skips extraction rather than extracting
        // wrong lines.
        let evidence: Vec<CodeEvidence> = markers
            .iter()
            .take(MAX_EVIDENCE)
            .map(|d| CodeEvidence {
                file: file.path.clone(),
                line: 0,
                end_line: 0,
                snippet: d.to_string(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: {
                let mut marker_names: Vec<String> = markers
                    .iter()
                    .map(|m| m.strip_prefix("pytest.mark.").unwrap_or(m).to_string())
                    .collect();
                marker_names.sort();
                marker_names.dedup();
                format!("Pytest markers in use: {}", marker_names.join(", "))
            },
            evidence,
            follows_convention: true,
            kind: FindingKind::Testing,
        });
    }
}

/// Detect Python test framework from imports and dependencies.
fn detect_python_framework(file: &ProjectFile) -> Option<TestFramework> {
    // Check dependencies first.
    for dep in &file.dependencies_used {
        if let Some(fw) = classify_python_test_framework(&dep.package) {
            return Some(fw);
        }
    }

    // Check imports — pytest and unittest may be imported directly.
    for imp in &file.imports {
        if let Some(fw) = classify_python_test_framework(&imp.module) {
            return Some(fw);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects testing patterns across all four supported languages.
///
/// Produces:
/// - **Convention** findings for testing framework, file placement, naming patterns, and
///   setup/teardown approaches.
/// - **Observation** findings for pytest markers and other notable patterns.
pub struct TestPatternsDetector;

impl ConventionDetector for TestPatternsDetector {
    fn name(&self) -> &'static str {
        DETECTOR_NAME
    }

    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
        match file.language {
            Language::Rust => detect_rust(file),
            Language::TypeScript => detect_typescript(file),
            Language::JavaScript => detect_javascript(file),
            Language::Python => detect_python(file),
        }
    }

    fn supported_languages(&self) -> &[Language] {
        Language::all()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{
        DependencyUsage, Function, FunctionCall, Import, JavaScriptIR, MacroCall, ModDeclaration,
        PythonIR, RustIR, TypeDef, TypeDefKind, TypeScriptIR,
    };
    use std::path::PathBuf;

    // -- Helpers --

    fn make_rust_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    fn make_ts_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        }
    }

    fn make_js_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::JavaScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR::default()),
            file_doc: None,
        }
    }

    fn make_python_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Python,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
            file_doc: None,
        }
    }

    fn make_function(name: &str, line: usize) -> Function {
        Function {
            name: name.to_owned(),
            is_public: false,
            is_async: false,
            line,
            end_line: line + 5,
            parameters: vec![],
            doc_comment: None,
        }
    }

    fn make_import(module: &str, names: &[&str], line: usize) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| (*s).to_owned()).collect(),
            is_type_only: false,
            line,
        }
    }

    fn make_dep(package: &str, import_path: &str, line: usize) -> DependencyUsage {
        DependencyUsage {
            package: package.to_owned(),
            import_path: import_path.to_owned(),
            line,
        }
    }

    fn make_rust_file_with_ir(path: &str, ir: RustIR) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(ir),
            file_doc: None,
        }
    }

    fn make_python_file_with_ir(path: &str, ir: PythonIR) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Python,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(ir),
            file_doc: None,
        }
    }

    // -- Trait basics --

    #[test]
    fn detector_name() {
        let detector = TestPatternsDetector;
        assert_eq!(detector.name(), "test_patterns");
    }

    #[test]
    fn supports_all_languages() {
        let detector = TestPatternsDetector;
        assert_eq!(detector.supported_languages().len(), 4);
    }

    #[test]
    fn empty_file_no_findings() {
        let detector = TestPatternsDetector;
        for file in &[
            make_rust_file("src/lib.rs"),
            make_ts_file("src/utils.ts"),
            make_js_file("src/helpers.js"),
            make_python_file("app/models.py"),
        ] {
            let findings = detector.detect(file);
            assert!(
                findings.is_empty(),
                "file {:?} should have no test findings",
                file.path
            );
        }
    }

    // -- Rust --

    #[test]
    fn rust_builtin_test_framework_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("src/parser.rs");
        file.functions = vec![
            make_function("parse_input", 1),
            make_function("test_parse_empty", 10),
            make_function("test_parse_valid", 20),
        ];

        let findings = detector.detect(&file);
        assert!(!findings.is_empty());

        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect framework");
        assert!(fw.description.contains("built-in #[test]"));
        assert_eq!(fw.nature, KnowledgeNature::Convention);
    }

    #[test]
    fn rust_inline_test_module_detected() {
        let detector = TestPatternsDetector;
        let ir = RustIR {
            mod_declarations: vec![ModDeclaration {
                name: "tests".to_owned(),
                line: 10,
            }],
            ..RustIR::default()
        };
        let mut file = make_rust_file_with_ir("src/lib.rs", ir);
        file.functions = vec![make_function("test_something", 50)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("inline"));
    }

    #[test]
    fn rust_separate_tests_dir() {
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("tests/integration_test.rs");
        file.functions = vec![make_function("test_integration", 1)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("separate"));
    }

    #[test]
    fn rust_test_naming_convention() {
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("src/parser.rs");
        file.functions = vec![
            make_function("test_parse_empty", 10),
            make_function("test_parse_valid", 20),
            make_function("test_parse_error", 30),
        ];

        let findings = detector.detect(&file);
        let naming = findings
            .iter()
            .find(|f| f.description.contains("Test naming convention"))
            .expect("should detect naming");
        assert!(naming.description.contains("test_* prefix"));
        assert!(naming.description.contains("(Rust)"));
    }

    #[test]
    fn rust_setup_builder_pattern() {
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("tests/helper.rs");
        file.functions = vec![
            make_function("test_something", 1),
            make_function("make_test_context", 20),
        ];

        let findings = detector.detect(&file);
        let setup = findings
            .iter()
            .find(|f| f.description.contains("Test setup pattern"));
        assert!(setup.is_some(), "should detect builder pattern");
        assert!(
            setup
                .unwrap()
                .description
                .contains("test builder/factory pattern")
        );
    }

    // -- TypeScript --

    #[test]
    fn ts_jest_framework_from_import() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.imports = vec![make_import(
            "@jest/globals",
            &["describe", "it", "expect"],
            1,
        )];
        file.functions = vec![make_function("describe", 5), make_function("it", 10)];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect Jest");
        assert!(fw.description.contains("Jest"));
    }

    #[test]
    fn ts_jest_framework_from_dep() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.dependencies_used = vec![make_dep("jest", "jest", 1)];
        file.functions = vec![make_function("describe", 5)];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect Jest");
        assert!(fw.description.contains("Jest"));
    }

    #[test]
    fn ts_vitest_framework_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.imports = vec![make_import("vitest", &["describe", "it", "expect"], 1)];
        file.functions = vec![make_function("describe", 5)];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect Vitest");
        assert!(fw.description.contains("Vitest"));
    }

    #[test]
    fn ts_mocha_framework_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("test/utils.test.ts");
        file.imports = vec![make_import("mocha", &["describe", "it"], 1)];
        file.dependencies_used = vec![make_dep("chai", "chai", 2)];
        file.functions = vec![make_function("describe", 5)];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect Mocha");
        assert!(fw.description.contains("Mocha"));
    }

    #[test]
    fn ts_colocated_test_file() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/components/Button.test.ts");
        file.functions = vec![make_function("describe", 1)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("co-located"));
    }

    #[test]
    fn ts_separate_tests_dir() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("__tests__/utils.test.ts");
        file.functions = vec![make_function("describe", 1)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("separate"));
    }

    #[test]
    fn ts_spec_file_colocated() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/services/api.spec.ts");
        file.functions = vec![make_function("describe", 1)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("co-located"));
    }

    #[test]
    fn ts_describe_it_naming() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.functions = vec![
            make_function("describe", 1),
            make_function("it", 5),
            make_function("it", 10),
            make_function("it", 15),
        ];

        let findings = detector.detect(&file);
        let naming = findings
            .iter()
            .find(|f| f.description.contains("Test naming convention"))
            .expect("should detect naming");
        assert!(naming.description.contains("describe/it blocks"));
        assert!(naming.description.contains("(TypeScript)"));
    }

    #[test]
    fn ts_hooks_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.functions = vec![
            make_function("describe", 1),
            make_function("beforeEach", 3),
            make_function("it", 10),
        ];

        let findings = detector.detect(&file);
        let setup = findings
            .iter()
            .find(|f| f.description.contains("Test setup pattern"))
            .expect("should detect hooks");
        assert!(setup.description.contains("beforeEach/afterEach hooks"));
    }

    #[test]
    fn ts_jest_inferred_from_globals() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        // No imports — Jest globals available without import.
        file.functions = vec![make_function("describe", 1), make_function("it", 5)];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should infer Jest from globals");
        assert!(fw.description.contains("Jest"));
    }

    // -- JavaScript --

    #[test]
    fn js_jest_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_js_file("src/utils.test.js");
        file.dependencies_used = vec![make_dep("jest", "jest", 1)];
        file.functions = vec![make_function("describe", 1), make_function("it", 5)];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect Jest in JS");
        assert!(fw.description.contains("Jest"));
    }

    #[test]
    fn js_separate_test_dir() {
        let detector = TestPatternsDetector;
        let mut file = make_js_file("test/helpers.test.js");
        file.functions = vec![make_function("describe", 1)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("separate"));
    }

    // -- Python --

    #[test]
    fn python_pytest_framework_from_import() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("tests/test_parser.py");
        file.imports = vec![make_import("pytest", &[], 1)];
        file.functions = vec![
            make_function("test_parse_empty", 5),
            make_function("test_parse_valid", 10),
        ];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect pytest");
        assert!(fw.description.contains("pytest"));
    }

    #[test]
    fn python_unittest_framework_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("tests/test_models.py");
        file.imports = vec![make_import("unittest", &[], 1)];
        file.functions = vec![
            make_function("test_create_model", 10),
            make_function("setUp", 5),
        ];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect unittest");
        assert!(fw.description.contains("unittest"));
    }

    #[test]
    fn python_separate_tests_dir() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("tests/test_parser.py");
        file.functions = vec![make_function("test_something", 1)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("separate"));
    }

    #[test]
    fn python_colocated_test_file() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("src/parser/test_parser.py");
        file.functions = vec![make_function("test_parse", 1)];

        let findings = detector.detect(&file);
        let placement = findings
            .iter()
            .find(|f| f.description.contains("Test file placement"))
            .expect("should detect placement");
        assert!(placement.description.contains("co-located"));
    }

    #[test]
    fn python_test_prefix_naming() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("tests/test_parser.py");
        file.functions = vec![
            make_function("test_parse_empty", 5),
            make_function("test_parse_valid", 10),
            make_function("test_parse_error", 15),
            make_function("helper_function", 20),
        ];

        let findings = detector.detect(&file);
        let naming = findings
            .iter()
            .find(|f| f.description.contains("test_* prefix"))
            .expect("should detect naming");
        assert!(naming.description.contains("(Python)"));
    }

    #[test]
    fn python_test_class_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("tests/test_calc.py");
        file.functions = vec![make_function("test_add", 10)];
        file.types = vec![
            TypeDef {
                name: "TestCalculator".to_owned(),
                kind: TypeDefKind::Class,
                is_public: true,
                line: 5,
                doc_comment: None,
            },
            TypeDef {
                name: "TestParser".to_owned(),
                kind: TypeDefKind::Class,
                is_public: true,
                line: 30,
                doc_comment: None,
            },
        ];

        let findings = detector.detect(&file);
        let class_naming = findings
            .iter()
            .find(|f| f.description.contains("TestClass"))
            .expect("should detect test class naming");
        assert!(class_naming.description.contains("(Python)"));
    }

    #[test]
    fn python_pytest_fixtures_detected() {
        let detector = TestPatternsDetector;
        let ir = PythonIR {
            decorators: vec!["pytest.fixture".to_owned(), "pytest.fixture".to_owned()],
            ..PythonIR::default()
        };
        let mut file = make_python_file_with_ir("tests/conftest.py", ir);
        file.functions = vec![
            make_function("test_something", 10),
            make_function("calculator", 1),
        ];

        let findings = detector.detect(&file);
        let setup = findings
            .iter()
            .find(|f| f.description.contains("Test setup pattern"))
            .expect("should detect pytest fixtures");
        assert!(setup.description.contains("@pytest.fixture"));
    }

    #[test]
    fn python_unittest_setup_teardown() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("tests/test_models.py");
        file.imports = vec![make_import("unittest", &[], 1)];
        file.functions = vec![
            make_function("setUp", 5),
            make_function("tearDown", 15),
            make_function("test_create", 10),
        ];

        let findings = detector.detect(&file);
        let setup = findings
            .iter()
            .find(|f| f.description.contains("setUp/tearDown"));
        assert!(setup.is_some(), "should detect setUp/tearDown methods");
    }

    #[test]
    fn python_pytest_parametrize_detected() {
        let detector = TestPatternsDetector;
        let ir = PythonIR {
            decorators: vec![
                "pytest.mark.parametrize".to_owned(),
                "pytest.mark.parametrize".to_owned(),
            ],
            ..PythonIR::default()
        };
        let mut file = make_python_file_with_ir("tests/test_calc.py", ir);
        file.imports = vec![make_import("pytest", &[], 1)];
        file.functions = vec![make_function("test_add_parametrized", 10)];

        let findings = detector.detect(&file);
        let parametrize = findings
            .iter()
            .find(|f| f.description.contains("parametrize"))
            .expect("should detect parametrize");
        assert_eq!(parametrize.description, "Pytest parametrize pattern");
    }

    #[test]
    fn python_pytest_markers_detected() {
        let detector = TestPatternsDetector;
        let ir = PythonIR {
            decorators: vec![
                "pytest.mark.slow".to_owned(),
                "pytest.mark.integration".to_owned(),
            ],
            ..PythonIR::default()
        };
        let mut file = make_python_file_with_ir("tests/test_slow.py", ir);
        file.functions = vec![make_function("test_slow_operation", 5)];

        let findings = detector.detect(&file);
        let markers = findings
            .iter()
            .find(|f| f.description.contains("Pytest markers"))
            .expect("should detect markers");
        assert!(markers.description.contains("slow"));
        assert!(markers.description.contains("integration"));
        assert_eq!(markers.nature, KnowledgeNature::Observation);
    }

    // -- Cross-language --

    #[test]
    fn non_test_file_produces_no_findings() {
        let detector = TestPatternsDetector;

        // Source file with no test indicators.
        let mut file = make_ts_file("src/utils.ts");
        file.functions = vec![
            make_function("formatDate", 1),
            make_function("parseQuery", 10),
        ];

        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    #[test]
    fn evidence_capped_at_max() {
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("tests/big_test.rs");
        file.functions = (0..20)
            .map(|i| make_function(&format!("test_case_{i}"), i * 10))
            .collect();

        let findings = detector.detect(&file);
        for finding in &findings {
            assert!(
                finding.evidence.len() <= MAX_EVIDENCE,
                "evidence should be capped at {MAX_EVIDENCE}, got {} for '{}'",
                finding.evidence.len(),
                finding.description
            );
        }
    }

    // -- Framework classification --

    #[test]
    fn classify_js_ts_test_frameworks() {
        assert_eq!(
            classify_js_ts_test_framework("jest"),
            Some(TestFramework::Jest)
        );
        assert_eq!(
            classify_js_ts_test_framework("@jest/globals"),
            Some(TestFramework::Jest)
        );
        assert_eq!(
            classify_js_ts_test_framework("vitest"),
            Some(TestFramework::Vitest)
        );
        assert_eq!(
            classify_js_ts_test_framework("mocha"),
            Some(TestFramework::Mocha)
        );
        assert_eq!(
            classify_js_ts_test_framework("chai"),
            Some(TestFramework::Mocha)
        );
        assert_eq!(classify_js_ts_test_framework("express"), None);
    }

    #[test]
    fn classify_python_test_frameworks() {
        assert_eq!(
            classify_python_test_framework("pytest"),
            Some(TestFramework::Pytest)
        );
        assert_eq!(
            classify_python_test_framework("pytest-cov"),
            Some(TestFramework::Pytest)
        );
        assert_eq!(
            classify_python_test_framework("unittest"),
            Some(TestFramework::Unittest)
        );
        assert_eq!(classify_python_test_framework("django"), None);
    }

    // -- Placement detection --

    #[test]
    fn test_file_placement_detection() {
        // Rust
        assert_eq!(
            detect_test_file_placement("tests/integration.rs", Language::Rust),
            Some(TestPlacement::Separate)
        );
        assert_eq!(
            detect_test_file_placement("src/parser_test.rs", Language::Rust),
            Some(TestPlacement::CoLocated)
        );
        assert_eq!(
            detect_test_file_placement("src/parser.rs", Language::Rust),
            None
        );

        // TypeScript
        assert_eq!(
            detect_test_file_placement("__tests__/utils.test.ts", Language::TypeScript),
            Some(TestPlacement::Separate)
        );
        assert_eq!(
            detect_test_file_placement("src/utils.test.ts", Language::TypeScript),
            Some(TestPlacement::CoLocated)
        );
        assert_eq!(
            detect_test_file_placement("src/utils.spec.ts", Language::TypeScript),
            Some(TestPlacement::CoLocated)
        );

        // Python
        assert_eq!(
            detect_test_file_placement("tests/test_parser.py", Language::Python),
            Some(TestPlacement::Separate)
        );
        assert_eq!(
            detect_test_file_placement("src/parser/test_parser.py", Language::Python),
            Some(TestPlacement::CoLocated)
        );
    }

    // -- Heuristic fallbacks (US-011) --

    #[test]
    fn config_file_jest_detected() {
        assert_eq!(
            detect_config_file_framework("jest.config.ts"),
            Some(TestFramework::Jest)
        );
        assert_eq!(
            detect_config_file_framework("jest.config.js"),
            Some(TestFramework::Jest)
        );
        assert_eq!(
            detect_config_file_framework("src/jest.config.ts"),
            Some(TestFramework::Jest)
        );
    }

    #[test]
    fn config_file_vitest_detected() {
        assert_eq!(
            detect_config_file_framework("vitest.config.ts"),
            Some(TestFramework::Vitest)
        );
        assert_eq!(
            detect_config_file_framework("vitest.config.js"),
            Some(TestFramework::Vitest)
        );
    }

    #[test]
    fn config_file_conftest_pytest_detected() {
        assert_eq!(
            detect_config_file_framework("conftest.py"),
            Some(TestFramework::Pytest)
        );
        assert_eq!(
            detect_config_file_framework("tests/conftest.py"),
            Some(TestFramework::Pytest)
        );
    }

    #[test]
    fn config_file_non_test_file_returns_none() {
        assert_eq!(detect_config_file_framework("src/utils.ts"), None);
        assert_eq!(detect_config_file_framework("app.py"), None);
        assert_eq!(detect_config_file_framework("Cargo.toml"), None);
    }

    #[test]
    fn jest_config_file_emits_observation_finding() {
        let detector = TestPatternsDetector;
        let file = make_js_file("jest.config.js");

        let findings = detector.detect(&file);
        let config_finding = findings.iter().find(|f| {
            f.description
                .contains("Testing framework (from config file)")
        });
        assert!(
            config_finding.is_some(),
            "should detect Jest from config file"
        );
        let cf = config_finding.unwrap();
        assert!(cf.description.contains("Jest"));
        assert_eq!(cf.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn vitest_config_file_emits_observation_finding() {
        let detector = TestPatternsDetector;
        let file = make_ts_file("vitest.config.ts");

        let findings = detector.detect(&file);
        let config_finding = findings.iter().find(|f| {
            f.description
                .contains("Testing framework (from config file)")
        });
        assert!(
            config_finding.is_some(),
            "should detect Vitest from config file"
        );
        let cf = config_finding.unwrap();
        assert!(cf.description.contains("Vitest"));
        assert_eq!(cf.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn conftest_py_emits_observation_finding() {
        let detector = TestPatternsDetector;
        let file = make_python_file("tests/conftest.py");

        let findings = detector.detect(&file);
        let config_finding = findings.iter().find(|f| {
            f.description
                .contains("Testing framework (from config file)")
        });
        assert!(
            config_finding.is_some(),
            "should detect pytest from conftest.py"
        );
        let cf = config_finding.unwrap();
        assert!(cf.description.contains("pytest"));
        assert_eq!(cf.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn unknown_framework_fallback_in_test_dir() {
        let detector = TestPatternsDetector;
        // A JS file in tests/ dir with test functions but no framework imports
        let mut file = make_js_file("tests/helper.test.js");
        file.functions = vec![
            make_function("test_something", 5),
            make_function("test_other", 10),
        ];
        // No imports, no deps — no known framework will be detected.
        // BUT: detect_js_ts_framework will infer Jest from "test" function name.
        // So we need a scenario where describe/it/test are NOT present.

        // Actually, with test_something and test_other, those won't match
        // describe/it/test. Let me verify...
        // detect_js_ts_framework checks for functions named exactly "describe", "it", "test"
        // "test_something" != "test", so no framework inferred.

        let findings = detector.detect(&file);
        let fallback = findings
            .iter()
            .find(|f| f.description.contains("framework unknown"));
        assert!(
            fallback.is_some(),
            "should emit unknown framework fallback for test file without framework"
        );
        let fb = fallback.unwrap();
        assert_eq!(fb.nature, KnowledgeNature::Observation);
        assert_eq!(fb.description, "Uses testing (framework unknown)");
    }

    #[test]
    fn unknown_framework_fallback_python() {
        let detector = TestPatternsDetector;
        // Python file in tests/ dir with test functions but no framework import
        let mut file = make_python_file("tests/test_utils.py");
        file.functions = vec![
            make_function("test_parse", 5),
            make_function("test_validate", 10),
            make_function("test_format", 15),
        ];
        // No imports — no pytest or unittest framework detected.

        let findings = detector.detect(&file);
        let fallback = findings
            .iter()
            .find(|f| f.description.contains("framework unknown"));
        assert!(
            fallback.is_some(),
            "should emit unknown framework fallback for Python test file"
        );
        let fb = fallback.unwrap();
        assert_eq!(fb.nature, KnowledgeNature::Observation);
        assert_eq!(fb.description, "Uses testing (framework unknown)");
    }

    #[test]
    fn known_framework_takes_priority_over_heuristic() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.imports = vec![make_import("vitest", &["describe", "it"], 1)];
        file.functions = vec![make_function("describe", 5), make_function("it", 10)];

        let findings = detector.detect(&file);

        // Should have Convention finding for Vitest, NOT Observation from heuristic
        let fw = findings.iter().find(|f| {
            f.description.contains("Tests written with")
                || f.description.contains("Testing framework:")
        });
        assert!(fw.is_some());
        assert_eq!(fw.unwrap().nature, KnowledgeNature::Convention);

        // Should NOT have an Observation config/unknown fallback
        let fallback = findings.iter().find(|f| {
            f.description.contains("framework unknown")
                || f.description.contains("from config file")
        });
        assert!(
            fallback.is_none(),
            "known framework should suppress heuristic fallbacks"
        );
    }

    #[test]
    fn heuristic_test_dep_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.ts");
        file.dependencies_used = vec![make_dep("my-test-utils", "my-test-utils", 5)];
        file.imports = vec![make_import("my-test-utils", &["setup"], 1)];

        let findings = detector.detect(&file);
        let heuristic = findings.iter().find(|f| {
            f.description
                .contains("Testing-related dependency (heuristic)")
        });
        assert!(
            heuristic.is_some(),
            "should detect test-related dependency by name"
        );
        let h = heuristic.unwrap();
        assert_eq!(h.nature, KnowledgeNature::Observation);
        assert!(h.description.contains("my-test-utils"));
    }

    #[test]
    fn heuristic_mock_dep_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_python_file("tests/test_api.py");
        file.functions = vec![make_function("test_api_call", 10)];
        file.dependencies_used = vec![make_dep("mockserver", "mockserver", 1)];
        file.imports = vec![
            make_import("pytest", &[], 1),
            make_import("mockserver", &["MockServer"], 2),
        ];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| f.description.contains("Testing-related"));
        assert!(heuristic.is_some(), "should detect mock-related dependency");
        assert!(heuristic.unwrap().description.contains("mockserver"));
    }

    #[test]
    fn heuristic_assert_dep_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("tests/integration.rs");
        file.functions = vec![make_function("test_integration", 5)];
        file.dependencies_used = vec![make_dep("my-assert-lib", "my_assert_lib::assert_that", 3)];
        file.imports = vec![make_import("my_assert_lib", &["assert_that"], 1)];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| f.description.contains("Testing-related"));
        assert!(
            heuristic.is_some(),
            "should detect assert-related dependency"
        );
    }

    #[test]
    fn known_test_framework_not_flagged_as_heuristic() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.dependencies_used = vec![make_dep("jest", "jest", 1)];
        file.imports = vec![make_import("jest", &["describe", "it"], 1)];
        file.functions = vec![make_function("describe", 5)];

        let findings = detector.detect(&file);
        let heuristic = findings.iter().find(|f| {
            f.description
                .contains("Testing-related dependency (heuristic)")
        });
        assert!(
            heuristic.is_none(),
            "known framework 'jest' should NOT be flagged as heuristic"
        );
    }

    #[test]
    fn heuristic_spec_import_detected() {
        let detector = TestPatternsDetector;
        let mut file = make_js_file("tests/app.test.js");
        file.functions = vec![make_function("describe", 5), make_function("it", 10)];
        file.imports = vec![make_import("spec-reporter", &["reporter"], 1)];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| f.description.contains("Testing-related import (heuristic)"));
        assert!(
            heuristic.is_some(),
            "should detect spec-related import by name"
        );
    }

    #[test]
    fn no_heuristic_for_unrelated_dep() {
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.ts");
        file.dependencies_used = vec![make_dep("lodash", "lodash", 5)];
        file.imports = vec![make_import("lodash", &["debounce"], 1)];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| f.description.contains("Testing-related"));
        assert!(
            heuristic.is_none(),
            "unrelated dep 'lodash' should NOT be flagged as testing-related"
        );
    }

    #[test]
    fn detect_with_source_sets_real_snippet() {
        let detector = TestPatternsDetector;
        // TypeScript test file with a function named "testSomething" — using
        // the JS path where a function named "test_something" is in a test dir.
        let mut file = make_js_file("tests/helper.test.js");
        file.functions = vec![make_function("test_something", 1)];
        let source = "function test_something() {}\n";

        let findings = detector.detect_with_source(&file, source);

        assert!(!findings.is_empty(), "should have at least one finding");
        // Look for a finding that has evidence with a non-empty snippet.
        let finding_with_snippet = findings.iter().find(|f| {
            f.evidence
                .iter()
                .any(|ev| ev.line > 0 && !ev.snippet.is_empty())
        });
        assert!(
            finding_with_snippet.is_some(),
            "at least one finding should have evidence with a real snippet: {findings:#?}"
        );
        let ev = finding_with_snippet
            .unwrap()
            .evidence
            .iter()
            .find(|ev| ev.line > 0 && !ev.snippet.is_empty())
            .unwrap();
        assert_eq!(ev.file, file.path);
        // Snippet must contain the actual function name from source.
        assert!(
            ev.snippet.contains("test_something"),
            "snippet must contain real source keyword 'test_something', got: {:?}",
            ev.snippet
        );
        assert!(
            !ev.snippet.starts_with("fn "),
            "snippet must not be a synthetic 'fn <name>' format string, got: {:?}",
            ev.snippet
        );
    }

    #[test]
    fn is_heuristic_test_dep_helper() {
        // Positive cases
        assert!(is_heuristic_test_dep("my-test-utils", Language::TypeScript));
        assert!(is_heuristic_test_dep("mockserver", Language::Python));
        assert!(is_heuristic_test_dep("better-assert", Language::JavaScript));
        assert!(is_heuristic_test_dep("spec-reporter", Language::TypeScript));

        // Negative: known frameworks should NOT match
        assert!(!is_heuristic_test_dep("jest", Language::TypeScript));
        assert!(!is_heuristic_test_dep("pytest", Language::Python));
        assert!(!is_heuristic_test_dep("mockall", Language::Rust));

        // Negative: unrelated packages
        assert!(!is_heuristic_test_dep("express", Language::JavaScript));
        assert!(!is_heuristic_test_dep("lodash", Language::TypeScript));
    }

    // -- US-005: call-site evidence integration --

    #[test]
    fn ts_jest_imports_shows_expect_call_site() {
        // TypeScript file with Jest import and expect/describe call sites.
        // The framework finding evidence should show the expect(...) call site,
        // not the import line.
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/utils.test.ts");
        file.imports = vec![make_import(
            "@jest/globals",
            &["expect", "describe", "it"],
            1,
        )];
        file.functions = vec![make_function("describe", 5), make_function("it", 10)];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![
                FunctionCall {
                    callee: "describe".to_owned(),
                    line: 5,
                    end_line: 20,
                    snippet: "describe('sum', () => {".to_owned(),
                },
                FunctionCall {
                    callee: "expect".to_owned(),
                    line: 15,
                    end_line: 15,
                    snippet: "expect(sum(1, 2)).toBe(3)".to_owned(),
                },
            ];
        }

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect Jest");
        assert!(fw.description.contains("Jest"));

        // Evidence should include call-site lines, not import line (line 1)
        let call_site_ev = fw.evidence.iter().find(|e| e.line > 1);
        assert!(
            call_site_ev.is_some(),
            "framework finding should have call-site evidence (line > 1), got: {:?}",
            fw.evidence
        );
        let ev = call_site_ev.unwrap();
        assert!(
            !ev.snippet.is_empty(),
            "call-site evidence should have a snippet"
        );
    }

    #[test]
    fn rust_test_file_shows_assert_macro_call_site() {
        // Rust file with #[test] functions and assert! macro calls.
        // The framework finding evidence should show assert! call sites.
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("src/parser.rs");
        file.imports = vec![make_import("std::assert", &["assert"], 1)];
        file.functions = vec![
            make_function("parse_input", 1),
            make_function("test_parse_valid", 10),
        ];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![
                MacroCall {
                    name: "assert".to_owned(),
                    line: 15,
                },
                MacroCall {
                    name: "assert_eq".to_owned(),
                    line: 20,
                },
            ];
            // Add assert and assert_eq to imports so they match
        }
        // Use an import that the assert! macro will match against
        file.imports = vec![make_import("std", &["assert", "assert_eq"], 1)];

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should detect Rust built-in testing");
        assert!(fw.description.contains("built-in #[test]"));

        // Evidence should show assert macro call sites (lines 15 and 20)
        let has_assert_call_site = fw.evidence.iter().any(|e| e.line == 15 || e.line == 20);
        assert!(
            has_assert_call_site,
            "framework finding evidence should include assert macro call sites, got: {:?}",
            fw.evidence
        );
    }

    // -----------------------------------------------------------------------
    // BUG: unscoped call_sites contaminate test pattern findings
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Integration: test function evidence includes #[test] annotation
    // -----------------------------------------------------------------------

    #[test]
    fn test_patterns_detector_shows_test_annotation_in_snippet() {
        // Rust test file with a test function at line 5.
        // The `#[test]` annotation at line 4 is 1 line before the function.
        // detect_with_source fetches 2 lines of context before evidence.line,
        // so snippet_start_line = 3 and the snippet includes #[test] at line 4.
        let detector = TestPatternsDetector;
        let mut file = make_rust_file("src/lib.rs");
        file.functions = vec![make_function("test_something", 5)];

        // Source: lines 1-10, with #[test] at line 4 and the fn at line 5.
        let source = "\
use std::assert_eq;\n\
\n\
// helper comment\n\
#[test]\n\
fn test_something() {\n\
    assert_eq!(1 + 1, 2);\n\
}\n\
\n\
// another function\n\
fn helper() {}\n\
";

        let findings = detector.detect_with_source(&file, source);
        let framework_finding = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should have testing framework finding");

        let ev = framework_finding
            .evidence
            .iter()
            .find(|e| e.line == 5)
            .expect("should have function evidence at line 5");

        assert!(
            !ev.snippet.is_empty(),
            "test function snippet must be populated, got empty"
        );
        assert!(
            ev.snippet_start_line > 0 && ev.snippet_start_line < 5,
            "snippet_start_line should be before line 5, got: {}",
            ev.snippet_start_line
        );
        assert!(
            ev.snippet.contains("#[test]"),
            "snippet should include #[test] annotation from context lines, got: {:?}",
            ev.snippet
        );
        assert!(
            ev.snippet.contains("fn test_something"),
            "snippet should include the function definition, got: {:?}",
            ev.snippet
        );
    }

    #[test]
    fn unscoped_call_sites_contaminate_ts_test_finding() {
        // TypeScript file with jest (testing) AND winston (logging) imports.
        // The "Testing framework: Jest" finding should only have jest-related
        // evidence, not winston logging calls.
        let detector = TestPatternsDetector;
        let mut file = make_ts_file("src/app.test.ts");
        file.imports = vec![
            make_import("@jest/globals", &["expect", "describe", "it"], 1),
            make_import("winston", &["logger"], 2),
        ];
        file.functions = vec![make_function("describe", 5), make_function("it", 10)];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![
                FunctionCall {
                    callee: "expect".to_owned(),
                    line: 15,
                    end_line: 15,
                    snippet: "expect(sum(1, 2)).toBe(3)".to_owned(),
                },
                FunctionCall {
                    callee: "logger.info".to_owned(),
                    line: 30,
                    end_line: 30,
                    snippet: "logger.info('test setup')".to_owned(),
                },
            ];
        }

        let findings = detector.detect(&file);
        let fw = findings
            .iter()
            .find(|f| {
                f.description.contains("Tests written with")
                    || f.description.contains("Testing framework")
            })
            .expect("should have testing framework finding");

        // After fix: test finding should only have jest evidence (line 15),
        // winston logging (line 30) should NOT appear.
        let evidence_lines: Vec<usize> = fw.evidence.iter().map(|e| e.line).collect();
        assert!(
            !evidence_lines.contains(&30),
            "test finding should NOT contain winston call sites, got: {:?}",
            evidence_lines
        );
        assert!(
            evidence_lines.contains(&15),
            "test finding should contain jest call site (line 15), got: {:?}",
            evidence_lines
        );
    }
}
