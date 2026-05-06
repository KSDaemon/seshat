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
    AnchorKind, CodeEvidence, ConventionFinding, FindingKind, KnowledgeNature, Language,
    LanguageIR, ProjectFile, TypeDef, TypeDefKind,
};

use crate::trait_def::ConventionDetector;
use crate::usage_evidence::find_usage_evidence_for_file_scoped;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DETECTOR_NAME: &str = "error_handling";

/// Maximum number of evidence entries per finding.
const MAX_EVIDENCE: usize = 5;

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
    Eyre,
    ColorEyre,
    Miette,
    Snafu,
    ErrorStack,
    Displaydoc,
    Custom,
}

impl RustErrorLib {
    fn as_str(self) -> &'static str {
        match self {
            Self::Thiserror => "thiserror",
            Self::Anyhow => "anyhow",
            Self::Eyre => "eyre",
            Self::ColorEyre => "color-eyre",
            Self::Miette => "miette",
            Self::Snafu => "snafu",
            Self::ErrorStack => "error-stack",
            Self::Displaydoc => "displaydoc",
            Self::Custom => "custom error enums",
        }
    }
}

/// Classify a Rust crate as a known error handling library from its import path.
fn classify_rust_error_lib(module: &str) -> Option<RustErrorLib> {
    // Extract the root crate name from the import path (e.g. "anyhow::Result" → "anyhow").
    let root = module.split("::").next().unwrap_or(module);
    match root {
        "thiserror" => Some(RustErrorLib::Thiserror),
        "anyhow" => Some(RustErrorLib::Anyhow),
        "eyre" => Some(RustErrorLib::Eyre),
        "color_eyre" => Some(RustErrorLib::ColorEyre),
        "miette" => Some(RustErrorLib::Miette),
        "snafu" => Some(RustErrorLib::Snafu),
        "error_stack" => Some(RustErrorLib::ErrorStack),
        "displaydoc" => Some(RustErrorLib::Displaydoc),
        _ => None,
    }
}

fn detect_rust(file: &ProjectFile) -> Vec<ConventionFinding> {
    let rust_ir = match &file.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => return Vec::new(),
    };

    let mut findings = Vec::new();

    // --- Detect known error libraries via imports ---
    let known_libs: Vec<RustErrorLib> = file
        .imports
        .iter()
        .filter_map(|imp| classify_rust_error_lib(&imp.module))
        .collect();

    let has_thiserror = known_libs.contains(&RustErrorLib::Thiserror);
    let has_anyhow = known_libs.contains(&RustErrorLib::Anyhow);

    // derive(Error) on an error-named type WITH thiserror import → thiserror convention.
    let has_thiserror_derive = has_thiserror
        && rust_ir
            .derive_macros
            .iter()
            .any(|d| d.derives.iter().any(|name| name == "Error") && d.type_name.contains("Error"));

    let has_error_types = !rust_ir.error_types.is_empty();

    // Determine the dominant approach (known libraries first).
    let lib = if has_thiserror || has_thiserror_derive {
        Some(RustErrorLib::Thiserror)
    } else if has_anyhow {
        Some(RustErrorLib::Anyhow)
    } else if let Some(&first_known) = known_libs.first() {
        // Other known error lib (eyre, miette, snafu, etc.)
        Some(first_known)
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
            kind: FindingKind::Other,
        });
    }

    // --- Heuristic: unknown crate with derive(Error) or impl Error ---
    // If no known error lib was detected but we see derive(Error) or
    // impl std::error::Error, flag as a heuristic error handling finding.
    if lib.is_none() {
        let has_derive_error = rust_ir
            .derive_macros
            .iter()
            .any(|d| d.derives.iter().any(|name| name == "Error"));
        let has_error_impl = rust_ir
            .trait_implementations
            .iter()
            .any(|ti| ti.trait_name == "Error" || ti.trait_name == "std::error::Error");

        if has_derive_error || has_error_impl {
            let mut evidence = Vec::new();
            for d in &rust_ir.derive_macros {
                if d.derives.iter().any(|name| name == "Error") {
                    evidence.push(CodeEvidence {
                        file: file.path.clone(),
                        line: d.line,
                        end_line: d.line,
                        snippet: String::new(),
                        snippet_start_line: 0,
                        anchor: AnchorKind::CallSite,
                    });
                }
            }
            for ti in &rust_ir.trait_implementations {
                if ti.trait_name == "Error" || ti.trait_name == "std::error::Error" {
                    evidence.push(CodeEvidence {
                        file: file.path.clone(),
                        line: ti.line,
                        end_line: ti.line,
                        snippet: String::new(),
                        snippet_start_line: 0,
                        anchor: AnchorKind::CallSite,
                    });
                }
            }
            evidence.truncate(MAX_EVIDENCE);

            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Observation,
                description: "Rust error handling: unknown library with Error derive/impl"
                    .to_owned(),
                evidence,
                follows_convention: true,
                kind: FindingKind::Other,
            });
        }
    }

    // --- Detect conflicting libraries ---
    let distinct_libs: Vec<RustErrorLib> = {
        let mut seen = Vec::new();
        for &l in &known_libs {
            if !seen.contains(&l) {
                seen.push(l);
            }
        }
        seen
    };

    if distinct_libs.len() > 1 {
        let mut evidence = Vec::new();
        for imp in &file.imports {
            if classify_rust_error_lib(&imp.module).is_some() {
                evidence.push(CodeEvidence {
                    file: file.path.clone(),
                    line: imp.line,
                    end_line: imp.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                });
            }
        }
        evidence.truncate(MAX_EVIDENCE);

        let mut lib_names: Vec<&str> = distinct_libs.iter().map(|l| l.as_str()).collect();
        lib_names.sort();
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: format!(
                "Multiple error handling libraries in same file: {}",
                lib_names.join(", ")
            ),
            evidence,
            follows_convention: false,
            kind: FindingKind::Other,
        });
    }

    // --- Detect error wrapping patterns (context/map_err) ---
    // We detect these via anyhow/eyre Context import.
    let context_sources: Vec<&str> = file
        .imports
        .iter()
        .filter(|imp| {
            (imp.module.starts_with("anyhow") || imp.module.starts_with("eyre"))
                && imp.names.iter().any(|n| n == "Context" || n == "WrapErr")
        })
        .map(|imp| imp.module.as_str())
        .collect();

    if !context_sources.is_empty() {
        let evidence: Vec<CodeEvidence> = file
            .imports
            .iter()
            .filter(|imp| {
                (imp.module.starts_with("anyhow") || imp.module.starts_with("eyre"))
                    && imp.names.iter().any(|n| n == "Context" || n == "WrapErr")
            })
            .take(3)
            .map(|imp| CodeEvidence {
                file: file.path.clone(),
                line: imp.line,
                end_line: imp.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        let source_lib = context_sources[0].split("::").next().unwrap_or("unknown");
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: format!("Error wrapping via {source_lib}::Context"),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
                file: file.path.clone(),
                line: ti.line,
                end_line: ti.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
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
                kind: FindingKind::Other,
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
                file: file.path.clone(),
                line: ti.line,
                end_line: ti.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Error type conversion via From impls (Rust)".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
        });
    }

    findings
}

/// Build evidence entries for the detected Rust error library.
///
/// Prefers call-site evidence (actual error construction sites like `Err(...)`,
/// `DatabaseError::new(...)`) over import-line evidence when available.
fn build_rust_error_evidence(
    file: &ProjectFile,
    rust_ir: &seshat_core::RustIR,
    lib: RustErrorLib,
) -> Vec<CodeEvidence> {
    // Prefer call-site evidence — shows where errors are constructed/propagated.
    let error_modules: Vec<&str> = file
        .imports
        .iter()
        .filter(|imp| {
            let root = imp.module.split("::").next().unwrap_or(&imp.module);
            matches!(
                root,
                "thiserror"
                    | "anyhow"
                    | "eyre"
                    | "color_eyre"
                    | "miette"
                    | "snafu"
                    | "error_stack"
                    | "displaydoc"
            ) || imp.names.iter().any(|n| n.contains("Error"))
        })
        .map(|imp| imp.module.split("::").next().unwrap_or(&imp.module))
        .collect();
    let call_sites = find_usage_evidence_for_file_scoped(file, &error_modules, MAX_EVIDENCE);
    if !call_sites.is_empty() {
        return call_sites;
    }

    // Fallback: import/derive evidence when no matching call sites found.
    let mut evidence = Vec::new();

    match lib {
        RustErrorLib::Thiserror => {
            // Show thiserror import.
            for imp in &file.imports {
                if classify_rust_error_lib(&imp.module) == Some(RustErrorLib::Thiserror) {
                    evidence.push(CodeEvidence {
                        file: file.path.clone(),
                        line: imp.line,
                        end_line: imp.line,
                        snippet: String::new(),
                        snippet_start_line: 0,
                        anchor: AnchorKind::CallSite,
                    });
                }
            }
            // Show derive(Error) usage.
            for d in &rust_ir.derive_macros {
                if d.derives.iter().any(|name| name == "Error") {
                    evidence.push(CodeEvidence {
                        file: file.path.clone(),
                        line: d.line,
                        end_line: d.line,
                        snippet: String::new(),
                        snippet_start_line: 0,
                        anchor: AnchorKind::CallSite,
                    });
                }
            }
        }
        RustErrorLib::Custom => {
            // Show the error type names from RustIR.
            for (i, error_type) in rust_ir.error_types.iter().enumerate() {
                if i >= MAX_EVIDENCE {
                    break;
                }
                // Find the TypeDef line if available.
                let line = file
                    .types
                    .iter()
                    .find(|t| &t.name == error_type)
                    .map_or(0, |t| t.line);
                evidence.push(CodeEvidence {
                    file: file.path.clone(),
                    line,
                    end_line: line,
                    snippet: String::new(),
                    snippet_start_line: 0, // detect_with_source will fill real source,
                    anchor: AnchorKind::CallSite,
                });
            }
        }
        // All other known libraries: show their imports as evidence.
        _ => {
            for imp in &file.imports {
                if classify_rust_error_lib(&imp.module) == Some(lib) {
                    evidence.push(CodeEvidence {
                        file: file.path.clone(),
                        line: imp.line,
                        end_line: imp.line,
                        snippet: String::new(),
                        snippet_start_line: 0,
                        anchor: AnchorKind::CallSite,
                    });
                }
            }
        }
    }

    // Cap evidence.
    evidence.truncate(MAX_EVIDENCE);
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
        // Prefer call-site evidence (error throwing / construction) over type definition lines.
        let error_modules: Vec<&str> =
            file.imports
                .iter()
                .filter(|imp| {
                    imp.names.iter().any(|n| {
                        n.contains("Error") || n.contains("Err") || n == "Result" || n == "Ok"
                    }) || imp.module.contains("neverthrow")
                        || imp.module.contains("fp-ts")
                        || imp.module.contains("either")
                })
                .map(|imp| imp.module.as_str())
                .collect();
        let call_sites = find_usage_evidence_for_file_scoped(file, &error_modules, MAX_EVIDENCE);
        let evidence: Vec<CodeEvidence> = if !call_sites.is_empty() {
            call_sites
        } else {
            error_classes
                .iter()
                .take(5)
                .map(|t| CodeEvidence {
                    file: file.path.clone(),
                    line: t.line,
                    end_line: t.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect()
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Custom error classes (TypeScript)".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
                    file: file.path.clone(),
                    line: t.line,
                    end_line: t.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                });
            }
        }
        for imp in &file.imports {
            if imp.module.contains("neverthrow")
                || imp.module.contains("fp-ts")
                || imp.module.contains("either")
            {
                evidence.push(CodeEvidence {
                    file: file.path.clone(),
                    line: imp.line,
                    end_line: imp.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
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
            kind: FindingKind::Other,
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
            nature: KnowledgeNature::Observation,
            description: "Error type guard functions detected".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
        // Prefer call-site evidence (error throwing / construction) over type definition lines.
        let error_modules: Vec<&str> =
            file.imports
                .iter()
                .filter(|imp| {
                    imp.names.iter().any(|n| {
                        n.contains("Error") || n.contains("Err") || n == "Result" || n == "Ok"
                    }) || imp.module.contains("neverthrow")
                        || imp.module.contains("fp-ts")
                        || imp.module.contains("either")
                })
                .map(|imp| imp.module.as_str())
                .collect();
        let call_sites = find_usage_evidence_for_file_scoped(file, &error_modules, MAX_EVIDENCE);
        let evidence: Vec<CodeEvidence> = if !call_sites.is_empty() {
            call_sites
        } else {
            error_classes
                .iter()
                .take(5)
                .map(|t| CodeEvidence {
                    file: file.path.clone(),
                    line: t.line,
                    end_line: t.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect()
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Custom error classes (JavaScript)".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
                file: file.path.clone(),
                line: imp.line,
                end_line: imp.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Promise library for error handling".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
            nature: KnowledgeNature::Observation,
            description: "Error handler functions detected".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
        // Prefer call-site evidence (error raising / construction) over type definition lines.
        let error_modules: Vec<&str> = file
            .imports
            .iter()
            .filter(|imp| imp.module.contains("exception") || imp.module.contains("error"))
            .map(|imp| imp.module.as_str())
            .collect();
        let call_sites = find_usage_evidence_for_file_scoped(file, &error_modules, MAX_EVIDENCE);
        let evidence: Vec<CodeEvidence> = if !call_sites.is_empty() {
            call_sites
        } else {
            custom_exceptions
                .iter()
                .take(5)
                .map(|t| CodeEvidence {
                    file: file.path.clone(),
                    line: t.line,
                    end_line: t.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect()
        };

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Custom exception hierarchy (Python)".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
        });
    }

    // If file only re-uses built-in exceptions via subclassing, note it.
    if !builtin_usage.is_empty() && custom_exceptions.is_empty() {
        let evidence: Vec<CodeEvidence> = builtin_usage
            .iter()
            .take(3)
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
            nature: KnowledgeNature::Observation,
            description: "Uses built-in exception types only".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
                file: file.path.clone(),
                line: imp.line,
                end_line: imp.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "contextlib used for error context management".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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
            nature: KnowledgeNature::Observation,
            description: "Error handling utility functions".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Other,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::{
        DeriveUsage, Function, FunctionCall, Import, JavaScriptIR, MacroCall, PythonIR, RustIR,
        TraitImpl, TypeScriptIR,
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
            file_doc: None,
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
            file_doc: None,
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
            file_doc: None,
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
            file_doc: None,
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
            doc_comment: None,
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
            doc_comment: None,
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
            !convention.evidence.is_empty(),
            "evidence should be non-empty for custom error enums"
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
                    && f.description.contains("Multiple error handling libraries")
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
        assert!(from_finding.description.contains("Rust"));
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
        assert!(convention.description.contains("TypeScript"));
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
        assert!(convention.description.contains("JavaScript"));
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
        assert!(convention.description.contains("Python"));
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

    // --- New known error handling libraries ---

    #[test]
    fn rust_detects_eyre() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("eyre", &["Result", "Report"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("eyre"))
            .expect("should detect eyre");
        assert!(convention.follows_convention);
    }

    #[test]
    fn rust_detects_color_eyre() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("color_eyre", &["eyre", "Result"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention && f.description.contains("color-eyre")
            })
            .expect("should detect color-eyre");
        assert!(convention.follows_convention);
    }

    #[test]
    fn rust_detects_miette() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("miette", &["Diagnostic", "Report"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("miette"))
            .expect("should detect miette");
        assert!(convention.follows_convention);
    }

    #[test]
    fn rust_detects_snafu() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("snafu", &["Snafu", "ResultExt"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| f.nature == KnowledgeNature::Convention && f.description.contains("snafu"))
            .expect("should detect snafu");
        assert!(convention.follows_convention);
    }

    #[test]
    fn rust_detects_error_stack() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("error_stack", &["Report", "ResultExt"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention && f.description.contains("error-stack")
            })
            .expect("should detect error-stack");
        assert!(convention.follows_convention);
    }

    #[test]
    fn rust_detects_displaydoc() {
        let detector = ErrorHandlingDetector;
        let file = make_rust_file(
            vec![imp("displaydoc", &["Display"], 1)],
            Vec::new(),
            RustIR::default(),
        );
        let findings = detector.detect(&file);

        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention && f.description.contains("displaydoc")
            })
            .expect("should detect displaydoc");
        assert!(convention.follows_convention);
    }

    // --- Heuristic: derive(Error) without known library ---

    #[test]
    fn detect_with_source_sets_real_snippet() {
        let detector = ErrorHandlingDetector;
        // Build a Rust file with a thiserror import at line 1 and a derive(Error) at line 3.
        let file = make_rust_file(
            vec![imp("thiserror", &["Error"], 1)],
            vec![typedef("MyError", TypeDefKind::Enum, 3)],
            RustIR {
                error_types: vec!["MyError".to_owned()],
                derive_macros: vec![derive("MyError", &["Debug", "Error"], 3)],
                ..RustIR::default()
            },
        );
        // Source with real content at lines 1 and 3.
        let source = "use thiserror::Error;\n\n#[derive(Debug, Error)]\npub enum MyError {\n    #[error(\"oops\")]\n    Bad,\n}\n";

        let findings = detector.detect_with_source(&file, source);

        assert!(!findings.is_empty(), "should have at least one finding");
        let finding = findings
            .iter()
            .find(|f| f.description.contains("thiserror"))
            .expect("should have thiserror finding");
        assert!(!finding.evidence.is_empty(), "finding should have evidence");
        let ev = &finding.evidence[0];
        assert_eq!(ev.file, file.path);
        // Snippet must contain the actual source line — "use thiserror::Error;"
        // is at line 1, so the snippet must include that text.
        assert!(
            ev.snippet.contains("thiserror"),
            "snippet must contain real source keyword 'thiserror', got: {:?}",
            ev.snippet
        );
        assert!(
            !ev.snippet.starts_with("Custom "),
            "snippet must not be a synthetic format string, got: {:?}",
            ev.snippet
        );
    }

    #[test]
    fn rust_heuristic_derive_error_without_known_lib() {
        let detector = ErrorHandlingDetector;
        // Unknown crate but has derive(Error)
        let file = make_rust_file(
            vec![imp("my_custom_error_lib", &["ErrorDerive"], 1)],
            Vec::new(),
            RustIR {
                derive_macros: vec![derive("AppError", &["Debug", "Error"], 5)],
                ..RustIR::default()
            },
        );
        let findings = detector.detect(&file);

        let heuristic = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Observation
                    && f.description
                        .contains("unknown library with Error derive/impl")
            })
            .expect("should detect heuristic derive(Error)");
        assert!(heuristic.follows_convention);
    }

    // --- Call-site evidence tests ---

    #[test]
    fn rust_thiserror_shows_err_construction_call_site() {
        // Rust file with thiserror import AND function calls that construct errors.
        // The detector should prefer call-site evidence over import/derive lines.
        let detector = ErrorHandlingDetector;
        let mut file = make_rust_file(
            vec![
                imp("thiserror", &["Error"], 1),
                // DatabaseError is also imported so its construction sites can be matched.
                imp("crate::errors", &["DatabaseError"], 2),
            ],
            vec![typedef("DatabaseError", TypeDefKind::Enum, 5)],
            RustIR {
                error_types: vec!["DatabaseError".to_owned()],
                derive_macros: vec![derive("DatabaseError", &["Debug", "Error"], 4)],
                ..RustIR::default()
            },
        );

        // Add function calls that construct the error — e.g. Err(DatabaseError::NotFound).
        // The callee "DatabaseError::NotFound" matches because "DatabaseError" is in the
        // import names above (Strategy B of matches_import).
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.function_calls = vec![
                FunctionCall {
                    callee: "DatabaseError::NotFound".to_owned(),
                    line: 25,
                    end_line: 25,
                    snippet: "Err(DatabaseError::NotFound)".to_owned(),
                },
                FunctionCall {
                    callee: "DatabaseError::ConnectionFailed".to_owned(),
                    line: 40,
                    end_line: 40,
                    snippet: "Err(DatabaseError::ConnectionFailed(e))".to_owned(),
                },
            ];
        }

        let findings = detector.detect(&file);
        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention && f.description.contains("thiserror")
            })
            .expect("should detect thiserror convention");

        assert!(
            !convention.evidence.is_empty(),
            "convention finding should have evidence"
        );
        let evidence_lines: Vec<usize> = convention.evidence.iter().map(|e| e.line).collect();
        // Evidence should point at call sites (lines 25, 40), not import line (1) or derive (4).
        assert!(
            evidence_lines.iter().any(|&l| l >= 25),
            "evidence should include call-site lines (>= 25), got: {:?}",
            evidence_lines
        );
        assert!(
            !evidence_lines.contains(&1),
            "evidence should NOT be the import line (1), got: {:?}",
            evidence_lines
        );
        // Snippet should contain the actual error construction code.
        let first_ev = &convention.evidence[0];
        assert!(
            first_ev.snippet.contains("DatabaseError"),
            "snippet should contain error construction, got: {:?}",
            first_ev.snippet
        );
    }

    // -----------------------------------------------------------------------
    // BUG: unscoped call_sites contaminate error handling findings
    // -----------------------------------------------------------------------

    #[test]
    fn unscoped_call_sites_contaminate_rust_error_handling() {
        // Rust file with thiserror (errors) AND tracing (logging) imports.
        // The "Rust error handling: thiserror" finding should only have
        // thiserror-related evidence, not tracing macro calls.
        let detector = ErrorHandlingDetector;
        let mut file = make_rust_file(
            vec![
                imp("thiserror", &["Error"], 1),
                imp("crate::errors", &["DatabaseError"], 2),
                imp("tracing", &["info", "error"], 3),
            ],
            vec![typedef("DatabaseError", TypeDefKind::Enum, 5)],
            RustIR {
                error_types: vec!["DatabaseError".to_owned()],
                derive_macros: vec![derive("DatabaseError", &["Debug", "Error"], 4)],
                ..RustIR::default()
            },
        );
        // File has both error construction calls AND logging macro calls.
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.function_calls = vec![FunctionCall {
                callee: "DatabaseError::NotFound".to_owned(),
                line: 25,
                end_line: 25,
                snippet: "Err(DatabaseError::NotFound)".to_owned(),
            }];
            ir.macro_calls = vec![
                MacroCall {
                    name: "info".to_owned(),
                    line: 40,
                },
                MacroCall {
                    name: "error".to_owned(),
                    line: 50,
                },
            ];
        }

        let findings = detector.detect(&file);
        let convention = findings
            .iter()
            .find(|f| {
                f.nature == KnowledgeNature::Convention && f.description.contains("thiserror")
            })
            .expect("should detect thiserror convention");

        // After fix: error handling finding should only have DatabaseError evidence (line 25),
        // tracing macros (lines 40, 50) should NOT appear.
        let evidence_lines: Vec<usize> = convention.evidence.iter().map(|e| e.line).collect();
        assert!(
            !evidence_lines.contains(&40) && !evidence_lines.contains(&50),
            "error handling finding should NOT contain tracing call sites, got: {:?}",
            evidence_lines
        );
        assert!(
            evidence_lines.contains(&25),
            "error finding should contain DatabaseError call site (line 25), got: {:?}",
            evidence_lines
        );
    }
}
