//! Test patterns detector — framework, file placement, naming conventions.
//!
//! Identifies the testing framework in use, detects test file placement
//! conventions (co-located vs separate `tests/` directory), test naming
//! patterns, and setup/teardown approaches. Uses [`DependencyUsage`] and
//! [`Import`] entries to identify test framework imports, and function/type
//! names to identify test structure.
//!
//! Supported languages: Rust, TypeScript, JavaScript, Python.

use seshat_core::{
    CodeEvidence, ConventionFinding, DependencyUsage, Function, Import, KnowledgeNature, Language,
    LanguageIR, ProjectFile, PythonIR,
};

use crate::trait_def::ConventionDetector;

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
        // If the file has mod declarations including "tests", it has an inline test module.
        if ir.mod_declarations.iter().any(|m| m == "tests") {
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
fn function_evidence(functions: &[&Function], max: usize) -> Vec<CodeEvidence> {
    functions
        .iter()
        .take(max)
        .map(|f| CodeEvidence {
            line: f.line,
            end_line: f.end_line,
            snippet: format!("fn {}", f.name),
        })
        .collect()
}

/// Build evidence from import references.
fn import_evidence(imports: &[&Import], max: usize) -> Vec<CodeEvidence> {
    imports
        .iter()
        .take(max)
        .map(|imp| {
            let snippet = if imp.names.is_empty() {
                format!("import {}", imp.module)
            } else {
                format!("import {{{}}} from {}", imp.names.join(", "), imp.module)
            };
            CodeEvidence {
                line: imp.line,
                end_line: imp.line,
                snippet,
            }
        })
        .collect()
}

/// Build evidence from dependency references.
fn dep_evidence(deps: &[&DependencyUsage], max: usize) -> Vec<CodeEvidence> {
    deps.iter()
        .take(max)
        .map(|d| CodeEvidence {
            line: d.line,
            end_line: d.line,
            snippet: d.import_path.clone(),
        })
        .collect()
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
    let evidence = function_evidence(&test_functions, MAX_EVIDENCE);
    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: KnowledgeNature::Convention,
        description: format!("Testing framework: {}", TestFramework::RustBuiltin.as_str()),
        evidence,
        follows_convention: true,
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
                line: 1,
                end_line: 1,
                snippet: path_str.to_string(),
            }],
            follows_convention: true,
        });
    } else if has_inline_tests {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Test file placement: inline #[cfg(test)] mod tests".to_owned(),
            evidence: vec![CodeEvidence {
                line: 1,
                end_line: 1,
                snippet: path_str.to_string(),
            }],
            follows_convention: true,
        });
    }

    // --- Test naming convention ---
    let naming_styles = detect_test_naming(&file.functions, Language::Rust);
    for (style, count) in &naming_styles {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Test naming convention: {} ({} test functions)",
                style.as_str(),
                count
            ),
            evidence: function_evidence(&test_functions, MAX_EVIDENCE),
            follows_convention: true,
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
                line: f.line,
                end_line: f.end_line,
                snippet: format!("fn {}", f.name),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Test setup pattern: {}", pattern.as_str()),
            evidence,
            follows_convention: true,
        });
    }

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

    // Check if this is a test file at all.
    let has_test_functions = file
        .functions
        .iter()
        .any(|f| f.name == "describe" || f.name == "it" || f.name == "test");
    let is_test_file = framework.is_some() || placement.is_some() || has_test_functions;

    if !is_test_file {
        return findings;
    }

    // Framework finding.
    if let Some(fw) = framework {
        let mut evidence = Vec::new();

        // Evidence from imports.
        let fw_imports: Vec<&Import> = file
            .imports
            .iter()
            .filter(|i| classify_js_ts_test_framework(&i.module).is_some())
            .collect();
        evidence.extend(import_evidence(&fw_imports, MAX_EVIDENCE));

        // Evidence from deps if imports didn't provide enough.
        if evidence.len() < MAX_EVIDENCE {
            let fw_deps: Vec<&DependencyUsage> = file
                .dependencies_used
                .iter()
                .filter(|d| classify_js_ts_test_framework(&d.package).is_some())
                .collect();
            evidence.extend(dep_evidence(&fw_deps, MAX_EVIDENCE - evidence.len()));
        }

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Testing framework: {}", fw.as_str()),
            evidence,
            follows_convention: true,
        });
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
                line: 1,
                end_line: 1,
                snippet: path_str.to_string(),
            }],
            follows_convention: true,
        });
    }

    // --- Test naming convention ---
    let naming_styles = detect_test_naming(&file.functions, file.language);
    for (style, count) in &naming_styles {
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
        let evidence = function_evidence(&relevant_fns, MAX_EVIDENCE);

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Test naming convention: {} ({} occurrences)",
                style.as_str(),
                count
            ),
            evidence,
            follows_convention: true,
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
                    line: f.line,
                    end_line: f.end_line,
                    snippet: format!("fn {}", f.name),
                })
                .collect(),
            SetupPattern::TestBuilder => file
                .functions
                .iter()
                .filter(|f| f.name.starts_with("create") || f.name.starts_with("make"))
                .take(MAX_EVIDENCE)
                .map(|f| CodeEvidence {
                    line: f.line,
                    end_line: f.end_line,
                    snippet: format!("fn {}", f.name),
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
        });
    }

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
        || !test_classes.is_empty();

    if !is_test_file {
        return findings;
    }

    // Framework finding.
    if let Some(fw) = framework {
        let mut evidence = Vec::new();

        // Evidence from imports.
        let fw_imports: Vec<&Import> = file
            .imports
            .iter()
            .filter(|i| classify_python_test_framework(&i.module).is_some())
            .collect();
        evidence.extend(import_evidence(&fw_imports, MAX_EVIDENCE));

        // Evidence from deps.
        if evidence.len() < MAX_EVIDENCE {
            let fw_deps: Vec<&DependencyUsage> = file
                .dependencies_used
                .iter()
                .filter(|d| classify_python_test_framework(&d.package).is_some())
                .collect();
            evidence.extend(dep_evidence(&fw_deps, MAX_EVIDENCE - evidence.len()));
        }

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Testing framework: {}", fw.as_str()),
            evidence,
            follows_convention: true,
        });
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
                line: 1,
                end_line: 1,
                snippet: path_str.to_string(),
            }],
            follows_convention: true,
        });
    }

    // --- Test naming convention ---
    let naming_styles = detect_test_naming(&file.functions, Language::Python);
    for (style, count) in &naming_styles {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Test naming convention: {} ({} test functions)",
                style.as_str(),
                count
            ),
            evidence: function_evidence(&test_functions, MAX_EVIDENCE),
            follows_convention: true,
        });
    }

    // Test class naming.
    if !test_classes.is_empty() {
        let evidence: Vec<CodeEvidence> = test_classes
            .iter()
            .take(MAX_EVIDENCE)
            .map(|t| CodeEvidence {
                line: t.line,
                end_line: t.line,
                snippet: format!("class {}", t.name),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Test naming convention: {} ({} test classes)",
                TestNamingStyle::TestClass.as_str(),
                test_classes.len()
            ),
            evidence,
            follows_convention: true,
        });
    }

    // --- Setup/teardown patterns ---
    let setup_patterns = detect_setup_patterns(file);
    for pattern in &setup_patterns {
        let evidence: Vec<CodeEvidence> = match pattern {
            SetupPattern::PytestFixtures => {
                if let LanguageIR::Python(ref ir) = file.language_ir {
                    ir.decorators
                        .iter()
                        .filter(|d| d.starts_with("pytest.fixture"))
                        .take(MAX_EVIDENCE)
                        .enumerate()
                        .map(|(i, d)| CodeEvidence {
                            line: i + 1,
                            end_line: i + 1,
                            snippet: format!("@{d}"),
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
                    line: f.line,
                    end_line: f.end_line,
                    snippet: format!("def {}", f.name),
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
        });
    }

    // --- Pytest-specific patterns ---
    detect_pytest_patterns(file, &mut findings);

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
        let evidence: Vec<CodeEvidence> = parametrize_decorators
            .iter()
            .take(MAX_EVIDENCE)
            .enumerate()
            .map(|(i, d)| CodeEvidence {
                line: i + 1,
                end_line: i + 1,
                snippet: format!("@{d}"),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Pytest parametrize pattern ({} usages)",
                parametrize_decorators.len()
            ),
            evidence,
            follows_convention: true,
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
        let evidence: Vec<CodeEvidence> = markers
            .iter()
            .take(MAX_EVIDENCE)
            .enumerate()
            .map(|(i, d)| CodeEvidence {
                line: i + 1,
                end_line: i + 1,
                snippet: format!("@{d}"),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!(
                "Pytest markers in use: {}",
                markers
                    .iter()
                    .map(|m| { m.strip_prefix("pytest.mark.").unwrap_or(m).to_string() })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            evidence,
            follows_convention: true,
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
        DependencyUsage, Function, Import, JavaScriptIR, PythonIR, RustIR, TypeDef, TypeDefKind,
        TypeScriptIR,
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
            .find(|f| f.description.contains("Testing framework"))
            .expect("should detect framework");
        assert!(fw.description.contains("built-in #[test]"));
        assert_eq!(fw.nature, KnowledgeNature::Convention);
    }

    #[test]
    fn rust_inline_test_module_detected() {
        let detector = TestPatternsDetector;
        let ir = RustIR {
            mod_declarations: vec!["tests".to_owned()],
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
        assert!(naming.description.contains("3 test functions"));
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
            .find(|f| f.description.contains("Testing framework"))
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
            .find(|f| f.description.contains("Testing framework"))
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
            .find(|f| f.description.contains("Testing framework"))
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
            .find(|f| f.description.contains("Testing framework"))
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
        assert!(naming.description.contains("4 occurrences"));
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
            .find(|f| f.description.contains("Testing framework"))
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
            .find(|f| f.description.contains("Testing framework"))
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
            .find(|f| f.description.contains("Testing framework"))
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
            .find(|f| f.description.contains("Testing framework"))
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
        assert!(naming.description.contains("3 test functions"));
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
            },
            TypeDef {
                name: "TestParser".to_owned(),
                kind: TypeDefKind::Class,
                is_public: true,
                line: 30,
            },
        ];

        let findings = detector.detect(&file);
        let class_naming = findings
            .iter()
            .find(|f| f.description.contains("TestClass"))
            .expect("should detect test class naming");
        assert!(class_naming.description.contains("2 test classes"));
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
        assert!(parametrize.description.contains("2 usages"));
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
}
