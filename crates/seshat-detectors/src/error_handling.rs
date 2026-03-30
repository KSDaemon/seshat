//! Error handling detector — error types, propagation, and wrapping patterns.
//!
//! Analyzes parsed IR to detect error handling conventions across all four
//! supported languages:
//!
//! - **Rust**: thiserror vs anyhow vs custom error enums; `?` propagation;
//!   error wrapping via `map_err`/`context`.
//! - **TypeScript**: custom error classes vs plain `Error`; try-catch patterns;
//!   Result/Either patterns.
//! - **JavaScript**: error handling style (try-catch, callback errors, Promise
//!   rejection).
//! - **Python**: exception hierarchy (custom vs built-in); try-except patterns;
//!   error wrapping.
//!
//! The Rust detector leverages `RustIR::error_types` from parsed IR.

use seshat_core::{
    CodeEvidence, ConventionFinding, KnowledgeNature, Language, LanguageIR, ProjectFile, TypeDef,
    TypeDefKind,
};

use crate::trait_def::ConventionDetector;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DETECTOR_NAME: &str = "error_handling";

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects error handling conventions across all four supported languages.
///
/// Produces:
/// - **Convention** findings for the dominant error handling approach.
/// - **Observation** findings for alternative/conflicting patterns.
pub struct ErrorHandlingDetector;

impl ConventionDetector for ErrorHandlingDetector {
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
// Rust error handling detection
// ---------------------------------------------------------------------------

/// Rust error handling library detected via derive macros and imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RustErrorLib {
    Thiserror,
    Anyhow,
    Custom,
}

impl RustErrorLib {
    fn as_str(self) -> &'static str {
        match self {
            Self::Thiserror => "thiserror",
            Self::Anyhow => "anyhow",
            Self::Custom => "custom error enums",
        }
    }
}

fn detect_rust(file: &ProjectFile) -> Vec<ConventionFinding> {
    let rust_ir = match &file.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => return Vec::new(),
    };

    let mut findings = Vec::new();

    // --- Detect error library ---
    let has_thiserror = file
        .imports
        .iter()
        .any(|imp| imp.module == "thiserror" || imp.module.starts_with("thiserror::"));

    let has_thiserror_derive = rust_ir
        .derive_macros
        .iter()
        .any(|d| d.derives.iter().any(|name| name == "Error") && d.type_name.contains("Error"));

    let has_anyhow = file
        .imports
        .iter()
        .any(|imp| imp.module == "anyhow" || imp.module.starts_with("anyhow::"));

    let has_error_types = !rust_ir.error_types.is_empty();

    // Determine the dominant approach.
    let lib = if has_thiserror || has_thiserror_derive {
        Some(RustErrorLib::Thiserror)
    } else if has_anyhow {
        Some(RustErrorLib::Anyhow)
    } else if has_error_types {
        Some(RustErrorLib::Custom)
    } else {
        None
    };

    if let Some(error_lib) = lib {
        let evidence = build_rust_error_evidence(file, rust_ir, error_lib);
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Rust error handling: {}", error_lib.as_str()),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect conflicting libraries ---
    if has_thiserror && has_anyhow {
        let mut evidence = Vec::new();
        for imp in &file.imports {
            if imp.module == "thiserror" || imp.module.starts_with("thiserror::") {
                evidence.push(CodeEvidence {
                    line: imp.line,
                    end_line: imp.line,
                    snippet: format!("use {}", imp.module),
                });
            }
            if imp.module == "anyhow" || imp.module.starts_with("anyhow::") {
                evidence.push(CodeEvidence {
                    line: imp.line,
                    end_line: imp.line,
                    snippet: format!("use {}", imp.module),
                });
            }
        }

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Both thiserror and anyhow used in same file".to_owned(),
            evidence,
            follows_convention: false,
        });
    }

    // --- Detect error wrapping patterns (context/map_err) ---
    // We detect these via anyhow::Context import or dependency usage.
    let has_context_import = file
        .imports
        .iter()
        .any(|imp| imp.module.starts_with("anyhow") && imp.names.iter().any(|n| n == "Context"));

    if has_context_import {
        let evidence: Vec<CodeEvidence> = file
            .imports
            .iter()
            .filter(|imp| {
                imp.module.starts_with("anyhow") && imp.names.iter().any(|n| n == "Context")
            })
            .take(3)
            .map(|imp| CodeEvidence {
                line: imp.line,
                end_line: imp.line,
                snippet: format!("use {} (Context trait for error wrapping)", imp.module),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Error wrapping via anyhow::Context".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect error trait implementations ---
    let error_trait_impls: Vec<_> = rust_ir
        .trait_implementations
        .iter()
        .filter(|ti| ti.trait_name == "Error" || ti.trait_name == "std::error::Error")
        .collect();

    if !error_trait_impls.is_empty() {
        let evidence: Vec<CodeEvidence> = error_trait_impls
            .iter()
            .take(5)
            .map(|ti| CodeEvidence {
                line: ti.line,
                end_line: ti.line,
                snippet: format!("impl {} for {}", ti.trait_name, ti.type_name),
            })
            .collect();

        // Manual Error trait impls are notable when thiserror is not used.
        if !has_thiserror {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Manual std::error::Error trait implementation".to_owned(),
                evidence,
                follows_convention: true,
            });
        }
    }

    // --- Detect From conversions for error types ---
    let from_impls: Vec<_> = rust_ir
        .trait_implementations
        .iter()
        .filter(|ti| ti.trait_name.starts_with("From<") && has_error_in_name(&ti.type_name))
        .collect();

    if !from_impls.is_empty() {
        let evidence: Vec<CodeEvidence> = from_impls
            .iter()
            .take(5)
            .map(|ti| CodeEvidence {
                line: ti.line,
                end_line: ti.line,
                snippet: format!("impl {} for {}", ti.trait_name, ti.type_name),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!(
                "Error type conversion via From impls ({})",
                from_impls.len()
            ),
            evidence,
            follows_convention: true,
        });
    }

    findings
}

/// Build evidence entries for the detected Rust error library.
fn build_rust_error_evidence(
    file: &ProjectFile,
    rust_ir: &seshat_core::RustIR,
    lib: RustErrorLib,
) -> Vec<CodeEvidence> {
    let mut evidence = Vec::new();

    match lib {
        RustErrorLib::Thiserror => {
            // Show thiserror import.
            for imp in &file.imports {
                if imp.module == "thiserror" || imp.module.starts_with("thiserror::") {
                    evidence.push(CodeEvidence {
                        line: imp.line,
                        end_line: imp.line,
                        snippet: format!("use {}", imp.module),
                    });
                }
            }
            // Show derive(Error) usage.
            for d in &rust_ir.derive_macros {
                if d.derives.iter().any(|name| name == "Error") {
                    evidence.push(CodeEvidence {
                        line: d.line,
                        end_line: d.line,
                        snippet: format!("#[derive({})] on {}", d.derives.join(", "), d.type_name),
                    });
                }
            }
        }
        RustErrorLib::Anyhow => {
            for imp in &file.imports {
                if imp.module == "anyhow" || imp.module.starts_with("anyhow::") {
                    evidence.push(CodeEvidence {
                        line: imp.line,
                        end_line: imp.line,
                        snippet: format!("use {}", imp.module),
                    });
                }
            }
        }
        RustErrorLib::Custom => {
            // Show the error type names from RustIR.
            for (i, error_type) in rust_ir.error_types.iter().enumerate() {
                if i >= 5 {
                    break;
                }
                // Find the TypeDef line if available.
                let line = file
                    .types
                    .iter()
                    .find(|t| &t.name == error_type)
                    .map_or(0, |t| t.line);
                evidence.push(CodeEvidence {
                    line,
                    end_line: line,
                    snippet: format!("Custom error type: {error_type}"),
                });
            }
        }
    }

    // Cap evidence.
    evidence.truncate(5);
    evidence
}

// ---------------------------------------------------------------------------
// TypeScript error handling detection
// ---------------------------------------------------------------------------

fn detect_typescript(file: &ProjectFile) -> Vec<ConventionFinding> {
    let _ts_ir = match &file.language_ir {
        LanguageIR::TypeScript(ir) => ir,
        _ => return Vec::new(),
    };

    let mut findings = Vec::new();

    // --- Detect custom error classes ---
    let error_classes = collect_error_types(file, TypeDefKind::Class);

    if !error_classes.is_empty() {
        let evidence: Vec<CodeEvidence> = error_classes
            .iter()
            .take(5)
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
                "Custom error classes ({} found): {}",
                error_classes.len(),
                error_class_names(&error_classes),
            ),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect Result/Either pattern usage ---
    let has_result_type = file
        .types
        .iter()
        .any(|t| t.name == "Result" || t.name.ends_with("Result") || t.name == "Either");

    let has_result_import = file.imports.iter().any(|imp| {
        imp.names
            .iter()
            .any(|n| n == "Result" || n == "Either" || n == "Ok" || n == "Err")
            || imp.module.contains("result")
            || imp.module.contains("either")
            || imp.module.contains("neverthrow")
            || imp.module.contains("fp-ts")
    });

    if has_result_type || has_result_import {
        let mut evidence = Vec::new();
        for t in &file.types {
            if t.name == "Result" || t.name.ends_with("Result") || t.name == "Either" {
                evidence.push(CodeEvidence {
                    line: t.line,
                    end_line: t.line,
                    snippet: format!("type {}", t.name),
                });
            }
        }
        for imp in &file.imports {
            if imp.module.contains("neverthrow")
                || imp.module.contains("fp-ts")
                || imp.module.contains("either")
            {
                evidence.push(CodeEvidence {
                    line: imp.line,
                    end_line: imp.line,
                    snippet: format!("import from '{}'", imp.module),
                });
            }
        }
        evidence.truncate(3);

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Result/Either pattern for error handling".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect type guard functions for errors ---
    let type_guard_fns: Vec<_> = file
        .functions
        .iter()
        .filter(|f| {
            let lower = f.name.to_lowercase();
            lower.starts_with("is") && lower.contains("error")
        })
        .collect();

    if !type_guard_fns.is_empty() {
        let evidence: Vec<CodeEvidence> = type_guard_fns
            .iter()
            .take(3)
            .map(|f| CodeEvidence {
                line: f.line,
                end_line: f.end_line,
                snippet: format!("function {}()", f.name),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Error type guard functions detected".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// JavaScript error handling detection
// ---------------------------------------------------------------------------

fn detect_javascript(file: &ProjectFile) -> Vec<ConventionFinding> {
    let _js_ir = match &file.language_ir {
        LanguageIR::JavaScript(ir) => ir,
        _ => return Vec::new(),
    };

    let mut findings = Vec::new();

    // --- Detect custom error classes ---
    let error_classes = collect_error_types(file, TypeDefKind::Class);

    if !error_classes.is_empty() {
        let evidence: Vec<CodeEvidence> = error_classes
            .iter()
            .take(5)
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
                "Custom error classes ({} found): {}",
                error_classes.len(),
                error_class_names(&error_classes),
            ),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect Promise rejection patterns via imports ---
    let has_promise_libs = file
        .imports
        .iter()
        .any(|imp| imp.module.contains("bluebird") || imp.names.iter().any(|n| n == "Promise"));

    if has_promise_libs {
        let evidence: Vec<CodeEvidence> = file
            .imports
            .iter()
            .filter(|imp| {
                imp.module.contains("bluebird") || imp.names.iter().any(|n| n == "Promise")
            })
            .take(3)
            .map(|imp| CodeEvidence {
                line: imp.line,
                end_line: imp.line,
                snippet: format!("import from '{}'", imp.module),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Promise library for error handling".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect error handler functions ---
    let error_handler_fns: Vec<_> = file
        .functions
        .iter()
        .filter(|f| {
            let lower = f.name.to_lowercase();
            lower.contains("error") || lower.contains("handleerr") || lower == "onerror"
        })
        .collect();

    if !error_handler_fns.is_empty() {
        let evidence: Vec<CodeEvidence> = error_handler_fns
            .iter()
            .take(3)
            .map(|f| CodeEvidence {
                line: f.line,
                end_line: f.end_line,
                snippet: format!("function {}()", f.name),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Error handler functions detected".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Python error handling detection
// ---------------------------------------------------------------------------

/// Known Python built-in exception names.
const PYTHON_BUILTIN_EXCEPTIONS: &[&str] = &[
    "Exception",
    "BaseException",
    "ValueError",
    "TypeError",
    "KeyError",
    "IndexError",
    "AttributeError",
    "IOError",
    "OSError",
    "RuntimeError",
    "NotImplementedError",
    "StopIteration",
    "ImportError",
    "FileNotFoundError",
    "PermissionError",
    "ConnectionError",
    "TimeoutError",
    "ArithmeticError",
    "LookupError",
    "EnvironmentError",
    "SystemError",
    "UnicodeError",
    "AssertionError",
];

fn detect_python(file: &ProjectFile) -> Vec<ConventionFinding> {
    let _py_ir = match &file.language_ir {
        LanguageIR::Python(ir) => ir,
        _ => return Vec::new(),
    };

    let mut findings = Vec::new();

    // --- Detect custom exception classes ---
    let error_classes: Vec<&TypeDef> = file
        .types
        .iter()
        .filter(|t| {
            t.kind == TypeDefKind::Class
                && (has_error_in_name(&t.name) || has_exception_in_name(&t.name))
        })
        .collect();

    let custom_exceptions: Vec<&&TypeDef> = error_classes
        .iter()
        .filter(|t| !is_python_builtin_exception(&t.name))
        .collect();

    let builtin_usage: Vec<&&TypeDef> = error_classes
        .iter()
        .filter(|t| is_python_builtin_exception(&t.name))
        .collect();

    if !custom_exceptions.is_empty() {
        let evidence: Vec<CodeEvidence> = custom_exceptions
            .iter()
            .take(5)
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
                "Custom exception hierarchy ({} classes): {}",
                custom_exceptions.len(),
                custom_exceptions
                    .iter()
                    .take(5)
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            evidence,
            follows_convention: true,
        });
    }

    // If file only re-uses built-in exceptions via subclassing, note it.
    if !builtin_usage.is_empty() && custom_exceptions.is_empty() {
        let evidence: Vec<CodeEvidence> = builtin_usage
            .iter()
            .take(3)
            .map(|t| CodeEvidence {
                line: t.line,
                end_line: t.line,
                snippet: format!("class {}", t.name),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Uses built-in exception types only".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect error wrapping patterns via imports ---
    // Python doesn't have a standard error wrapping library, but some projects
    // use `contextlib` or custom wrapping. We detect wrapping-related imports.
    let has_contextlib = file.imports.iter().any(|imp| imp.module == "contextlib");

    if has_contextlib {
        let evidence: Vec<CodeEvidence> = file
            .imports
            .iter()
            .filter(|imp| imp.module == "contextlib")
            .take(2)
            .map(|imp| CodeEvidence {
                line: imp.line,
                end_line: imp.line,
                snippet: format!("import {}", imp.module),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "contextlib used for error context management".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    // --- Detect error-related functions ---
    let error_fns: Vec<_> = file
        .functions
        .iter()
        .filter(|f| {
            let lower = f.name.to_lowercase();
            lower.contains("error") || lower.contains("exception") || lower.starts_with("handle_")
        })
        .collect();

    if !error_fns.is_empty() {
        let evidence: Vec<CodeEvidence> = error_fns
            .iter()
            .take(3)
            .map(|f| CodeEvidence {
                line: f.line,
                end_line: f.end_line,
                snippet: format!("def {}()", f.name),
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Error handling utility functions".to_owned(),
            evidence,
            follows_convention: true,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Check whether a type name indicates an error type.
fn has_error_in_name(name: &str) -> bool {
    name.contains("Error") || name.contains("error")
}

/// Check whether a type name indicates an exception type (Python-style).
fn has_exception_in_name(name: &str) -> bool {
    name.contains("Exception") || name.contains("exception")
}

/// Check whether an exception name is a Python built-in.
fn is_python_builtin_exception(name: &str) -> bool {
    PYTHON_BUILTIN_EXCEPTIONS.contains(&name)
}

/// Collect types whose name suggests an error type from the given file,
/// filtered to the specified [`TypeDefKind`].
fn collect_error_types(file: &ProjectFile, kind: TypeDefKind) -> Vec<&TypeDef> {
    file.types
        .iter()
        .filter(|t| t.kind == kind && has_error_in_name(&t.name))
        .collect()
}

/// Format a short summary of error class names for finding descriptions.
fn error_class_names(types: &[&TypeDef]) -> String {
    types
        .iter()
        .take(5)
        .map(|t| t.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::{
        DeriveUsage, Function, Import, JavaScriptIR, PythonIR, RustIR, TraitImpl, TypeScriptIR,
    };
    use std::path::PathBuf;

    // --- Test helpers ---

    fn make_rust_file(imports: Vec<Import>, types: Vec<TypeDef>, rust_ir: RustIR) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/error.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions: Vec::new(),
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(rust_ir),
        }
    }

    fn make_ts_file(
        imports: Vec<Import>,
        types: Vec<TypeDef>,
        functions: Vec<Function>,
    ) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/errors.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
        }
    }

    fn make_js_file(
        imports: Vec<Import>,
        types: Vec<TypeDef>,
        functions: Vec<Function>,
    ) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/errors.js"),
            language: Language::JavaScript,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR::default()),
        }
    }

    fn make_py_file(
        imports: Vec<Import>,
        types: Vec<TypeDef>,
        functions: Vec<Function>,
    ) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/errors.py"),
            language: Language::Python,
            content_hash: String::new(),
            imports,
            exports: Vec::new(),
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
        }
    }

    fn imp(module: &str, names: &[&str], line: usize) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| (*s).to_owned()).collect(),
            is_type_only: false,
            line,
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

    fn derive(type_name: &str, derives: &[&str], line: usize) -> DeriveUsage {
        DeriveUsage {
            type_name: type_name.to_owned(),
            derives: derives.iter().map(|s| (*s).to_owned()).collect(),
            line,
        }
    }

    fn trait_impl(trait_name: &str, type_name: &str, line: usize) -> TraitImpl {
        TraitImpl {
            trait_name: trait_name.to_owned(),
            type_name: type_name.to_owned(),
            line,
        }
    }

    // --- General tests ---

    #[test]
    fn detector_name() {
        let detector = ErrorHandlingDetector;
        assert_eq!(detector.name(), "error_handling");
    }

    #[test]
    fn supports_all_languages() {
        let detector = ErrorHandlingDetector;
        assert_eq!(detector.supported_languages(), Language::all());
    }

    #[test]
    fn empty_rust_file_produces_no_findings() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(Vec::new(), Vec::new(), RustIR::default());
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    #[test]
    fn empty_ts_file_produces_no_findings() {
        let detector = ErrorHandlingDetector;
        let file = make_ts_file(Vec::new(), Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    #[test]
    fn empty_js_file_produces_no_findings() {
        let detector = ErrorHandlingDetector;
        let file = make_js_file(Vec::new(), Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    #[test]
    fn empty_py_file_produces_no_findings() {
        let detector = ErrorHandlingDetector;
        let file = make_py_file(Vec::new(), Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    // --- Rust tests ---

    #[test]
    fn rust_detects_thiserror() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("thiserror", &["Error"], 1)],
            vec![typedef("DatabaseError", TypeDefKind::Enum, 5)],
            RustIR {
                error_types: vec!["DatabaseError".to_owned()],
                derive_macros: vec![derive("DatabaseError", &["Debug", "Error"], 4)],
                ..RustIR::default()
            },
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention && f.description.contains("thiserror")
            })
            .expect("should detect thiserror");
        assert!(convention.follows_convention);
        assert!(!convention.evidence.is_empty());
    }

    #[test]
    fn rust_detects_anyhow() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("anyhow", &["Result", "Context"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("anyhow"))
            .expect("should detect anyhow");
        assert!(convention.follows_convention);
    }

    #[test]
    fn rust_detects_custom_error_enums() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            Vec::new(),
            vec![typedef("AppError", TypeDefKind::Enum, 5)],
            RustIR {
                error_types: vec!["AppError".to_owned()],
                ..RustIR::default()
            },
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("custom"))
            .expect("should detect custom error enums");
        assert!(convention.follows_convention);
        assert!(
            convention
                .evidence
                .iter()
                .any(|e| e.snippet.contains("AppError")),
            "evidence should mention AppError"
        );
    }

    #[test]
    fn rust_flags_thiserror_and_anyhow_conflict() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![
                imp("thiserror", &["Error"], 1),
                imp("anyhow", &["Result"], 2),
            ],
            Vec::new(),
            RustIR {
                derive_macros: vec![derive("MyError", &["Debug", "Error"], 5)],
                error_types: vec!["MyError".to_owned()],
                ..RustIR::default()
            },
        );
        let findings = detector.detect(&file);

        let observation = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Observation
                    && f.description.contains("thiserror")
                    && f.description.contains("anyhow")
            })
            .expect("should flag thiserror + anyhow conflict");
        assert!(!observation.follows_convention);
    }

    #[test]
    fn rust_detects_context_wrapping() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("anyhow", &["Context", "Result"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let context = findings
            .iter()
            .find(|f| f.description.contains("Context"))
            .expect("should detect anyhow::Context wrapping");
        assert!(context.follows_convention);
    }

    #[test]
    fn rust_detects_manual_error_trait_impl() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            Vec::new(),
            vec![typedef("MyError", TypeDefKind::Struct, 5)],
            RustIR {
                trait_implementations: vec![
                    trait_impl("std::error::Error", "MyError", 10),
                    trait_impl("Display", "MyError", 20),
                ],
                error_types: vec!["MyError".to_owned()],
                ..RustIR::default()
            },
        );
        let findings = detector.detect(&file);

        let manual = findings
            .iter()
            .find(|f| f.description.contains("Manual std::error::Error"))
            .expect("should detect manual Error impl");
        assert!(manual.follows_convention);
    }

    #[test]
    fn rust_detects_from_impls_for_error_types() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            Vec::new(),
            vec![typedef("AppError", TypeDefKind::Enum, 5)],
            RustIR {
                error_types: vec!["AppError".to_owned()],
                trait_implementations: vec![
                    trait_impl("From<std::io::Error>", "AppError", 10),
                    trait_impl("From<DatabaseError>", "AppError", 20),
                ],
                ..RustIR::default()
            },
        );
        let findings = detector.detect(&file);

        let from_finding = findings
            .iter()
            .find(|f| f.description.contains("From impls"))
            .expect("should detect From conversions");
        assert!(from_finding.description.contains('2'));
        assert!(from_finding.follows_convention);
    }

    // --- TypeScript tests ---

    #[test]
    fn ts_detects_custom_error_classes() {
        let detector = ErrorHandlingDetector;
        let file = make_ts_file(
            Vec::new(),
            vec![
                typedef("BaseError", TypeDefKind::Class, 5),
                typedef("NotFoundError", TypeDefKind::Class, 29),
                typedef("ValidationError", TypeDefKind::Class, 35),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention
                    && f.description.contains("Custom error classes")
            })
            .expect("should detect custom error classes");
        assert!(convention.description.contains("3 found"));
        assert!(convention.follows_convention);
    }

    #[test]
    fn ts_detects_result_either_pattern() {
        let detector = ErrorHandlingDetector;
        let file = make_ts_file(
            vec![imp("neverthrow", &["Result", "ok", "err"], 1)],
            Vec::new(),
            Vec::new(),
        );
        let findings = detector.detect(&file);

        let result = findings
            .iter()
            .find(|f| f.description.contains("Result/Either"))
            .expect("should detect Result/Either pattern");
        assert!(result.follows_convention);
    }

    #[test]
    fn ts_detects_type_guard_functions() {
        let detector = ErrorHandlingDetector;
        let file = make_ts_file(
            Vec::new(),
            Vec::new(),
            vec![func("isBaseError", 66), func("isNotFoundError", 70)],
        );
        let findings = detector.detect(&file);

        let guard = findings
            .iter()
            .find(|f| f.description.contains("type guard"))
            .expect("should detect type guard functions");
        assert!(guard.evidence.len() == 2);
    }

    #[test]
    fn ts_no_findings_for_non_error_classes() {
        let detector = ErrorHandlingDetector;
        let file = make_ts_file(
            Vec::new(),
            vec![
                typedef("UserService", TypeDefKind::Class, 1),
                typedef("Logger", TypeDefKind::Class, 20),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    // --- JavaScript tests ---

    #[test]
    fn js_detects_custom_error_classes() {
        let detector = ErrorHandlingDetector;
        let file = make_js_file(
            Vec::new(),
            vec![
                typedef("AppError", TypeDefKind::Class, 5),
                typedef("HttpError", TypeDefKind::Class, 15),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention
                    && f.description.contains("Custom error classes")
            })
            .expect("should detect custom error classes in JS");
        assert!(convention.description.contains("2 found"));
    }

    #[test]
    fn js_detects_error_handler_functions() {
        let detector = ErrorHandlingDetector;
        let file = make_js_file(
            Vec::new(),
            Vec::new(),
            vec![func("handleError", 5), func("onError", 20)],
        );
        let findings = detector.detect(&file);

        let handler = findings
            .iter()
            .find(|f| f.description.contains("Error handler functions"))
            .expect("should detect error handler functions");
        assert!(handler.evidence.len() == 2);
    }

    // --- Python tests ---

    #[test]
    fn py_detects_custom_exception_hierarchy() {
        let detector = ErrorHandlingDetector;
        let file = make_py_file(
            Vec::new(),
            vec![
                typedef("AppError", TypeDefKind::Class, 5),
                typedef("ValidationError", TypeDefKind::Class, 15),
                typedef("NotFoundError", TypeDefKind::Class, 25),
            ],
            Vec::new(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention
                    && f.description.contains("Custom exception")
            })
            .expect("should detect custom exception hierarchy");
        assert!(convention.description.contains("3 classes"));
        assert!(convention.follows_convention);
    }

    #[test]
    fn py_detects_builtin_exception_only() {
        let detector = ErrorHandlingDetector;
        let file = make_py_file(
            Vec::new(),
            vec![typedef("ValueError", TypeDefKind::Class, 5)],
            Vec::new(),
        );
        let findings = detector.detect(&file);

        // ValueError is a built-in, but it has "Error" in the name, and it IS a
        // built-in, so we should get the "built-in exception" observation only.
        let obs = findings
            .iter()
            .find(|f| f.description.contains("built-in exception"))
            .expect("should note built-in exception usage");
        assert!(obs.nature == KnowledgeNature::Observation);
    }

    #[test]
    fn py_detects_error_functions() {
        let detector = ErrorHandlingDetector;
        let file = make_py_file(
            Vec::new(),
            Vec::new(),
            vec![func("handle_error", 5), func("format_exception", 20)],
        );
        let findings = detector.detect(&file);

        let handler = findings
            .iter()
            .find(|f| f.description.contains("Error handling utility"))
            .expect("should detect error handling functions");
        assert!(handler.evidence.len() == 2);
    }

    #[test]
    fn py_detects_contextlib_usage() {
        let detector = ErrorHandlingDetector;
        let file = make_py_file(
            vec![imp("contextlib", &["suppress", "contextmanager"], 1)],
            Vec::new(),
            Vec::new(),
        );
        let findings = detector.detect(&file);

        let ctx = findings
            .iter()
            .find(|f| f.description.contains("contextlib"))
            .expect("should detect contextlib usage");
        assert!(ctx.nature == KnowledgeNature::Observation);
    }

    #[test]
    fn py_empty_file_produces_no_findings() {
        let detector = ErrorHandlingDetector;
        let file = make_py_file(Vec::new(), Vec::new(), Vec::new());
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
    }

    #[test]
    fn rust_thiserror_preferred_over_anyhow_detection() {
        // When both thiserror import and error_types exist,
        // thiserror should be the detected library (not custom).
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("thiserror", &["Error"], 1)],
            vec![typedef("MyError", TypeDefKind::Enum, 5)],
            RustIR {
                error_types: vec!["MyError".to_owned()],
                derive_macros: vec![derive("MyError", &["Debug", "Error"], 4)],
                ..RustIR::default()
            },
        );
        let findings = detector.detect(&file);

        let conventions: Vec<_> = findings
            .iter()
            .filter(|f| {
                f.nature == KnowledgeNature::Convention
                    && f.description.contains("Rust error handling")
            })
            .collect();
        assert_eq!(conventions.len(), 1);
        assert!(conventions[0].description.contains("thiserror"));
    }
}
