//! Naming conventions detector — case patterns for files, functions, types,
//! parameters, and constants.
//!
//! Analyses [`ProjectFile::functions`], [`ProjectFile::types`], function
//! parameters, and file paths to determine the dominant naming convention per
//! category. Detected case patterns include `snake_case`, `camelCase`,
//! `PascalCase`, `SCREAMING_SNAKE_CASE`, and `kebab-case`.
//!
//! Language-aware weighting: Rust conventions are weighted lower (the compiler
//! already enforces snake_case/PascalCase), while JS/TS/Python conventions are
//! weighted higher because they are purely community-driven.

use std::collections::HashMap;
use std::path::Path;

use seshat_core::{
    CodeEvidence, ConventionFinding, Function, KnowledgeNature, Language, ProjectFile, TypeDef,
};

use crate::trait_def::ConventionDetector;

const DETECTOR_NAME: &str = "naming_conventions";

/// Naming conventions detector.
///
/// Detects case-style patterns for function names, type names, and file names
/// across all four supported languages.
pub struct NamingConventionsDetector;

impl ConventionDetector for NamingConventionsDetector {
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
// Case pattern classification
// ---------------------------------------------------------------------------

/// Recognised case patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CasePattern {
    /// `snake_case` — lowercase with underscores.
    SnakeCase,
    /// `camelCase` — starts lowercase, no underscores, has uppercase.
    CamelCase,
    /// `PascalCase` — starts uppercase, no underscores (beyond leading).
    PascalCase,
    /// `SCREAMING_SNAKE_CASE` — all uppercase with underscores.
    ScreamingSnakeCase,
    /// `kebab-case` — lowercase with hyphens (files only).
    KebabCase,
    /// Single lowercase word — could be snake, camel, or kebab.
    SingleLowerWord,
    /// Single uppercase word — could be screaming or pascal.
    SingleUpperWord,
    /// Could not classify (mixed patterns, numbers only, etc.).
    Unknown,
}

impl CasePattern {
    fn as_str(self) -> &'static str {
        match self {
            Self::SnakeCase => "snake_case",
            Self::CamelCase => "camelCase",
            Self::PascalCase => "PascalCase",
            Self::ScreamingSnakeCase => "SCREAMING_SNAKE_CASE",
            Self::KebabCase => "kebab-case",
            Self::SingleLowerWord => "single_lower_word",
            Self::SingleUpperWord => "single_upper_word",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify a name into a [`CasePattern`].
///
/// The classifier ignores leading/trailing underscores (common in Python
/// dunder methods and private convention). Names that are empty or consist
/// only of underscores/hyphens return [`CasePattern::Unknown`].
fn classify_case(name: &str) -> CasePattern {
    // Strip leading/trailing underscores (Python dunder, private).
    let stripped = name.trim_matches('_');
    if stripped.is_empty() {
        return CasePattern::Unknown;
    }

    let has_underscore = stripped.contains('_');
    let has_hyphen = stripped.contains('-');
    let has_upper = stripped.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = stripped.chars().any(|c| c.is_ascii_lowercase());
    let starts_upper = stripped
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase());

    // kebab-case: has hyphens, no underscores, lowercase
    if has_hyphen && !has_underscore && !has_upper {
        return CasePattern::KebabCase;
    }

    // SCREAMING_SNAKE_CASE: all uppercase + underscores (or all uppercase no separator)
    if has_upper && !has_lower && has_underscore {
        return CasePattern::ScreamingSnakeCase;
    }

    // Single uppercase word with no separators (e.g., "IO", "HTTP")
    if has_upper && !has_lower && !has_underscore && !has_hyphen {
        return CasePattern::SingleUpperWord;
    }

    // snake_case: has underscores, all lowercase between underscores
    if has_underscore && !has_upper {
        return CasePattern::SnakeCase;
    }

    // PascalCase: starts uppercase, no underscores
    if starts_upper && !has_underscore && !has_hyphen {
        return CasePattern::PascalCase;
    }

    // camelCase: starts lowercase, has uppercase letters, no underscores
    if !starts_upper && has_upper && !has_underscore && !has_hyphen {
        return CasePattern::CamelCase;
    }

    // Single lowercase word with no separators
    if has_lower && !has_upper && !has_underscore && !has_hyphen {
        return CasePattern::SingleLowerWord;
    }

    // Mixed / unrecognised (e.g., has both underscores and uppercase in non-SCREAMING form)
    CasePattern::Unknown
}

/// Extract the file stem from a path (without extension).
fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_owned())
}

// ---------------------------------------------------------------------------
// Per-language detection
// ---------------------------------------------------------------------------

/// Rust conventions: snake_case functions, PascalCase types (compiler-enforced).
/// Findings are reported as Observation (lower weight) since the compiler
/// already enforces these.
fn detect_rust(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    // Function naming
    detect_function_naming(file, &mut findings, Language::Rust);

    // Parameter naming
    detect_parameter_naming(file, &mut findings, Language::Rust);

    // Type naming
    detect_type_naming(file, &mut findings, Language::Rust);

    // File naming (Rust convention: snake_case file names)
    detect_file_naming(file, &mut findings, Language::Rust);

    // Constants: check for SCREAMING_SNAKE_CASE exports/types named all-uppercase
    detect_constant_naming_from_types(file, &mut findings, Language::Rust);

    findings
}

/// TypeScript conventions: camelCase functions, PascalCase types/interfaces/classes.
fn detect_typescript(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    detect_function_naming(file, &mut findings, Language::TypeScript);
    detect_parameter_naming(file, &mut findings, Language::TypeScript);
    detect_type_naming(file, &mut findings, Language::TypeScript);
    detect_file_naming(file, &mut findings, Language::TypeScript);
    detect_constant_naming_from_types(file, &mut findings, Language::TypeScript);

    findings
}

/// JavaScript conventions: camelCase functions, PascalCase classes.
fn detect_javascript(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    detect_function_naming(file, &mut findings, Language::JavaScript);
    detect_parameter_naming(file, &mut findings, Language::JavaScript);
    detect_type_naming(file, &mut findings, Language::JavaScript);
    detect_file_naming(file, &mut findings, Language::JavaScript);
    detect_constant_naming_from_types(file, &mut findings, Language::JavaScript);

    findings
}

/// Python conventions: snake_case functions, PascalCase classes, SCREAMING_SNAKE_CASE constants.
fn detect_python(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    detect_function_naming(file, &mut findings, Language::Python);
    detect_parameter_naming(file, &mut findings, Language::Python);
    detect_type_naming(file, &mut findings, Language::Python);
    detect_file_naming(file, &mut findings, Language::Python);
    detect_constant_naming_from_types(file, &mut findings, Language::Python);

    findings
}

// ---------------------------------------------------------------------------
// Shared detection helpers
// ---------------------------------------------------------------------------

/// Expected function naming convention per language.
fn expected_function_case(lang: Language) -> CasePattern {
    match lang {
        Language::Rust | Language::Python => CasePattern::SnakeCase,
        Language::TypeScript | Language::JavaScript => CasePattern::CamelCase,
    }
}

/// Expected type naming convention per language.
fn expected_type_case(_lang: Language) -> CasePattern {
    // PascalCase is the convention for types/classes across all 4 languages.
    CasePattern::PascalCase
}

/// Expected parameter naming convention per language.
///
/// Parameters follow the same case convention as function names:
/// snake_case for Rust/Python, camelCase for JS/TS.
fn expected_parameter_case(lang: Language) -> CasePattern {
    match lang {
        Language::Rust | Language::Python => CasePattern::SnakeCase,
        Language::TypeScript | Language::JavaScript => CasePattern::CamelCase,
    }
}

/// Expected file naming convention per language.
fn expected_file_case(lang: Language) -> CasePattern {
    match lang {
        Language::Rust | Language::Python => CasePattern::SnakeCase,
        Language::TypeScript | Language::JavaScript => CasePattern::KebabCase,
    }
}

/// Whether this language's naming conventions are compiler-enforced (lower weight).
fn is_compiler_enforced(lang: Language) -> bool {
    matches!(lang, Language::Rust)
}

/// Build nature for naming findings — Observation for compiler-enforced languages,
/// Convention otherwise.
fn finding_nature(lang: Language) -> KnowledgeNature {
    if is_compiler_enforced(lang) {
        KnowledgeNature::Observation
    } else {
        KnowledgeNature::Convention
    }
}

/// Detect naming patterns in function names.
fn detect_function_naming(
    file: &ProjectFile,
    findings: &mut Vec<ConventionFinding>,
    lang: Language,
) {
    if file.functions.is_empty() {
        return;
    }

    let expected = expected_function_case(lang);
    let (conforming, non_conforming) = classify_names(
        file.functions.iter().map(|f| (f.name.as_str(), f.line)),
        expected,
    );

    let total = conforming.len() + non_conforming.len();
    if total == 0 {
        return;
    }

    let adoption_pct = (conforming.len() as f64 / total as f64) * 100.0;
    let follows = !conforming.is_empty() && non_conforming.is_empty();

    let description = format!(
        "Function naming: {:.0}% of functions use {} (expected for {})",
        adoption_pct,
        expected.as_str(),
        lang,
    );

    let mut evidence = build_evidence_from_functions(&conforming, file, "follows convention");
    evidence.extend(build_evidence_from_functions(
        &non_conforming,
        file,
        "deviates from convention",
    ));
    // Limit evidence to 10 entries.
    evidence.truncate(10);

    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: finding_nature(lang),
        description,
        evidence,
        follows_convention: follows,
    });
}

/// Detect naming patterns in function parameter names.
///
/// Collects all parameter names from all functions in the file and classifies
/// them against the expected convention. Rust parameters are weighted lower
/// (compiler/clippy enforces snake_case), while JS/TS/Python parameters are
/// weighted higher (community-driven convention).
fn detect_parameter_naming(
    file: &ProjectFile,
    findings: &mut Vec<ConventionFinding>,
    lang: Language,
) {
    // Collect (param_name, function_line) pairs from all functions.
    let params: Vec<(&str, usize)> = file
        .functions
        .iter()
        .flat_map(|f| f.parameters.iter().map(move |p| (p.as_str(), f.line)))
        .collect();

    if params.is_empty() {
        return;
    }

    let expected = expected_parameter_case(lang);
    let (conforming, non_conforming) = classify_names(params.into_iter(), expected);

    let total = conforming.len() + non_conforming.len();
    if total == 0 {
        return;
    }

    let adoption_pct = (conforming.len() as f64 / total as f64) * 100.0;
    let follows = !conforming.is_empty() && non_conforming.is_empty();

    let description = format!(
        "Parameter naming: {:.0}% of parameters use {} (expected for {})",
        adoption_pct,
        expected.as_str(),
        lang,
    );

    let mut evidence: Vec<CodeEvidence> = conforming
        .iter()
        .map(|(name, line)| CodeEvidence {
            line: *line,
            end_line: *line,
            snippet: format!("param '{name}' (follows convention)"),
        })
        .collect();

    evidence.extend(non_conforming.iter().map(|(name, line)| CodeEvidence {
        line: *line,
        end_line: *line,
        snippet: format!("param '{name}' (deviates from convention)"),
    }));
    evidence.truncate(10);

    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: finding_nature(lang),
        description,
        evidence,
        follows_convention: follows,
    });
}

/// Detect naming patterns in type definitions.
fn detect_type_naming(file: &ProjectFile, findings: &mut Vec<ConventionFinding>, lang: Language) {
    // Separate types from constants/enum-variants that may be SCREAMING_SNAKE.
    let type_names: Vec<(&str, usize)> = file
        .types
        .iter()
        .filter(|t| !is_likely_constant_name(&t.name))
        .map(|t| (t.name.as_str(), t.line))
        .collect();

    if type_names.is_empty() {
        return;
    }

    let expected = expected_type_case(lang);
    let (conforming, non_conforming) = classify_names(type_names.into_iter(), expected);

    let total = conforming.len() + non_conforming.len();
    if total == 0 {
        return;
    }

    let adoption_pct = (conforming.len() as f64 / total as f64) * 100.0;
    let follows = !conforming.is_empty() && non_conforming.is_empty();

    let description = format!(
        "Type naming: {:.0}% of types use {} (expected for {})",
        adoption_pct,
        expected.as_str(),
        lang,
    );

    let mut evidence = build_evidence_from_types(&conforming, file, "follows convention");
    evidence.extend(build_evidence_from_types(
        &non_conforming,
        file,
        "deviates from convention",
    ));
    evidence.truncate(10);

    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: finding_nature(lang),
        description,
        evidence,
        follows_convention: follows,
    });
}

/// Detect file naming convention.
///
/// Conforming files use a generalized description for proper aggregation
/// (all conforming files for the same language produce the same description).
/// Non-conforming files keep a specific description with the stem for
/// diagnostic value.
fn detect_file_naming(file: &ProjectFile, findings: &mut Vec<ConventionFinding>, lang: Language) {
    let stem = match file_stem(&file.path) {
        Some(s) => s,
        None => return,
    };

    // Skip special files: mod.rs, lib.rs, main.rs, index.ts/js, __init__.py, etc.
    if is_special_filename(&stem, lang) {
        return;
    }

    let pattern = classify_case(&stem);
    let expected = expected_file_case(lang);

    // Single-word files are ambiguous — match SnakeCase or KebabCase expectations.
    let follows = matches_expected(pattern, expected);

    let description = if follows {
        // Conforming: use the expected pattern name (not the raw classified pattern).
        // This ensures single-word files like "utils" display as "snake_case" instead
        // of "single_lower_word", and all conforming files aggregate into one entry.
        format!("File naming: {} convention ({})", expected.as_str(), lang,)
    } else {
        // Non-conforming: keep specific description for diagnostic value.
        format!(
            "File naming: '{}' uses {} (expected {} for {})",
            stem,
            pattern.as_str(),
            expected.as_str(),
            lang,
        )
    };

    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: finding_nature(lang),
        description,
        evidence: vec![CodeEvidence {
            line: 0,
            end_line: 0,
            snippet: format!("file: {}", file.path.display()),
        }],
        follows_convention: follows,
    });
}

/// Detect constant-style naming from type names that look like constants
/// (SCREAMING_SNAKE_CASE or single uppercase words).
fn detect_constant_naming_from_types(
    file: &ProjectFile,
    findings: &mut Vec<ConventionFinding>,
    lang: Language,
) {
    let constant_names: Vec<(&str, usize)> = file
        .types
        .iter()
        .filter(|t| is_likely_constant_name(&t.name))
        .map(|t| (t.name.as_str(), t.line))
        .collect();

    if constant_names.is_empty() {
        return;
    }

    let (screaming, other): (Vec<_>, Vec<_>) = constant_names
        .into_iter()
        .partition(|(name, _)| classify_case(name) == CasePattern::ScreamingSnakeCase);

    if screaming.is_empty() && other.is_empty() {
        return;
    }

    let total = screaming.len() + other.len();
    let adoption_pct = (screaming.len() as f64 / total as f64) * 100.0;
    let follows = !screaming.is_empty() && other.is_empty();

    let description = format!(
        "Constant naming: {:.0}% use SCREAMING_SNAKE_CASE ({} language)",
        adoption_pct, lang,
    );

    let mut evidence: Vec<CodeEvidence> = screaming
        .iter()
        .map(|(name, line)| CodeEvidence {
            line: *line,
            end_line: *line,
            snippet: format!("{name} (SCREAMING_SNAKE_CASE)"),
        })
        .collect();

    evidence.extend(other.iter().map(|(name, line)| CodeEvidence {
        line: *line,
        end_line: *line,
        snippet: format!("{name} (non-standard constant naming)"),
    }));
    evidence.truncate(10);

    findings.push(ConventionFinding {
        file_path: file.path.clone(),
        detector_name: DETECTOR_NAME.to_owned(),
        nature: finding_nature(lang),
        description,
        evidence,
        follows_convention: follows,
    });
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Check if a name is likely a constant (all uppercase, possibly with underscores).
fn is_likely_constant_name(name: &str) -> bool {
    let stripped = name.trim_matches('_');
    !stripped.is_empty()
        && stripped
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
}

/// Check if a file stem is a "special" file name that should be excluded from
/// naming convention analysis (e.g., `mod`, `lib`, `main`, `index`, `__init__`).
fn is_special_filename(stem: &str, lang: Language) -> bool {
    match lang {
        Language::Rust => matches!(stem, "mod" | "lib" | "main" | "build"),
        Language::TypeScript | Language::JavaScript => {
            matches!(stem, "index" | "vite.config" | "tsconfig")
                || stem.ends_with(".config")
                || stem.ends_with(".d")
        }
        Language::Python => matches!(stem, "__init__" | "__main__" | "setup" | "conftest"),
    }
}

/// Check whether a classified pattern matches the expected convention.
///
/// Single-word names are treated as conforming to the expected convention
/// since they are ambiguous (a single lowercase word is valid snake_case,
/// camelCase, and kebab-case).
fn matches_expected(pattern: CasePattern, expected: CasePattern) -> bool {
    if pattern == expected {
        return true;
    }

    match (pattern, expected) {
        // Single lowercase word matches snake_case, camelCase, or kebab-case.
        (CasePattern::SingleLowerWord, CasePattern::SnakeCase)
        | (CasePattern::SingleLowerWord, CasePattern::CamelCase)
        | (CasePattern::SingleLowerWord, CasePattern::KebabCase) => true,

        // Single uppercase word matches PascalCase or SCREAMING_SNAKE_CASE.
        (CasePattern::SingleUpperWord, CasePattern::PascalCase)
        | (CasePattern::SingleUpperWord, CasePattern::ScreamingSnakeCase) => true,

        _ => false,
    }
}

/// A name with its source line number.
type NameEntry<'a> = (&'a str, usize);

/// Partition names into conforming and non-conforming based on expected pattern.
fn classify_names<'a>(
    names: impl Iterator<Item = NameEntry<'a>>,
    expected: CasePattern,
) -> (Vec<NameEntry<'a>>, Vec<NameEntry<'a>>) {
    let mut conforming = Vec::new();
    let mut non_conforming = Vec::new();

    for (name, line) in names {
        let pattern = classify_case(name);
        if matches_expected(pattern, expected) {
            conforming.push((name, line));
        } else if pattern != CasePattern::Unknown {
            non_conforming.push((name, line));
        }
        // Unknown patterns are excluded from both counts.
    }

    (conforming, non_conforming)
}

/// Build [`CodeEvidence`] entries from function matches.
fn build_evidence_from_functions(
    names: &[(&str, usize)],
    file: &ProjectFile,
    label: &str,
) -> Vec<CodeEvidence> {
    let func_map: HashMap<&str, &Function> = file
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();

    names
        .iter()
        .filter_map(|(name, _)| {
            func_map.get(name).map(|f| CodeEvidence {
                line: f.line,
                end_line: f.line,
                snippet: format!("fn {} ({label})", f.name),
            })
        })
        .collect()
}

/// Build [`CodeEvidence`] entries from type matches.
fn build_evidence_from_types(
    names: &[(&str, usize)],
    file: &ProjectFile,
    label: &str,
) -> Vec<CodeEvidence> {
    let type_map: HashMap<&str, &TypeDef> =
        file.types.iter().map(|t| (t.name.as_str(), t)).collect();

    names
        .iter()
        .filter_map(|(name, _)| {
            type_map.get(name).map(|t| CodeEvidence {
                line: t.line,
                end_line: t.line,
                snippet: format!("{:?} {} ({label})", t.kind, t.name),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{
        JavaScriptIR, Language, PythonIR, RustIR, TypeDef, TypeDefKind, TypeScriptIR,
    };
    use std::path::PathBuf;

    // -- Test helpers -------------------------------------------------------

    fn func(name: &str, line: usize) -> Function {
        Function {
            name: name.to_owned(),
            is_public: true,
            is_async: false,
            line,
            end_line: line + 5,
            parameters: vec![],
        }
    }

    fn typedef(name: &str, kind: TypeDefKind, line: usize) -> TypeDef {
        TypeDef {
            name: name.to_owned(),
            kind,
            is_public: true,
            line,
        }
    }

    fn make_rust_file(path: &str, functions: Vec<Function>, types: Vec<TypeDef>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        }
    }

    fn make_ts_file(path: &str, functions: Vec<Function>, types: Vec<TypeDef>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
        }
    }

    fn make_js_file(path: &str, functions: Vec<Function>, types: Vec<TypeDef>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::JavaScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR::default()),
        }
    }

    fn make_py_file(path: &str, functions: Vec<Function>, types: Vec<TypeDef>) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Python,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
        }
    }

    // -- Case classification tests ------------------------------------------

    #[test]
    fn classify_snake_case() {
        assert_eq!(classify_case("my_function"), CasePattern::SnakeCase);
        assert_eq!(classify_case("get_user_by_id"), CasePattern::SnakeCase);
        assert_eq!(classify_case("parse_args"), CasePattern::SnakeCase);
    }

    #[test]
    fn classify_camel_case() {
        assert_eq!(classify_case("myFunction"), CasePattern::CamelCase);
        assert_eq!(classify_case("getUserById"), CasePattern::CamelCase);
        assert_eq!(classify_case("parseArgs"), CasePattern::CamelCase);
    }

    #[test]
    fn classify_pascal_case() {
        assert_eq!(classify_case("MyStruct"), CasePattern::PascalCase);
        assert_eq!(classify_case("UserService"), CasePattern::PascalCase);
        assert_eq!(classify_case("HttpClient"), CasePattern::PascalCase);
    }

    #[test]
    fn classify_screaming_snake() {
        assert_eq!(
            classify_case("MAX_RETRIES"),
            CasePattern::ScreamingSnakeCase
        );
        assert_eq!(
            classify_case("DEFAULT_TIMEOUT"),
            CasePattern::ScreamingSnakeCase
        );
        assert_eq!(
            classify_case("API_BASE_URL"),
            CasePattern::ScreamingSnakeCase
        );
    }

    #[test]
    fn classify_kebab_case() {
        assert_eq!(classify_case("my-component"), CasePattern::KebabCase);
        assert_eq!(classify_case("user-service"), CasePattern::KebabCase);
        assert_eq!(classify_case("api-handler"), CasePattern::KebabCase);
    }

    #[test]
    fn classify_single_word() {
        assert_eq!(classify_case("main"), CasePattern::SingleLowerWord);
        assert_eq!(classify_case("parse"), CasePattern::SingleLowerWord);
        assert_eq!(classify_case("IO"), CasePattern::SingleUpperWord);
        assert_eq!(classify_case("HTTP"), CasePattern::SingleUpperWord);
    }

    #[test]
    fn classify_with_leading_underscores() {
        // Python dunder: leading/trailing underscores are stripped.
        assert_eq!(classify_case("__init__"), CasePattern::SingleLowerWord);
        assert_eq!(classify_case("_private_func"), CasePattern::SnakeCase);
        assert_eq!(classify_case("__MyClass"), CasePattern::PascalCase);
    }

    #[test]
    fn classify_empty_and_underscore_only() {
        assert_eq!(classify_case(""), CasePattern::Unknown);
        assert_eq!(classify_case("_"), CasePattern::Unknown);
        assert_eq!(classify_case("___"), CasePattern::Unknown);
    }

    // -- matches_expected tests ---------------------------------------------

    #[test]
    fn single_lower_word_matches_expected_conventions() {
        assert!(matches_expected(
            CasePattern::SingleLowerWord,
            CasePattern::SnakeCase
        ));
        assert!(matches_expected(
            CasePattern::SingleLowerWord,
            CasePattern::CamelCase
        ));
        assert!(matches_expected(
            CasePattern::SingleLowerWord,
            CasePattern::KebabCase
        ));
        assert!(!matches_expected(
            CasePattern::SingleLowerWord,
            CasePattern::PascalCase
        ));
    }

    #[test]
    fn single_upper_word_matches_pascal_and_screaming() {
        assert!(matches_expected(
            CasePattern::SingleUpperWord,
            CasePattern::PascalCase
        ));
        assert!(matches_expected(
            CasePattern::SingleUpperWord,
            CasePattern::ScreamingSnakeCase
        ));
        assert!(!matches_expected(
            CasePattern::SingleUpperWord,
            CasePattern::SnakeCase
        ));
    }

    // -- Rust detection tests -----------------------------------------------

    #[test]
    fn rust_snake_case_functions_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file(
            "src/utils.rs",
            vec![
                func("get_user", 1),
                func("parse_config", 10),
                func("build_query", 20),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(fn_finding.follows_convention);
        assert!(fn_finding.description.contains("100%"));
        // Rust findings are Observation (compiler-enforced).
        assert_eq!(fn_finding.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn rust_camel_case_functions_deviate() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file(
            "src/bad.rs",
            vec![func("getUser", 1), func("parseConfig", 10)],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(!fn_finding.follows_convention);
        assert!(fn_finding.description.contains("0%"));
    }

    #[test]
    fn rust_pascal_case_types_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file(
            "src/types.rs",
            Vec::new(),
            vec![
                typedef("UserService", TypeDefKind::Struct, 1),
                typedef("ErrorKind", TypeDefKind::Enum, 10),
                typedef("Handler", TypeDefKind::Trait, 20),
            ],
        );
        let findings = detector.detect(&file);
        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("Type naming"))
            .expect("should have type naming finding");
        assert!(type_finding.follows_convention);
        assert!(type_finding.description.contains("100%"));
    }

    #[test]
    fn rust_file_naming_snake_case() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file("src/my_module.rs", Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        let file_finding = findings
            .iter()
            .find(|f| f.description.contains("File naming"))
            .expect("should have file naming finding");
        assert!(file_finding.follows_convention);
    }

    #[test]
    fn rust_special_files_excluded() {
        let detector = NamingConventionsDetector;
        for name in &["mod.rs", "lib.rs", "main.rs"] {
            let file = make_rust_file(&format!("src/{name}"), Vec::new(), Vec::new());
            let findings = detector.detect(&file);
            assert!(
                !findings
                    .iter()
                    .any(|f| f.description.contains("File naming")),
                "special file {name} should be excluded from file naming analysis"
            );
        }
    }

    // -- TypeScript detection tests -----------------------------------------

    #[test]
    fn ts_camel_case_functions_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file(
            "src/user-service.ts",
            vec![
                func("getUser", 1),
                func("parseConfig", 10),
                func("buildQuery", 20),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(fn_finding.follows_convention);
        assert!(fn_finding.description.contains("100%"));
        // TypeScript findings are Convention (community-driven).
        assert_eq!(fn_finding.nature, KnowledgeNature::Convention);
    }

    #[test]
    fn ts_mixed_function_naming() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file(
            "src/mixed.ts",
            vec![
                func("getUser", 1),       // camelCase (correct)
                func("parse_config", 10), // snake_case (deviation)
                func("BuildQuery", 20),   // PascalCase (deviation)
                func("fetchData", 30),    // camelCase (correct)
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(!fn_finding.follows_convention);
        // 2 out of 4 follow camelCase.
        assert!(fn_finding.description.contains("50%"));
    }

    #[test]
    fn ts_pascal_case_types_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file(
            "src/types.ts",
            Vec::new(),
            vec![
                typedef("UserService", TypeDefKind::Interface, 1),
                typedef("ErrorType", TypeDefKind::TypeAlias, 10),
                typedef("AppComponent", TypeDefKind::Class, 20),
            ],
        );
        let findings = detector.detect(&file);
        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("Type naming"))
            .expect("should have type naming finding");
        assert!(type_finding.follows_convention);
    }

    #[test]
    fn ts_kebab_case_file_follows_convention() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file("src/user-service.ts", Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        let file_finding = findings
            .iter()
            .find(|f| f.description.contains("File naming"))
            .expect("should have file naming finding");
        assert!(file_finding.follows_convention);
    }

    #[test]
    fn ts_snake_case_file_deviates() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file("src/user_service.ts", Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        let file_finding = findings
            .iter()
            .find(|f| f.description.contains("File naming"))
            .expect("should have file naming finding");
        assert!(!file_finding.follows_convention);
    }

    // -- JavaScript detection tests -----------------------------------------

    #[test]
    fn js_camel_case_functions_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_js_file(
            "src/utils.js",
            vec![func("handleClick", 1), func("formatDate", 10)],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(fn_finding.follows_convention);
        assert_eq!(fn_finding.nature, KnowledgeNature::Convention);
    }

    // -- Python detection tests ---------------------------------------------

    #[test]
    fn python_snake_case_functions_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_py_file(
            "src/utils.py",
            vec![
                func("get_user", 1),
                func("parse_config", 10),
                func("build_query", 20),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(fn_finding.follows_convention);
        assert_eq!(fn_finding.nature, KnowledgeNature::Convention);
    }

    #[test]
    fn python_pascal_case_classes_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_py_file(
            "src/models.py",
            Vec::new(),
            vec![
                typedef("UserModel", TypeDefKind::Class, 1),
                typedef("OrderService", TypeDefKind::Class, 10),
            ],
        );
        let findings = detector.detect(&file);
        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("Type naming"))
            .expect("should have type naming finding");
        assert!(type_finding.follows_convention);
    }

    #[test]
    fn python_camel_case_functions_deviate() {
        let detector = NamingConventionsDetector;
        let file = make_py_file(
            "src/bad.py",
            vec![func("getUser", 1), func("parseConfig", 10)],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(!fn_finding.follows_convention);
    }

    #[test]
    fn python_init_file_excluded() {
        let detector = NamingConventionsDetector;
        let file = make_py_file("src/__init__.py", Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("File naming")),
            "__init__.py should be excluded"
        );
    }

    // -- Constant naming tests ----------------------------------------------

    #[test]
    fn screaming_snake_constants_detected() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file(
            "src/config.rs",
            Vec::new(),
            vec![
                typedef("MAX_RETRIES", TypeDefKind::TypeAlias, 1),
                typedef("DEFAULT_TIMEOUT", TypeDefKind::TypeAlias, 5),
            ],
        );
        let findings = detector.detect(&file);
        let const_finding = findings
            .iter()
            .find(|f| f.description.contains("Constant naming"))
            .expect("should have constant naming finding");
        assert!(const_finding.follows_convention);
        assert!(const_finding.description.contains("100%"));
    }

    // -- Empty / no-op tests ------------------------------------------------

    #[test]
    fn empty_file_produces_no_findings() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file("src/empty.rs", Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        // Only a file naming finding (no functions/types).
        assert!(findings.len() <= 1);
        // No function or type findings.
        assert!(!findings.iter().any(|f| f.description.contains("Function")));
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("Type naming"))
        );
    }

    #[test]
    fn detector_name_is_correct() {
        let detector = NamingConventionsDetector;
        assert_eq!(detector.name(), "naming_conventions");
    }

    #[test]
    fn detector_supports_all_languages() {
        let detector = NamingConventionsDetector;
        assert_eq!(detector.supported_languages(), Language::all());
    }

    // -- Mixed-style tests (as required by acceptance criteria) ---------------

    #[test]
    fn mixed_naming_styles_in_rust() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file(
            "src/mixed_styles.rs",
            vec![
                func("good_function", 1), // snake_case (correct)
                func("badFunction", 10),  // camelCase (deviation)
                func("also_good", 20),    // snake_case (correct)
            ],
            vec![
                typedef("GoodStruct", TypeDefKind::Struct, 30), // PascalCase (correct)
                typedef("bad_struct", TypeDefKind::Struct, 40), // snake_case (deviation)
            ],
        );
        let findings = detector.detect(&file);

        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(!fn_finding.follows_convention);
        // 2/3 snake_case → ~67%.
        assert!(fn_finding.description.contains("67%"));

        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("Type naming"))
            .expect("should have type naming finding");
        assert!(!type_finding.follows_convention);
        // 1/2 PascalCase → 50%.
        assert!(type_finding.description.contains("50%"));
    }

    #[test]
    fn mixed_naming_styles_in_typescript() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file(
            "src/user-service.ts",
            vec![
                func("getUser", 1),      // camelCase (correct)
                func("get_config", 10),  // snake_case (deviation)
                func("FetchData", 20),   // PascalCase (deviation)
                func("handleEvent", 30), // camelCase (correct)
            ],
            vec![
                typedef("UserService", TypeDefKind::Interface, 40), // PascalCase (correct)
                typedef("error_handler", TypeDefKind::TypeAlias, 50), // snake_case (deviation)
            ],
        );
        let findings = detector.detect(&file);

        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(!fn_finding.follows_convention);
        assert!(fn_finding.description.contains("50%"));
    }

    #[test]
    fn mixed_naming_styles_in_python() {
        let detector = NamingConventionsDetector;
        let file = make_py_file(
            "src/service.py",
            vec![
                func("get_user", 1),     // snake_case (correct)
                func("getConfig", 10),   // camelCase (deviation)
                func("build_query", 20), // snake_case (correct)
            ],
            vec![
                typedef("UserModel", TypeDefKind::Class, 30), // PascalCase (correct)
                typedef("orderService", TypeDefKind::Class, 40), // camelCase (deviation)
                typedef("MAX_RETRIES", TypeDefKind::TypeAlias, 50), // constant (separate finding)
            ],
        );
        let findings = detector.detect(&file);

        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"))
            .expect("should have function naming finding");
        assert!(!fn_finding.follows_convention);
        assert!(fn_finding.description.contains("67%"));

        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("Type naming"))
            .expect("should have type naming finding");
        assert!(!type_finding.follows_convention);
        // 1/2 PascalCase (MAX_RETRIES excluded as constant).
        assert!(type_finding.description.contains("50%"));
    }

    #[test]
    fn single_word_function_treated_as_conforming() {
        let detector = NamingConventionsDetector;
        // Single-word functions are ambiguous — should be treated as conforming.
        let file = make_rust_file("src/lib.rs", vec![func("parse", 1)], Vec::new());
        let findings = detector.detect(&file);
        let fn_finding = findings
            .iter()
            .find(|f| f.description.contains("Function naming"));
        if let Some(finding) = fn_finding {
            assert!(finding.follows_convention);
        }
    }

    // -- Parameter naming helpers -------------------------------------------

    fn func_with_params(name: &str, line: usize, params: Vec<&str>) -> Function {
        Function {
            name: name.to_owned(),
            is_public: true,
            is_async: false,
            line,
            end_line: line + 5,
            parameters: params.into_iter().map(|p| p.to_owned()).collect(),
        }
    }

    // -- Parameter naming: Rust (Observation, lower weight) -----------------

    #[test]
    fn rust_snake_case_params_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file(
            "src/utils.rs",
            vec![
                func_with_params("get_user", 1, vec!["user_id", "include_deleted"]),
                func_with_params("parse_config", 10, vec!["file_path", "strict_mode"]),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(param_finding.follows_convention);
        assert!(param_finding.description.contains("100%"));
        // Rust findings are Observation (compiler-enforced).
        assert_eq!(param_finding.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn rust_camel_case_params_deviate() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file(
            "src/bad.rs",
            vec![func_with_params(
                "get_user",
                1,
                vec!["userId", "includeDeleted"],
            )],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(!param_finding.follows_convention);
        assert!(param_finding.description.contains("0%"));
    }

    // -- Parameter naming: TypeScript (Convention, higher weight) -----------

    #[test]
    fn ts_camel_case_params_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file(
            "src/user-service.ts",
            vec![
                func_with_params("getUser", 1, vec!["userId", "includeDeleted"]),
                func_with_params("parseConfig", 10, vec!["filePath", "strictMode"]),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(param_finding.follows_convention);
        assert!(param_finding.description.contains("100%"));
        // TS findings are Convention (community-driven).
        assert_eq!(param_finding.nature, KnowledgeNature::Convention);
    }

    #[test]
    fn ts_snake_case_params_deviate() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file(
            "src/bad.ts",
            vec![func_with_params(
                "getUser",
                1,
                vec!["user_id", "include_deleted"],
            )],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(!param_finding.follows_convention);
        assert!(param_finding.description.contains("0%"));
    }

    // -- Parameter naming: JavaScript (Convention, higher weight) -----------

    #[test]
    fn js_camel_case_params_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_js_file(
            "src/utils.js",
            vec![func_with_params(
                "handleClick",
                1,
                vec!["eventData", "targetElement"],
            )],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(param_finding.follows_convention);
        assert_eq!(param_finding.nature, KnowledgeNature::Convention);
    }

    #[test]
    fn js_snake_case_params_deviate() {
        let detector = NamingConventionsDetector;
        let file = make_js_file(
            "src/bad.js",
            vec![func_with_params(
                "handleClick",
                1,
                vec!["event_data", "target_element"],
            )],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(!param_finding.follows_convention);
    }

    // -- Parameter naming: Python (Convention, higher weight) ---------------

    #[test]
    fn python_snake_case_params_follow_convention() {
        let detector = NamingConventionsDetector;
        let file = make_py_file(
            "src/utils.py",
            vec![
                func_with_params("get_user", 1, vec!["user_id", "include_deleted"]),
                func_with_params("parse_config", 10, vec!["file_path", "strict_mode"]),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(param_finding.follows_convention);
        assert!(param_finding.description.contains("100%"));
        assert_eq!(param_finding.nature, KnowledgeNature::Convention);
    }

    #[test]
    fn python_camel_case_params_deviate() {
        let detector = NamingConventionsDetector;
        let file = make_py_file(
            "src/bad.py",
            vec![func_with_params(
                "get_user",
                1,
                vec!["userId", "includeDeleted"],
            )],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(!param_finding.follows_convention);
        assert!(param_finding.description.contains("0%"));
    }

    // -- Parameter naming: mixed styles ------------------------------------

    #[test]
    fn ts_mixed_param_naming() {
        let detector = NamingConventionsDetector;
        let file = make_ts_file(
            "src/service.ts",
            vec![
                func_with_params("getUser", 1, vec!["userId", "user_name"]), // 1 camel, 1 snake
                func_with_params("parseConfig", 10, vec!["filePath"]),       // 1 camel
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(!param_finding.follows_convention);
        // 2 out of 3 are camelCase → ~67%.
        assert!(param_finding.description.contains("67%"));
    }

    // -- Parameter naming: no parameters -----------------------------------

    #[test]
    fn no_params_produces_no_param_finding() {
        let detector = NamingConventionsDetector;
        let file = make_rust_file("src/lib.rs", vec![func("no_params", 1)], Vec::new());
        let findings = detector.detect(&file);
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("Parameter naming")),
            "functions with no parameters should not produce a parameter naming finding"
        );
    }

    // -- Parameter naming: single-word params (ambiguous) ------------------

    #[test]
    fn single_word_params_treated_as_conforming() {
        let detector = NamingConventionsDetector;
        // Single-word params are ambiguous — count as conforming for both snake_case and camelCase.
        let file = make_ts_file(
            "src/utils.ts",
            vec![func_with_params("process", 1, vec!["data", "options"])],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(param_finding.follows_convention);
    }

    // -- Parameter naming: evidence is capped at 10 entries ----------------

    #[test]
    fn param_evidence_capped_at_10() {
        let detector = NamingConventionsDetector;
        let many_params: Vec<&str> = vec![
            "param_one",
            "param_two",
            "param_three",
            "param_four",
            "param_five",
            "param_six",
            "param_seven",
            "param_eight",
            "param_nine",
            "param_ten",
            "param_eleven",
            "param_twelve",
        ];
        let file = make_rust_file(
            "src/many.rs",
            vec![func_with_params("big_function", 1, many_params)],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        let param_finding = findings
            .iter()
            .find(|f| f.description.contains("Parameter naming"))
            .expect("should have parameter naming finding");
        assert!(param_finding.evidence.len() <= 10);
    }

    // -- File naming: single-word description fix (AC 5, AC 6) ---------------

    #[test]
    fn single_word_file_uses_expected_pattern_in_description() {
        // AC 5: "utils.py" should produce "File naming: snake_case convention (Python)"
        let detector = NamingConventionsDetector;
        let file = make_py_file("src/utils.py", Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        let file_finding = findings
            .iter()
            .find(|f| f.description.contains("File naming"))
            .expect("should have file naming finding");
        assert!(file_finding.follows_convention);
        assert_eq!(
            file_finding.description,
            "File naming: snake_case convention (Python)"
        );
        // Must NOT contain "single_lower_word".
        assert!(
            !file_finding.description.contains("single_lower_word"),
            "description should use expected pattern name, not raw classified pattern"
        );
    }

    #[test]
    fn non_conforming_file_keeps_specific_description() {
        // Non-conforming: "MyFile.py" should keep specific description with stem.
        let detector = NamingConventionsDetector;
        let file = make_py_file("src/MyFile.py", Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        let file_finding = findings
            .iter()
            .find(|f| f.description.contains("File naming"))
            .expect("should have file naming finding");
        assert!(!file_finding.follows_convention);
        assert!(file_finding.description.contains("MyFile"));
        assert!(file_finding.description.contains("PascalCase"));
    }

    #[test]
    fn conforming_files_aggregate_into_one_description() {
        // AC 6: Multiple conforming Python files should all produce the SAME description.
        let detector = NamingConventionsDetector;
        let files = vec![
            make_py_file("src/utils.py", Vec::new(), Vec::new()), // single word
            make_py_file("src/my_module.py", Vec::new(), Vec::new()), // snake_case
            make_py_file("src/config.py", Vec::new(), Vec::new()), // single word
            make_py_file("src/data_loader.py", Vec::new(), Vec::new()), // snake_case
        ];

        let mut descriptions = std::collections::HashSet::new();
        for file in &files {
            let findings = detector.detect(file);
            if let Some(ff) = findings
                .iter()
                .find(|f| f.description.contains("File naming"))
            {
                assert!(ff.follows_convention);
                descriptions.insert(ff.description.clone());
            }
        }

        assert_eq!(
            descriptions.len(),
            1,
            "All conforming files should produce the same description for aggregation; got: {:?}",
            descriptions
        );
        assert!(descriptions.contains("File naming: snake_case convention (Python)"));
    }

    #[test]
    fn conforming_ts_files_use_kebab_case_description() {
        let detector = NamingConventionsDetector;
        // Both single-word and kebab-case conforming TS files aggregate.
        let files = vec![
            make_ts_file("src/utils.ts", Vec::new(), Vec::new()), // single word
            make_ts_file("src/user-service.ts", Vec::new(), Vec::new()), // kebab-case
        ];

        let mut descriptions = std::collections::HashSet::new();
        for file in &files {
            let findings = detector.detect(file);
            if let Some(ff) = findings
                .iter()
                .find(|f| f.description.contains("File naming"))
            {
                assert!(ff.follows_convention);
                descriptions.insert(ff.description.clone());
            }
        }

        assert_eq!(descriptions.len(), 1);
        assert!(descriptions.contains("File naming: kebab-case convention (TypeScript)"));
    }
}
