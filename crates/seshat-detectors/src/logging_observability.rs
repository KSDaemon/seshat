//! Logging and observability detector — library and structured vs unstructured preference.
//!
//! Identifies the canonical logging library per language by examining
//! [`DependencyUsage`] and [`Import`] entries. Detects whether a project
//! prefers structured logging (fields/key-value pairs) vs unstructured
//! (string interpolation). Conflicting logging libraries are flagged as
//! `Observation` findings.
//!
//! Supported languages: Rust, TypeScript, JavaScript, Python.

use std::collections::HashMap;
use std::path::Path;

use seshat_core::{
    CodeEvidence, ConventionFinding, DependencyUsage, Import, KnowledgeNature, Language,
    LanguageIR, ProjectFile,
};

use crate::trait_def::ConventionDetector;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DETECTOR_NAME: &str = "logging_observability";

/// Maximum number of evidence entries per finding.
const MAX_EVIDENCE: usize = 5;

// ---------------------------------------------------------------------------
// Logging library classification
// ---------------------------------------------------------------------------

/// Known logging library family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LoggingLibrary {
    // Rust
    Tracing,
    Log,
    Slog,
    // JS/TS
    Winston,
    Pino,
    Bunyan,
    Log4js,
    // Python
    StdlibLogging,
    Loguru,
    Structlog,
}

impl LoggingLibrary {
    /// Human-readable name for finding descriptions.
    fn as_str(self) -> &'static str {
        match self {
            Self::Tracing => "tracing",
            Self::Log => "log",
            Self::Slog => "slog",
            Self::Winston => "winston",
            Self::Pino => "pino",
            Self::Bunyan => "bunyan",
            Self::Log4js => "log4js",
            Self::StdlibLogging => "logging (stdlib)",
            Self::Loguru => "loguru",
            Self::Structlog => "structlog",
        }
    }

    /// Whether this library is inherently structured.
    fn is_structured(self) -> bool {
        matches!(
            self,
            Self::Tracing | Self::Slog | Self::Pino | Self::Bunyan | Self::Structlog
        )
    }
}

/// Classify a Rust package as a logging library.
fn classify_rust_logging(package: &str) -> Option<LoggingLibrary> {
    match package {
        "tracing"
        | "tracing-subscriber"
        | "tracing-log"
        | "tracing-appender"
        | "tracing-futures"
        | "tracing-opentelemetry" => Some(LoggingLibrary::Tracing),
        "log" | "env_logger" | "pretty_env_logger" | "flexi_logger" | "simple_logger" | "fern" => {
            Some(LoggingLibrary::Log)
        }
        "slog" | "slog-async" | "slog-term" | "slog-json" | "slog-scope" => {
            Some(LoggingLibrary::Slog)
        }
        _ => None,
    }
}

/// Classify a JS/TS package as a logging library.
fn classify_js_ts_logging(package: &str) -> Option<LoggingLibrary> {
    match package {
        "winston" | "winston-daily-rotate-file" | "winston-transport" => {
            Some(LoggingLibrary::Winston)
        }
        "pino" | "pino-pretty" | "pino-http" => Some(LoggingLibrary::Pino),
        "bunyan" => Some(LoggingLibrary::Bunyan),
        "log4js" => Some(LoggingLibrary::Log4js),
        _ => None,
    }
}

/// Classify a Python package as a logging library.
fn classify_python_logging(package: &str) -> Option<LoggingLibrary> {
    match package {
        "logging" => Some(LoggingLibrary::StdlibLogging),
        "loguru" => Some(LoggingLibrary::Loguru),
        "structlog" => Some(LoggingLibrary::Structlog),
        _ => None,
    }
}

/// Classify a package as a logging library for the given language.
fn classify_logging(package: &str, language: Language) -> Option<LoggingLibrary> {
    match language {
        Language::Rust => classify_rust_logging(package),
        Language::TypeScript | Language::JavaScript => classify_js_ts_logging(package),
        Language::Python => classify_python_logging(package),
    }
}

// ---------------------------------------------------------------------------
// Heuristic classification
// ---------------------------------------------------------------------------

/// Logging-related substrings for name-based heuristic detection.
const LOGGING_NAME_HINTS: &[&str] = &["log", "logger", "logging", "trace", "tracing", "observ"];

/// Log-level method/function names that suggest a logging API.
const LOG_API_NAMES: &[&str] = &["info", "debug", "warn", "error", "fatal", "trace"];

/// Check whether a package/module name looks like a logging library based on
/// name substring heuristics. Returns `true` only when the name is **not**
/// already classified as a known library for the given language.
fn is_heuristic_logging_name(name: &str, language: Language) -> bool {
    // Skip if it's already a known library.
    if classify_logging(name, language).is_some() {
        return false;
    }
    let lower = name.to_lowercase();
    LOGGING_NAME_HINTS.iter().any(|hint| lower.contains(hint))
}

/// Check whether an import's named bindings look like a logging API
/// (info, debug, warn, error, fatal, trace).
fn has_logging_api_shape(names: &[String]) -> bool {
    let matches = names
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            LOG_API_NAMES.contains(&lower.as_str())
        })
        .count();
    // At least 2 log-level names imported → strong signal.
    matches >= 2
}

// ---------------------------------------------------------------------------
// Structured vs unstructured heuristics
// ---------------------------------------------------------------------------

/// Detect whether a Rust file uses structured logging.
///
/// Structured indicators: named fields in tracing macros (e.g. `info!(count, "msg")`),
/// `#[instrument]` attribute.
/// Unstructured indicators: plain `log::info!("string {}", var)` or `println!`.
fn detect_rust_structured(file: &ProjectFile) -> Option<bool> {
    let has_tracing = file
        .dependencies_used
        .iter()
        .any(|d| classify_rust_logging(&d.package) == Some(LoggingLibrary::Tracing));

    if !has_tracing {
        // For the `log` crate, logging is inherently unstructured.
        let has_log = file
            .dependencies_used
            .iter()
            .any(|d| classify_rust_logging(&d.package) == Some(LoggingLibrary::Log));
        if has_log {
            return Some(false);
        }
        return None;
    }

    // tracing is inherently structured — check for `instrument` usage as extra signal.
    if let LanguageIR::Rust(ref ir) = file.language_ir {
        let has_instrument = ir
            .derive_macros
            .iter()
            .any(|d| d.derives.iter().any(|name| name == "instrument"));

        // If imports include `instrument`, it's a structured logging file.
        let imports_instrument = file
            .imports
            .iter()
            .any(|i| i.module == "tracing" && i.names.iter().any(|n| n == "instrument"));

        if has_instrument || imports_instrument {
            return Some(true);
        }
    }

    // tracing usage defaults to structured
    Some(true)
}

/// Detect whether a JS/TS file uses structured logging.
///
/// Structured: pino, bunyan (inherently structured), or winston with metadata objects.
/// Unstructured: console.log, winston with string templates only.
fn detect_js_ts_structured(file: &ProjectFile) -> Option<bool> {
    let libs: Vec<LoggingLibrary> = file
        .dependencies_used
        .iter()
        .filter_map(|d| classify_js_ts_logging(&d.package))
        .collect();

    if libs.is_empty() {
        // Check for console.log usage in imports — it's a built-in, so check functions.
        let has_console_fn = file
            .functions
            .iter()
            .any(|f| f.name.starts_with("console."));

        if has_console_fn {
            return Some(false);
        }
        return None;
    }

    // If any inherently structured library is used, consider it structured.
    if libs.iter().any(|l| l.is_structured()) {
        return Some(true);
    }

    // Winston and log4js are configurable — default to unstructured.
    Some(false)
}

/// Detect whether a Python file uses structured logging.
///
/// Structured: structlog (inherently structured), or stdlib `logging` with `extra={}`.
/// Unstructured: plain logging.info("message") without extra.
fn detect_python_structured(file: &ProjectFile) -> Option<bool> {
    let libs: Vec<LoggingLibrary> = collect_python_logging_libs(file);

    if libs.is_empty() {
        return None;
    }

    if libs.iter().any(|l| l.is_structured()) {
        return Some(true);
    }

    // For stdlib logging, check if imports include structlog-like patterns.
    // We can't see `extra={}` in the IR, but we can note it's stdlib.
    Some(false)
}

/// Collect all logging libraries detected in a Python file's dependencies and imports.
fn collect_python_logging_libs(file: &ProjectFile) -> Vec<LoggingLibrary> {
    let mut libs: Vec<LoggingLibrary> = file
        .dependencies_used
        .iter()
        .filter_map(|d| classify_python_logging(&d.package))
        .collect();

    // Also check imports directly — Python's `import logging` may not appear
    // in dependencies_used since it's stdlib.
    for imp in &file.imports {
        if let Some(lib) = classify_python_logging(&imp.module) {
            if !libs.contains(&lib) {
                libs.push(lib);
            }
        }
    }

    libs
}

// ---------------------------------------------------------------------------
// Heuristic finding generation
// ---------------------------------------------------------------------------

/// Generate heuristic logging findings for dependencies and imports that are
/// not matched by known-library classification but have logging-related names
/// or API shapes. All heuristic findings use [`KnowledgeNature::Observation`]
/// which maps to lower confidence.
fn detect_heuristic_logging(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    // --- Name-based heuristic: dependency name contains logging keywords ---
    let heuristic_deps: Vec<&DependencyUsage> = file
        .dependencies_used
        .iter()
        .filter(|d| is_heuristic_logging_name(&d.package, file.language))
        .collect();

    if !heuristic_deps.is_empty() {
        let pkg_name = &heuristic_deps[0].package;
        let evidence: Vec<CodeEvidence> = heuristic_deps
            .iter()
            .take(MAX_EVIDENCE)
            .map(|d| CodeEvidence {
                file: file.path.clone(),
                line: d.line,
                end_line: d.line,
                snippet: String::new(),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!("Possible logging library (name heuristic): {pkg_name}"),
            evidence,
            follows_convention: true,
        });
    }

    // --- Name-based heuristic: import module name contains logging keywords ---
    let heuristic_imports: Vec<&Import> = file
        .imports
        .iter()
        .filter(|imp| {
            is_heuristic_logging_name(&imp.module, file.language)
                // Don't duplicate what was already caught via dependencies_used.
                && !heuristic_deps.iter().any(|d| d.package == imp.module)
        })
        .collect();

    for imp in heuristic_imports.iter().take(1) {
        let evidence = vec![CodeEvidence {
            file: file.path.clone(),
            line: imp.line,
            end_line: imp.line,
            snippet: String::new(),
        }];

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!("Possible logging library (name heuristic): {}", imp.module),
            evidence,
            follows_convention: true,
        });
    }

    // --- API shape heuristic: import with log-level named bindings ---
    let api_shape_imports: Vec<&Import> = file
        .imports
        .iter()
        .filter(|imp| {
            // Only for modules that are NOT already known.
            classify_logging(&imp.module, file.language).is_none()
                && has_logging_api_shape(&imp.names)
        })
        .collect();

    for imp in api_shape_imports.iter().take(1) {
        let _log_names: Vec<&str> = imp
            .names
            .iter()
            .filter(|n| LOG_API_NAMES.contains(&n.to_lowercase().as_str()))
            .map(String::as_str)
            .collect();

        let evidence = vec![CodeEvidence {
            file: file.path.clone(),
            line: imp.line,
            end_line: imp.line,
            snippet: String::new(),
        }];

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!("Possible structured logging (API shape): {}", imp.module),
            evidence,
            follows_convention: true,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Per-language detection
// ---------------------------------------------------------------------------

/// Collect logging-related evidence from DependencyUsage entries.
fn dependency_evidence(
    deps: &[DependencyUsage],
    language: Language,
    file_path: &Path,
) -> Vec<(LoggingLibrary, Vec<CodeEvidence>)> {
    let mut lib_evidence: HashMap<LoggingLibrary, Vec<CodeEvidence>> = HashMap::new();

    for dep in deps {
        if let Some(lib) = classify_logging(&dep.package, language) {
            let evidence = lib_evidence.entry(lib).or_default();
            if evidence.len() < MAX_EVIDENCE {
                evidence.push(CodeEvidence {
                    file: file_path.to_path_buf(),
                    line: dep.line,
                    end_line: dep.line,
                    snippet: String::new(),
                });
            }
        }
    }

    lib_evidence.into_iter().collect()
}

/// Collect logging-related evidence from Import entries.
fn import_evidence(
    imports: &[Import],
    language: Language,
    file_path: &Path,
) -> Vec<(LoggingLibrary, Vec<CodeEvidence>)> {
    let mut lib_evidence: HashMap<LoggingLibrary, Vec<CodeEvidence>> = HashMap::new();

    for imp in imports {
        if let Some(lib) = classify_logging(&imp.module, language) {
            let evidence = lib_evidence.entry(lib).or_default();
            if evidence.len() < MAX_EVIDENCE {
                evidence.push(CodeEvidence {
                    file: file_path.to_path_buf(),
                    line: imp.line,
                    end_line: imp.line,
                    snippet: String::new(),
                });
            }
        }
    }

    lib_evidence.into_iter().collect()
}

/// Merge two evidence maps, combining evidence per library.
fn merge_evidence(
    a: Vec<(LoggingLibrary, Vec<CodeEvidence>)>,
    b: Vec<(LoggingLibrary, Vec<CodeEvidence>)>,
) -> HashMap<LoggingLibrary, Vec<CodeEvidence>> {
    let mut merged: HashMap<LoggingLibrary, Vec<CodeEvidence>> = HashMap::new();

    for (lib, ev) in a {
        merged.entry(lib).or_default().extend(ev);
    }
    for (lib, ev) in b {
        let entry = merged.entry(lib).or_default();
        for e in ev {
            if entry.len() < MAX_EVIDENCE {
                entry.push(e);
            }
        }
    }

    merged
}

/// Detect logging patterns in a Rust file.
fn detect_rust(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    let dep_ev = dependency_evidence(&file.dependencies_used, Language::Rust, &file.path);
    let imp_ev = import_evidence(&file.imports, Language::Rust, &file.path);
    let merged = merge_evidence(dep_ev, imp_ev);

    if merged.is_empty() {
        // No known library found — try heuristic detection.
        return detect_heuristic_logging(file);
    }

    // Determine the primary (most evidence) logging library.
    let primary = merged
        .iter()
        .max_by_key(|(_, ev)| ev.len())
        .map(|(lib, _)| *lib);

    // Report canonical library finding.
    if let Some(lib) = primary {
        let evidence: Vec<CodeEvidence> = merged
            .get(&lib)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(MAX_EVIDENCE)
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Canonical logging library: {}", lib.as_str()),
            evidence,
            follows_convention: true,
        });
    }

    // Flag conflicting libraries.
    if merged.len() > 1 {
        let mut lib_names: Vec<&str> = merged.keys().map(|l| l.as_str()).collect();
        lib_names.sort();
        let all_evidence: Vec<CodeEvidence> = merged
            .values()
            .flat_map(|ev| ev.iter().cloned())
            .take(MAX_EVIDENCE)
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!(
                "Conflicting logging libraries in same file: {}",
                lib_names.join(", ")
            ),
            evidence: all_evidence,
            follows_convention: false,
        });
    }

    // Structured vs unstructured.
    if let Some(is_structured) = detect_rust_structured(file) {
        let style = if is_structured {
            "structured"
        } else {
            "unstructured"
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Logging style: {style} logging"),
            evidence: merged
                .values()
                .flat_map(|ev| ev.iter().cloned())
                .take(MAX_EVIDENCE)
                .collect(),
            follows_convention: true,
        });
    }

    findings
}

/// Detect logging patterns in a TypeScript file.
fn detect_typescript(file: &ProjectFile) -> Vec<ConventionFinding> {
    detect_js_ts(file)
}

/// Detect logging patterns in a JavaScript file.
fn detect_javascript(file: &ProjectFile) -> Vec<ConventionFinding> {
    detect_js_ts(file)
}

/// Shared JS/TS logging detection.
fn detect_js_ts(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    let dep_ev = dependency_evidence(&file.dependencies_used, file.language, &file.path);
    let imp_ev = import_evidence(&file.imports, file.language, &file.path);
    let merged = merge_evidence(dep_ev, imp_ev);

    if merged.is_empty() {
        // No known library found — try heuristic detection.
        return detect_heuristic_logging(file);
    }

    // Determine the primary logging library.
    let primary = merged
        .iter()
        .max_by_key(|(_, ev)| ev.len())
        .map(|(lib, _)| *lib);

    if let Some(lib) = primary {
        let evidence: Vec<CodeEvidence> = merged
            .get(&lib)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(MAX_EVIDENCE)
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Canonical logging library: {}", lib.as_str()),
            evidence,
            follows_convention: true,
        });
    }

    // Flag conflicting libraries.
    if merged.len() > 1 {
        let mut lib_names: Vec<&str> = merged.keys().map(|l| l.as_str()).collect();
        lib_names.sort();
        let all_evidence: Vec<CodeEvidence> = merged
            .values()
            .flat_map(|ev| ev.iter().cloned())
            .take(MAX_EVIDENCE)
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!(
                "Conflicting logging libraries in same file: {}",
                lib_names.join(", ")
            ),
            evidence: all_evidence,
            follows_convention: false,
        });
    }

    // Structured vs unstructured.
    if let Some(is_structured) = detect_js_ts_structured(file) {
        let style = if is_structured {
            "structured"
        } else {
            "unstructured"
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Logging style: {style} logging"),
            evidence: merged
                .values()
                .flat_map(|ev| ev.iter().cloned())
                .take(MAX_EVIDENCE)
                .collect(),
            follows_convention: true,
        });
    }

    findings
}

/// Detect logging patterns in a Python file.
fn detect_python(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    let dep_ev = dependency_evidence(&file.dependencies_used, Language::Python, &file.path);
    let imp_ev = import_evidence(&file.imports, Language::Python, &file.path);
    let merged = merge_evidence(dep_ev, imp_ev);

    if merged.is_empty() {
        // No known library found — try heuristic detection.
        return detect_heuristic_logging(file);
    }

    // Determine the primary logging library.
    let primary = merged
        .iter()
        .max_by_key(|(_, ev)| ev.len())
        .map(|(lib, _)| *lib);

    if let Some(lib) = primary {
        let evidence: Vec<CodeEvidence> = merged
            .get(&lib)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(MAX_EVIDENCE)
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Canonical logging library: {}", lib.as_str()),
            evidence,
            follows_convention: true,
        });
    }

    // Flag conflicting libraries.
    if merged.len() > 1 {
        let mut lib_names: Vec<&str> = merged.keys().map(|l| l.as_str()).collect();
        lib_names.sort();
        let all_evidence: Vec<CodeEvidence> = merged
            .values()
            .flat_map(|ev| ev.iter().cloned())
            .take(MAX_EVIDENCE)
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!(
                "Conflicting logging libraries in same file: {}",
                lib_names.join(", ")
            ),
            evidence: all_evidence,
            follows_convention: false,
        });
    }

    // Structured vs unstructured.
    if let Some(is_structured) = detect_python_structured(file) {
        let style = if is_structured {
            "structured"
        } else {
            "unstructured"
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Logging style: {style} logging"),
            evidence: merged
                .values()
                .flat_map(|ev| ev.iter().cloned())
                .take(MAX_EVIDENCE)
                .collect(),
            follows_convention: true,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects logging and observability patterns across all four supported languages.
///
/// Produces:
/// - **Convention** findings for the canonical logging library and logging style.
/// - **Observation** findings for conflicting logging libraries in the same file.
pub struct LoggingObservabilityDetector;

impl ConventionDetector for LoggingObservabilityDetector {
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
    use seshat_core::{JavaScriptIR, PythonIR, RustIR, TypeScriptIR};
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

    fn make_dep(package: &str, import_path: &str, line: usize) -> DependencyUsage {
        DependencyUsage {
            package: package.to_owned(),
            import_path: import_path.to_owned(),
            line,
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

    // -- Trait basics --

    #[test]
    fn detector_name() {
        let detector = LoggingObservabilityDetector;
        assert_eq!(detector.name(), "logging_observability");
    }

    #[test]
    fn supports_all_languages() {
        let detector = LoggingObservabilityDetector;
        assert_eq!(detector.supported_languages().len(), 4);
    }

    #[test]
    fn empty_file_no_findings() {
        let detector = LoggingObservabilityDetector;
        let file = make_rust_file("src/lib.rs");
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    // -- Rust --

    #[test]
    fn rust_tracing_library_detected() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/server.rs");
        file.dependencies_used = vec![make_dep("tracing", "tracing", 1)];
        file.imports = vec![make_import("tracing", &["info", "warn", "error"], 1)];

        let findings = detector.detect(&file);
        assert!(!findings.is_empty());

        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should have canonical finding");
        assert!(canonical.description.contains("tracing"));
        assert_eq!(canonical.nature, KnowledgeNature::Convention);
        assert!(canonical.follows_convention);
    }

    #[test]
    fn rust_log_library_detected() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/main.rs");
        file.dependencies_used = vec![make_dep("log", "log", 1)];
        file.imports = vec![make_import("log", &["info", "debug"], 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should have canonical finding");
        assert!(canonical.description.contains("log"));
    }

    #[test]
    fn rust_conflicting_libraries_flagged() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/mixed.rs");
        file.dependencies_used = vec![make_dep("tracing", "tracing", 1), make_dep("log", "log", 5)];
        file.imports = vec![
            make_import("tracing", &["info"], 1),
            make_import("log", &["warn"], 5),
        ];

        let findings = detector.detect(&file);
        let conflict = findings
            .iter()
            .find(|f| f.description.contains("Conflicting"))
            .expect("should flag conflicting libraries");
        assert_eq!(conflict.nature, KnowledgeNature::Observation);
        assert!(!conflict.follows_convention);
    }

    #[test]
    fn rust_tracing_structured() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/handler.rs");
        file.dependencies_used = vec![make_dep("tracing", "tracing", 1)];
        file.imports = vec![make_import("tracing", &["info", "warn", "instrument"], 1)];

        let findings = detector.detect(&file);
        let style = findings
            .iter()
            .find(|f| f.description.contains("Logging style"))
            .expect("should detect logging style");
        assert!(style.description.contains("structured"));
    }

    #[test]
    fn rust_log_unstructured() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/util.rs");
        file.dependencies_used = vec![make_dep("log", "log", 1)];
        file.imports = vec![make_import("log", &["info"], 1)];

        let findings = detector.detect(&file);
        let style = findings
            .iter()
            .find(|f| f.description.contains("Logging style"))
            .expect("should detect logging style");
        assert!(style.description.contains("unstructured"));
    }

    #[test]
    fn rust_slog_detected() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/app.rs");
        file.dependencies_used = vec![make_dep("slog", "slog", 1)];
        file.imports = vec![make_import("slog", &["Logger", "info"], 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect slog");
        assert!(canonical.description.contains("slog"));
    }

    // -- TypeScript / JavaScript --

    #[test]
    fn ts_winston_detected() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_ts_file("src/logger.ts");
        file.dependencies_used = vec![make_dep("winston", "winston", 1)];
        file.imports = vec![make_import("winston", &["createLogger"], 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect winston");
        assert!(canonical.description.contains("winston"));
    }

    #[test]
    fn ts_pino_structured() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_ts_file("src/app.ts");
        file.dependencies_used = vec![make_dep("pino", "pino", 1)];
        file.imports = vec![make_import("pino", &[], 1)];

        let findings = detector.detect(&file);
        let style = findings
            .iter()
            .find(|f| f.description.contains("Logging style"))
            .expect("should detect logging style");
        assert!(style.description.contains("structured"));
    }

    #[test]
    fn js_winston_detected() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_js_file("src/logger.js");
        file.dependencies_used = vec![make_dep("winston", "winston", 1)];
        file.imports = vec![make_import("winston", &["createLogger"], 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect winston in JS");
        assert!(canonical.description.contains("winston"));
    }

    #[test]
    fn js_conflicting_libraries() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_js_file("src/mixed.js");
        file.dependencies_used = vec![
            make_dep("winston", "winston", 1),
            make_dep("pino", "pino", 5),
        ];
        file.imports = vec![
            make_import("winston", &["createLogger"], 1),
            make_import("pino", &[], 5),
        ];

        let findings = detector.detect(&file);
        let conflict = findings
            .iter()
            .find(|f| f.description.contains("Conflicting"))
            .expect("should flag conflicting JS logging libraries");
        assert_eq!(conflict.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn ts_bunyan_detected() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_ts_file("src/service.ts");
        file.dependencies_used = vec![make_dep("bunyan", "bunyan", 1)];
        file.imports = vec![make_import("bunyan", &["createLogger"], 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect bunyan");
        assert!(canonical.description.contains("bunyan"));
    }

    // -- Python --

    #[test]
    fn python_stdlib_logging_via_import() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_python_file("app/logger.py");
        file.imports = vec![make_import("logging", &[], 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect stdlib logging");
        assert!(canonical.description.contains("logging (stdlib)"));
    }

    #[test]
    fn python_loguru_detected() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_python_file("app/main.py");
        file.dependencies_used = vec![make_dep("loguru", "loguru", 1)];
        file.imports = vec![make_import("loguru", &["logger"], 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect loguru");
        assert!(canonical.description.contains("loguru"));
    }

    #[test]
    fn python_structlog_structured() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_python_file("app/structured.py");
        file.dependencies_used = vec![make_dep("structlog", "structlog", 1)];
        file.imports = vec![make_import("structlog", &["get_logger"], 1)];

        let findings = detector.detect(&file);
        let style = findings
            .iter()
            .find(|f| f.description.contains("Logging style"))
            .expect("should detect structured logging");
        assert!(style.description.contains("structured"));
    }

    #[test]
    fn python_stdlib_unstructured() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_python_file("app/basic.py");
        file.imports = vec![make_import("logging", &[], 1)];

        let findings = detector.detect(&file);
        let style = findings
            .iter()
            .find(|f| f.description.contains("Logging style"))
            .expect("should detect unstructured logging");
        assert!(style.description.contains("unstructured"));
    }

    #[test]
    fn python_conflicting_libraries() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_python_file("app/mixed.py");
        file.imports = vec![make_import("logging", &[], 1)];
        file.dependencies_used = vec![make_dep("loguru", "loguru", 5)];

        let findings = detector.detect(&file);
        let conflict = findings
            .iter()
            .find(|f| f.description.contains("Conflicting"))
            .expect("should flag conflicting Python logging libraries");
        assert_eq!(conflict.nature, KnowledgeNature::Observation);
    }

    // -- Evidence and edge cases --

    #[test]
    fn evidence_capped_at_max() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/many_imports.rs");
        // Create more than MAX_EVIDENCE dependency entries.
        file.dependencies_used = (0..10)
            .map(|i| make_dep("tracing", &format!("tracing::{i}"), i))
            .collect();

        let findings = detector.detect(&file);
        for finding in &findings {
            assert!(
                finding.evidence.len() <= MAX_EVIDENCE,
                "evidence should be capped at {MAX_EVIDENCE}, got {}",
                finding.evidence.len()
            );
        }
    }

    #[test]
    fn no_logging_no_findings() {
        let detector = LoggingObservabilityDetector;

        let files = [
            make_rust_file("src/lib.rs"),
            make_ts_file("src/utils.ts"),
            make_js_file("src/helpers.js"),
            make_python_file("app/models.py"),
        ];

        for file in &files {
            let findings = detector.detect(file);
            assert!(
                findings.is_empty(),
                "file {:?} should have no logging findings",
                file.path
            );
        }
    }

    #[test]
    fn classify_rust_logging_coverage() {
        assert_eq!(
            classify_rust_logging("tracing"),
            Some(LoggingLibrary::Tracing)
        );
        assert_eq!(
            classify_rust_logging("tracing-subscriber"),
            Some(LoggingLibrary::Tracing)
        );
        assert_eq!(classify_rust_logging("log"), Some(LoggingLibrary::Log));
        assert_eq!(
            classify_rust_logging("env_logger"),
            Some(LoggingLibrary::Log)
        );
        assert_eq!(classify_rust_logging("slog"), Some(LoggingLibrary::Slog));
        assert_eq!(classify_rust_logging("serde"), None);
    }

    #[test]
    fn classify_js_ts_logging_coverage() {
        assert_eq!(
            classify_js_ts_logging("winston"),
            Some(LoggingLibrary::Winston)
        );
        assert_eq!(classify_js_ts_logging("pino"), Some(LoggingLibrary::Pino));
        assert_eq!(
            classify_js_ts_logging("bunyan"),
            Some(LoggingLibrary::Bunyan)
        );
        assert_eq!(
            classify_js_ts_logging("log4js"),
            Some(LoggingLibrary::Log4js)
        );
        assert_eq!(classify_js_ts_logging("express"), None);
    }

    #[test]
    fn classify_python_logging_coverage() {
        assert_eq!(
            classify_python_logging("logging"),
            Some(LoggingLibrary::StdlibLogging)
        );
        assert_eq!(
            classify_python_logging("loguru"),
            Some(LoggingLibrary::Loguru)
        );
        assert_eq!(
            classify_python_logging("structlog"),
            Some(LoggingLibrary::Structlog)
        );
        assert_eq!(classify_python_logging("django"), None);
    }

    #[test]
    fn rust_tracing_subscriber_classified() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/setup.rs");
        file.dependencies_used = vec![make_dep("tracing-subscriber", "tracing_subscriber", 1)];

        let findings = detector.detect(&file);
        assert!(!findings.is_empty());
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect tracing");
        assert!(canonical.description.contains("tracing"));
    }

    #[test]
    fn ts_pino_http_classified() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_ts_file("src/middleware.ts");
        file.dependencies_used = vec![make_dep("pino-http", "pino-http", 1)];

        let findings = detector.detect(&file);
        let canonical = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should detect pino");
        assert!(canonical.description.contains("pino"));
    }

    #[test]
    fn python_import_and_dep_deduplication() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_python_file("app/service.py");
        // Same library via both import and dependency — should not double-count.
        file.imports = vec![make_import("logging", &[], 1)];
        file.dependencies_used = vec![make_dep("logging", "logging", 1)];

        let findings = detector.detect(&file);
        // Should have canonical + style, no conflicts.
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("Conflicting")),
            "same library via import and dep should not be flagged as conflict"
        );
    }

    // -- Heuristic: name-based detection --

    #[test]
    fn heuristic_name_based_dep_with_log_in_name() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/app.rs");
        // "my-logger" is not a known Rust logging library but contains "log".
        file.dependencies_used = vec![make_dep("my-logger", "my_logger", 1)];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| {
                f.description
                    .contains("Possible logging library (name heuristic)")
            })
            .expect("should detect heuristic logging by name");
        assert_eq!(heuristic.nature, KnowledgeNature::Observation);
        assert!(heuristic.follows_convention);
    }

    #[test]
    fn heuristic_name_based_import_with_tracing_in_name() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_ts_file("src/app.ts");
        // "custom-tracing-lib" is not a known JS/TS logging library.
        file.imports = vec![make_import("custom-tracing-lib", &["setup"], 1)];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| {
                f.description
                    .contains("Possible logging library (name heuristic)")
            })
            .expect("should detect heuristic logging by import name");
        assert_eq!(heuristic.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn heuristic_name_based_observability_in_name() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_python_file("app/telemetry.py");
        file.dependencies_used = vec![make_dep("observability-sdk", "observability_sdk", 1)];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| {
                f.description
                    .contains("Possible logging library (name heuristic)")
            })
            .expect("should detect heuristic logging by 'observ' substring");
        assert_eq!(heuristic.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn known_library_takes_priority_over_heuristic() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/app.rs");
        // "tracing" is a known library — should NOT produce heuristic findings.
        file.dependencies_used = vec![make_dep("tracing", "tracing", 1)];
        file.imports = vec![make_import("tracing", &["info", "warn"], 1)];

        let findings = detector.detect(&file);
        // Should have canonical finding.
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("Canonical logging library"))
        );
        // Should NOT have heuristic finding.
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("Possible logging"))
        );
    }

    #[test]
    fn no_heuristic_for_unrelated_dep() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_rust_file("src/lib.rs");
        // "serde" has no logging-related keywords.
        file.dependencies_used = vec![make_dep("serde", "serde", 1)];

        let findings = detector.detect(&file);
        assert!(
            findings.is_empty(),
            "unrelated dep should not trigger heuristic"
        );
    }

    // -- Heuristic: API shape detection --

    #[test]
    fn heuristic_api_shape_log_level_imports() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_js_file("src/app.js");
        // Unknown module but imports log-level functions.
        file.imports = vec![make_import(
            "my-custom-lib",
            &["info", "debug", "warn", "error"],
            1,
        )];

        let findings = detector.detect(&file);
        let heuristic = findings
            .iter()
            .find(|f| {
                f.description
                    .contains("Possible structured logging (API shape)")
            })
            .expect("should detect heuristic logging by API shape");
        assert_eq!(heuristic.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn heuristic_api_shape_insufficient_names() {
        let detector = LoggingObservabilityDetector;
        let mut file = make_js_file("src/app.js");
        // Only 1 log-level name — not enough signal.
        file.imports = vec![make_import("some-lib", &["info", "getData"], 1)];

        let findings = detector.detect(&file);
        assert!(
            !findings.iter().any(|f| f.description.contains("API shape")),
            "single log-level name should not trigger API shape heuristic"
        );
    }

    #[test]
    fn heuristic_name_based_does_not_double_count_known_lib() {
        // Verify that "loguru" is classified as known, not heuristic.
        assert!(!is_heuristic_logging_name("loguru", Language::Python));
        assert!(!is_heuristic_logging_name("tracing", Language::Rust));
        assert!(!is_heuristic_logging_name("winston", Language::TypeScript));
    }

    #[test]
    fn heuristic_name_based_matches_unknown_logging_names() {
        assert!(is_heuristic_logging_name("fast-logger", Language::Rust));
        assert!(is_heuristic_logging_name(
            "my-tracing-util",
            Language::TypeScript
        ));
        assert!(is_heuristic_logging_name(
            "observability-toolkit",
            Language::Python
        ));
        assert!(!is_heuristic_logging_name("serde", Language::Rust));
        assert!(!is_heuristic_logging_name("express", Language::JavaScript));
    }

    #[test]
    fn detect_with_source_sets_real_snippet() {
        let detector = LoggingObservabilityDetector;
        // TypeScript file with a winston import at line 1.
        let mut file = make_ts_file("src/logger.ts");
        file.dependencies_used = vec![make_dep("winston", "winston", 1)];
        file.imports = vec![make_import("winston", &["createLogger"], 1)];
        let source = "import winston from 'winston';\n";

        let findings = detector.detect_with_source(&file, source);

        assert!(!findings.is_empty(), "should have at least one finding");
        let finding = findings
            .iter()
            .find(|f| f.description.contains("Canonical logging library"))
            .expect("should have canonical logging library finding");
        assert!(!finding.evidence.is_empty(), "finding should have evidence");
        let ev = &finding.evidence[0];
        assert_eq!(ev.file, file.path);
        // Snippet must contain the actual import keyword from source.
        assert!(
            ev.snippet.contains("winston"),
            "snippet must contain real source keyword 'winston', got: {:?}",
            ev.snippet
        );
        assert!(
            !ev.snippet.starts_with("Custom "),
            "snippet must not be a synthetic format string, got: {:?}",
            ev.snippet
        );
    }
}
