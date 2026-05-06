//! Export patterns detector — default vs named, barrel exports, pub/mod.
//!
//! Analyses [`ProjectFile::exports`] and language-specific IR fields to detect
//! export conventions across all four supported languages:
//!
//! - **TypeScript/JavaScript**: default vs named export preference with adoption
//!   rate; barrel export pattern detection via `TypeScriptIR::has_barrel_exports`
//!   or file path heuristics.
//! - **Rust**: `pub` usage patterns, `mod` re-export patterns.
//! - **Python**: `__all__` usage pattern via `PythonIR::has_all_export`.
//!
//! Each finding includes representative [`CodeEvidence`] snippets.

use std::path::Path;

use seshat_core::{
    AnchorKind, CodeEvidence, ConventionFinding, Export, FindingKind, KnowledgeNature, Language,
    LanguageIR, ModuleSystem, ProjectFile,
};

use crate::trait_def::ConventionDetector;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DETECTOR_NAME: &str = "export_patterns";

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Detects export conventions across all four supported languages.
///
/// Produces:
/// - **Convention** findings for the dominant export style.
/// - **Observation** findings for alternative/mixed patterns.
pub struct ExportPatternsDetector;

impl ConventionDetector for ExportPatternsDetector {
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
// Helpers
// ---------------------------------------------------------------------------

/// Build a `CodeEvidence` entry for an export.
fn export_evidence(export: &Export, file_path: &Path) -> CodeEvidence {
    CodeEvidence {
        file: file_path.to_path_buf(),
        line: export.line,
        end_line: export.line,
        snippet: String::new(),
        snippet_start_line: 0,
        anchor: AnchorKind::CallSite,
    }
}

/// Partition exports into default and named (non-type-only).
fn partition_exports(exports: &[Export]) -> (Vec<&Export>, Vec<&Export>) {
    let defaults: Vec<&Export> = exports.iter().filter(|e| e.is_default).collect();
    let named: Vec<&Export> = exports.iter().filter(|e| !e.is_default).collect();
    (defaults, named)
}

/// Check whether a file path looks like a barrel/index file.
fn is_barrel_file_path(path: &Path) -> bool {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    stem == "index" || stem == "mod"
}

// ---------------------------------------------------------------------------
// TypeScript detection
// ---------------------------------------------------------------------------

fn detect_typescript(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    if file.exports.is_empty() {
        return findings;
    }

    let ts_ir = match &file.language_ir {
        LanguageIR::TypeScript(ir) => ir,
        _ => return findings,
    };

    // --- Default vs Named export preference --------------------------------
    let (defaults, named) = partition_exports(&file.exports);
    let default_count = defaults.len();
    let named_count = named.len();
    let total = file.exports.len();

    if total > 0 {
        let evidence: Vec<CodeEvidence> = file
            .exports
            .iter()
            .take(5)
            .map(|e| export_evidence(e, &file.path))
            .collect();

        if default_count > 0 && named_count == 0 {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Uses default exports exclusively (TypeScript)".to_owned(),
                evidence,
                follows_convention: true,
                kind: FindingKind::Export,
            });
        } else if named_count > 0 && default_count == 0 {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Uses named exports exclusively (TypeScript)".to_owned(),
                evidence,
                follows_convention: true,
                kind: FindingKind::Export,
            });
        } else if default_count > 0 && named_count > 0 {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Observation,
                description: "Mixes default and named exports (TypeScript)".to_owned(),
                evidence,
                follows_convention: false,
                kind: FindingKind::Export,
            });
        }
    }

    // --- Barrel exports detection -------------------------------------------
    if ts_ir.has_barrel_exports || is_barrel_file_path(&file.path) {
        let re_export_evidence: Vec<CodeEvidence> = file
            .exports
            .iter()
            .take(5)
            .map(|e| CodeEvidence {
                file: file.path.clone(),
                line: e.line,
                end_line: e.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Uses barrel export pattern (re-exports from index file)".to_owned(),
            evidence: re_export_evidence,
            follows_convention: true,
            kind: FindingKind::Export,
        });
    }

    // --- Type-only exports detection ----------------------------------------
    let type_only_count = file.exports.iter().filter(|e| e.is_type_only).count();
    if type_only_count > 0 {
        let type_evidence: Vec<CodeEvidence> = file
            .exports
            .iter()
            .filter(|e| e.is_type_only)
            .take(5)
            .map(|e| CodeEvidence {
                file: file.path.clone(),
                line: e.line,
                end_line: e.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Uses type-only exports (TypeScript)".to_owned(),
            evidence: type_evidence,
            follows_convention: true,
            kind: FindingKind::Export,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// JavaScript detection
// ---------------------------------------------------------------------------

fn detect_javascript(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    let js_ir = match &file.language_ir {
        LanguageIR::JavaScript(ir) => ir,
        _ => return findings,
    };

    // No exports and no module.exports — nothing to report.
    if file.exports.is_empty() && !js_ir.has_module_exports {
        return findings;
    }

    // --- Default vs Named export preference --------------------------------
    if !file.exports.is_empty() {
        let (defaults, named) = partition_exports(&file.exports);
        let default_count = defaults.len();
        let named_count = named.len();
        let total = file.exports.len();

        let evidence: Vec<CodeEvidence> = file
            .exports
            .iter()
            .take(5)
            .map(|e| export_evidence(e, &file.path))
            .collect();

        if default_count > 0 && named_count == 0 {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Uses default exports exclusively (JavaScript)".to_owned(),
                evidence,
                follows_convention: true,
                kind: FindingKind::Export,
            });
        } else if named_count > 0 && default_count == 0 {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Uses named exports exclusively (JavaScript)".to_owned(),
                evidence,
                follows_convention: true,
                kind: FindingKind::Export,
            });
        } else if default_count > 0 && named_count > 0 {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Observation,
                description: "Mixes default and named exports (JavaScript)".to_owned(),
                evidence,
                follows_convention: false,
                kind: FindingKind::Export,
            });
        }

        // --- Barrel exports detection (path-based) -------------------------
        if is_barrel_file_path(&file.path) && total > 1 {
            let re_export_evidence: Vec<CodeEvidence> = file
                .exports
                .iter()
                .take(5)
                .map(|e| CodeEvidence {
                    file: file.path.clone(),
                    line: e.line,
                    end_line: e.line,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                })
                .collect();

            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Uses barrel export pattern (re-exports from index file)".to_owned(),
                evidence: re_export_evidence,
                follows_convention: true,
                kind: FindingKind::Export,
            });
        }
    }

    // --- module.exports detection (CommonJS) --------------------------------
    if js_ir.has_module_exports {
        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Uses CommonJS module.exports pattern".to_owned(),
            evidence: vec![CodeEvidence {
                file: file.path.clone(),
                line: 0, // file-level signal, no single source line
                end_line: 0,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::FileLevel,
            }],
            follows_convention: true,
            kind: FindingKind::Export,
        });
    }

    // --- Module system detection (ESM / CommonJS / mixed) --------------------
    detect_module_system_finding(file, js_ir, &mut findings);

    findings
}

/// Emit a finding based on the `JavaScriptIR::module_system` field.
///
/// When the parser resolved `ModuleSystem::ESM` but the file also has CJS
/// signals (`has_module_exports` or non-empty `require_calls`), the file is
/// considered **mixed** and flagged as an [`KnowledgeNature::Observation`].
fn detect_module_system_finding(
    file: &ProjectFile,
    js_ir: &seshat_core::JavaScriptIR,
    findings: &mut Vec<ConventionFinding>,
) {
    match js_ir.module_system {
        ModuleSystem::ESM => {
            let has_cjs_signals = js_ir.has_module_exports || !js_ir.require_calls.is_empty();
            if has_cjs_signals {
                // Mixed ESM + CJS in same file
                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: DETECTOR_NAME.to_owned(),
                    nature: KnowledgeNature::Observation,
                    description: "Mixes ESM and CommonJS module systems in the same file"
                        .to_owned(),
                    evidence: vec![CodeEvidence {
                        file: file.path.clone(),
                        line: 0, // file-level signal, no single source line
                        end_line: 0,
                        snippet: String::new(),
                        snippet_start_line: 0,
                        anchor: AnchorKind::FileLevel,
                    }],
                    follows_convention: false,
                    kind: FindingKind::Export,
                });
            } else {
                findings.push(ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: DETECTOR_NAME.to_owned(),
                    nature: KnowledgeNature::Observation,
                    description: "Uses ESM module system".to_owned(),
                    evidence: vec![CodeEvidence {
                        file: file.path.clone(),
                        line: 0, // file-level signal, no single source line
                        end_line: 0,
                        snippet: String::new(),
                        snippet_start_line: 0,
                        anchor: AnchorKind::FileLevel,
                    }],
                    follows_convention: true,
                    kind: FindingKind::Export,
                });
            }
        }
        ModuleSystem::CommonJS => {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Observation,
                description: "Uses CommonJS module system".to_owned(),
                evidence: vec![CodeEvidence {
                    file: file.path.clone(),
                    line: 0, // file-level signal, no single source line
                    end_line: 0,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::FileLevel,
                }],
                follows_convention: true,
                kind: FindingKind::Export,
            });
        }
        ModuleSystem::Unknown => {
            // No module system signals — nothing to report.
        }
    }
}

// ---------------------------------------------------------------------------
// Rust detection
// ---------------------------------------------------------------------------

fn detect_rust(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    let rust_ir = match &file.language_ir {
        LanguageIR::Rust(ir) => ir,
        _ => return findings,
    };

    // --- pub usage patterns --------------------------------------------------
    let pub_functions = file.functions.iter().filter(|f| f.is_public).count();
    let total_functions = file.functions.len();
    let pub_types = file.types.iter().filter(|t| t.is_public).count();
    let total_types = file.types.len();

    let total_items = total_functions + total_types;
    let pub_items = pub_functions + pub_types;

    if total_items > 0 {
        let mut evidence = Vec::new();

        // Gather evidence from public functions.
        for f in file.functions.iter().filter(|f| f.is_public).take(3) {
            evidence.push(CodeEvidence {
                file: file.path.clone(),
                line: f.line,
                end_line: f.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            });
        }

        // Gather evidence from public types.
        for t in file.types.iter().filter(|t| t.is_public).take(3) {
            evidence.push(CodeEvidence {
                file: file.path.clone(),
                line: t.line,
                end_line: t.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            });
        }

        if pub_items > 0 {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Observation,
                description: "pub visibility pattern (Rust)".to_owned(),
                evidence,
                follows_convention: true,
                kind: FindingKind::Export,
            });
        }
    }

    // --- mod re-export patterns ----------------------------------------------
    if !rust_ir.mod_declarations.is_empty() {
        // ModDeclaration now carries a real line number, so detect_with_source
        // will extract up to max_lines of source context starting at that line.
        let mod_evidence: Vec<CodeEvidence> = rust_ir
            .mod_declarations
            .iter()
            .take(5)
            .map(|m| CodeEvidence {
                file: file.path.clone(),
                line: m.line,
                end_line: m.line,
                snippet: String::new(),
                snippet_start_line: 0, // filled by detect_with_source,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        let is_lib_or_mod = is_lib_or_mod_file(&file.path);

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: if is_lib_or_mod {
                KnowledgeNature::Convention
            } else {
                KnowledgeNature::Observation
            },
            description: if is_lib_or_mod {
                "Module declarations (module root file) (Rust)".to_owned()
            } else {
                "Module declarations (Rust)".to_owned()
            },
            evidence: mod_evidence,
            follows_convention: true,
            kind: FindingKind::Export,
        });
    }

    // --- pub re-exports from exports vec ------------------------------------
    if !file.exports.is_empty() {
        let reexport_evidence: Vec<CodeEvidence> = file
            .exports
            .iter()
            .take(5)
            .map(|e| CodeEvidence {
                file: file.path.clone(),
                line: e.line,
                end_line: e.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Convention,
            description: "Uses pub use re-exports (Rust)".to_owned(),
            evidence: reexport_evidence,
            follows_convention: true,
            kind: FindingKind::Export,
        });
    }

    findings
}

/// Check if the file is a `lib.rs` or `mod.rs` (module root).
fn is_lib_or_mod_file(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    file_name == "lib.rs" || file_name == "mod.rs"
}

// ---------------------------------------------------------------------------
// Python detection
// ---------------------------------------------------------------------------

fn detect_python(file: &ProjectFile) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    let py_ir = match &file.language_ir {
        LanguageIR::Python(ir) => ir,
        _ => return findings,
    };

    // --- __all__ export pattern ----------------------------------------------
    if py_ir.has_all_export {
        let evidence = if !file.exports.is_empty() {
            file.exports
                .iter()
                .take(5)
                .map(|e| CodeEvidence {
                    file: file.path.clone(),
                    line: e.line,
                    end_line: e.line,
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
            description: "Uses __all__ to define explicit public API".to_owned(),
            evidence,
            follows_convention: true,
            kind: FindingKind::Export,
        });
    }

    // --- __init__.py re-export pattern --------------------------------------
    if py_ir.is_init_file && (!file.exports.is_empty() || !file.imports.is_empty()) {
        let evidence: Vec<CodeEvidence> = file
            .exports
            .iter()
            .take(3)
            .map(|e| CodeEvidence {
                file: file.path.clone(),
                line: e.line,
                end_line: e.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .chain(file.imports.iter().take(3).map(|i| CodeEvidence {
                file: file.path.clone(),
                line: i.line,
                end_line: i.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            }))
            .collect();

        if !evidence.is_empty() {
            findings.push(ConventionFinding {
                file_path: file.path.clone(),
                detector_name: DETECTOR_NAME.to_owned(),
                nature: KnowledgeNature::Convention,
                description: "Uses __init__.py as package re-export point".to_owned(),
                evidence,
                follows_convention: true,
                kind: FindingKind::Export,
            });
        }
    }

    // --- Explicit exports without __all__ (missing __all__) ------------------
    if !py_ir.has_all_export && !file.exports.is_empty() {
        let evidence: Vec<CodeEvidence> = file
            .exports
            .iter()
            .take(5)
            .map(|e| CodeEvidence {
                file: file.path.clone(),
                line: e.line,
                end_line: e.line,
                snippet: String::new(),
                snippet_start_line: 0,
                anchor: AnchorKind::CallSite,
            })
            .collect();

        findings.push(ConventionFinding {
            file_path: file.path.clone(),
            detector_name: DETECTOR_NAME.to_owned(),
            nature: KnowledgeNature::Observation,
            description: "Exports without __all__ definition (Python)".to_owned(),
            evidence,
            follows_convention: false,
            kind: FindingKind::Export,
        });
    }

    findings
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{
        Function, Import, JavaScriptIR, Language, ModDeclaration, ModuleSystem, PythonIR, RustIR,
        TypeDef, TypeDefKind, TypeScriptIR,
    };
    use std::path::PathBuf;

    // -- Test helpers -------------------------------------------------------

    fn make_export(name: &str, is_default: bool, is_type_only: bool, line: usize) -> Export {
        Export {
            name: name.to_owned(),
            is_default,
            is_type_only,
            line,
        }
    }

    fn make_ts_file(path: &str, exports: Vec<Export>, ts_ir: TypeScriptIR) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports,
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(ts_ir),
            file_doc: None,
        }
    }

    fn make_js_file(path: &str, exports: Vec<Export>, js_ir: JavaScriptIR) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::JavaScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports,
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(js_ir),
            file_doc: None,
        }
    }

    fn make_rust_file(
        path: &str,
        exports: Vec<Export>,
        functions: Vec<Function>,
        types: Vec<TypeDef>,
        rust_ir: RustIR,
    ) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports,
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(rust_ir),
            file_doc: None,
        }
    }

    fn make_py_file(
        path: &str,
        exports: Vec<Export>,
        imports: Vec<Import>,
        py_ir: PythonIR,
    ) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: Language::Python,
            content_hash: String::new(),
            imports,
            exports,
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(py_ir),
            file_doc: None,
        }
    }

    // -- Trait tests ---------------------------------------------------------

    #[test]
    fn detector_name() {
        let d = ExportPatternsDetector;
        assert_eq!(d.name(), "export_patterns");
    }

    #[test]
    fn supported_languages_all() {
        let d = ExportPatternsDetector;
        assert_eq!(d.supported_languages(), Language::all());
    }

    // -- TypeScript tests ----------------------------------------------------

    #[test]
    fn ts_named_exports_only() {
        let file = make_ts_file(
            "src/utils.ts",
            vec![
                make_export("formatDate", false, false, 1),
                make_export("parseDate", false, false, 5),
                make_export("isLeapYear", false, false, 10),
            ],
            TypeScriptIR::default(),
        );
        let findings = detect_typescript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("named exports exclusively")),
            "should detect named-only export pattern"
        );
        assert!(
            findings.iter().all(|f| f.follows_convention),
            "named-only pattern should follow convention"
        );
    }

    #[test]
    fn ts_default_exports_only() {
        let file = make_ts_file(
            "src/App.ts",
            vec![make_export("App", true, false, 1)],
            TypeScriptIR {
                default_export: true,
                ..TypeScriptIR::default()
            },
        );
        let findings = detect_typescript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("default exports exclusively")),
            "should detect default-only export pattern"
        );
    }

    #[test]
    fn ts_mixed_default_and_named() {
        let file = make_ts_file(
            "src/component.ts",
            vec![
                make_export("Component", true, false, 1),
                make_export("ComponentProps", false, false, 10),
                make_export("useComponent", false, false, 20),
            ],
            TypeScriptIR::default(),
        );
        let findings = detect_typescript(&file);
        let mixed = findings
            .iter()
            .find(|f| f.description.contains("Mixes default and named"));
        assert!(mixed.is_some(), "should detect mixed export pattern");
        assert!(
            !mixed.unwrap().follows_convention,
            "mixed exports should not follow convention"
        );
        assert_eq!(
            mixed.unwrap().nature,
            KnowledgeNature::Observation,
            "mixed exports should be Observation"
        );
    }

    #[test]
    fn ts_barrel_exports_via_ir_flag() {
        let file = make_ts_file(
            "src/components/index.ts",
            vec![
                make_export("Button", false, false, 1),
                make_export("Input", false, false, 2),
            ],
            TypeScriptIR {
                has_barrel_exports: true,
                ..TypeScriptIR::default()
            },
        );
        let findings = detect_typescript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("barrel export pattern")),
            "should detect barrel exports via IR flag"
        );
    }

    #[test]
    fn ts_barrel_exports_via_path() {
        let file = make_ts_file(
            "src/index.ts",
            vec![
                make_export("UserService", false, false, 1),
                make_export("AuthService", false, false, 2),
            ],
            TypeScriptIR::default(),
        );
        let findings = detect_typescript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("barrel export pattern")),
            "should detect barrel exports via index.ts path"
        );
    }

    #[test]
    fn ts_type_only_exports() {
        let file = make_ts_file(
            "src/types.ts",
            vec![
                make_export("User", false, true, 1),
                make_export("Post", false, true, 3),
                make_export("formatUser", false, false, 5),
            ],
            TypeScriptIR::default(),
        );
        let findings = detect_typescript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("type-only exports")),
            "should detect type-only export pattern"
        );
        let type_finding = findings
            .iter()
            .find(|f| f.description.contains("type-only"))
            .unwrap();
        assert!(
            type_finding.description.contains("TypeScript"),
            "should include TypeScript language label"
        );
    }

    #[test]
    fn ts_no_exports_produces_no_findings() {
        let file = make_ts_file("src/internal.ts", vec![], TypeScriptIR::default());
        let findings = detect_typescript(&file);
        assert!(findings.is_empty(), "no exports should produce no findings");
    }

    // -- JavaScript tests ----------------------------------------------------

    #[test]
    fn js_named_exports() {
        let file = make_js_file(
            "src/utils.js",
            vec![
                make_export("add", false, false, 1),
                make_export("subtract", false, false, 5),
            ],
            JavaScriptIR::default(),
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("named exports exclusively")),
            "should detect named-only exports"
        );
    }

    #[test]
    fn js_default_exports() {
        let file = make_js_file(
            "src/App.js",
            vec![make_export("App", true, false, 1)],
            JavaScriptIR::default(),
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("default exports exclusively")),
            "should detect default-only exports"
        );
    }

    #[test]
    fn js_module_exports_commonjs() {
        let file = make_js_file(
            "src/legacy.js",
            vec![],
            JavaScriptIR {
                module_system: ModuleSystem::CommonJS,
                has_module_exports: true,
                require_calls: vec!["express".to_owned()],
                function_calls: vec![],
            },
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("CommonJS module.exports")),
            "should detect CommonJS module.exports pattern"
        );
    }

    #[test]
    fn js_barrel_exports_index_file() {
        let file = make_js_file(
            "src/index.js",
            vec![
                make_export("Foo", false, false, 1),
                make_export("Bar", false, false, 2),
                make_export("Baz", false, false, 3),
            ],
            JavaScriptIR::default(),
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("barrel export pattern")),
            "should detect barrel exports in index.js"
        );
    }

    #[test]
    fn js_no_exports_no_module_exports() {
        let file = make_js_file("src/internal.js", vec![], JavaScriptIR::default());
        let findings = detect_javascript(&file);
        assert!(findings.is_empty(), "no exports should produce no findings");
    }

    #[test]
    fn js_mixed_default_and_named() {
        let file = make_js_file(
            "src/component.js",
            vec![
                make_export("Component", true, false, 1),
                make_export("helper", false, false, 10),
            ],
            JavaScriptIR::default(),
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("Mixes default and named")),
            "should detect mixed exports"
        );
    }

    // -- JavaScript module system tests -----------------------------------------

    #[test]
    fn js_pure_esm_file() {
        let file = make_js_file(
            "src/utils.mjs",
            vec![make_export("add", false, false, 1)],
            JavaScriptIR {
                module_system: ModuleSystem::ESM,
                has_module_exports: false,
                require_calls: vec![],
                function_calls: vec![],
            },
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("Uses ESM module system")),
            "should detect ESM module system"
        );
        let esm = findings
            .iter()
            .find(|f| f.description.contains("Uses ESM module system"))
            .unwrap();
        assert!(esm.follows_convention, "ESM should follow convention");
        assert_eq!(esm.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn js_pure_cjs_file() {
        let file = make_js_file(
            "src/legacy.cjs",
            vec![],
            JavaScriptIR {
                module_system: ModuleSystem::CommonJS,
                has_module_exports: true,
                require_calls: vec!["express".to_owned()],
                function_calls: vec![],
            },
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("Uses CommonJS module system")),
            "should detect CommonJS module system"
        );
        let cjs = findings
            .iter()
            .find(|f| f.description.contains("Uses CommonJS module system"))
            .unwrap();
        assert!(cjs.follows_convention, "CommonJS should follow convention");
        assert_eq!(cjs.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn js_mixed_esm_and_cjs_file() {
        // Parser resolves mixed to ESM, but has_module_exports or require_calls
        // indicate CJS signals are also present.
        let file = make_js_file(
            "src/mixed.js",
            vec![make_export("handler", false, false, 1)],
            JavaScriptIR {
                module_system: ModuleSystem::ESM,
                has_module_exports: true,
                require_calls: vec!["path".to_owned()],
                function_calls: vec![],
            },
        );
        let findings = detect_javascript(&file);
        let mixed = findings
            .iter()
            .find(|f| f.description.contains("Mixes ESM and CommonJS"));
        assert!(
            mixed.is_some(),
            "should detect mixed ESM + CJS: {findings:?}"
        );
        let mixed = mixed.unwrap();
        assert!(
            !mixed.follows_convention,
            "mixed module systems should not follow convention"
        );
        assert_eq!(
            mixed.nature,
            KnowledgeNature::Observation,
            "mixed finding should be Observation"
        );
    }

    #[test]
    fn js_mixed_esm_with_require_calls_only() {
        // ESM module system but has require() calls (no module.exports)
        let file = make_js_file(
            "src/hybrid.js",
            vec![make_export("util", false, false, 1)],
            JavaScriptIR {
                module_system: ModuleSystem::ESM,
                has_module_exports: false,
                require_calls: vec!["fs".to_owned()],
                function_calls: vec![],
            },
        );
        let findings = detect_javascript(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("Mixes ESM and CommonJS")),
            "require() calls in ESM file should trigger mixed finding"
        );
    }

    #[test]
    fn js_unknown_module_system_no_finding() {
        let file = make_js_file(
            "src/empty.js",
            vec![],
            JavaScriptIR {
                module_system: ModuleSystem::Unknown,
                has_module_exports: false,
                require_calls: vec![],
                function_calls: vec![],
            },
        );
        let findings = detect_javascript(&file);
        // No module system findings for Unknown
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("module system")
                    || f.description.contains("ESM")
                    || f.description.contains("CommonJS")),
            "Unknown module system should produce no module system finding"
        );
    }

    // -- Rust tests ----------------------------------------------------------

    #[test]
    fn rust_pub_visibility_pattern() {
        let file = make_rust_file(
            "src/utils.rs",
            vec![],
            vec![
                Function {
                    name: "process".to_owned(),
                    is_public: true,
                    is_async: false,
                    line: 1,
                    end_line: 10,
                    parameters: vec![],
                    doc_comment: None,
                },
                Function {
                    name: "helper".to_owned(),
                    is_public: false,
                    is_async: false,
                    line: 12,
                    end_line: 20,
                    parameters: vec![],
                    doc_comment: None,
                },
            ],
            vec![TypeDef {
                name: "Config".to_owned(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 22,
                doc_comment: None,
            }],
            RustIR::default(),
        );
        let findings = detect_rust(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("pub visibility")),
            "should detect pub visibility pattern"
        );
        let pub_finding = findings
            .iter()
            .find(|f| f.description.contains("pub visibility"))
            .unwrap();
        // Static description with Rust label
        assert!(
            pub_finding.description.contains("Rust"),
            "should include Rust language label: {}",
            pub_finding.description,
        );
    }

    #[test]
    fn rust_mod_declarations_in_lib() {
        let file = make_rust_file(
            "src/lib.rs",
            vec![],
            vec![],
            vec![],
            RustIR {
                mod_declarations: vec![
                    ModDeclaration {
                        name: "config".to_owned(),
                        line: 3,
                    },
                    ModDeclaration {
                        name: "pipeline".to_owned(),
                        line: 4,
                    },
                ],
                ..RustIR::default()
            },
        );
        let findings = detect_rust(&file);
        let mod_finding = findings
            .iter()
            .find(|f| f.description.contains("Module declarations"));
        assert!(mod_finding.is_some(), "should detect mod declarations");
        assert!(
            mod_finding.unwrap().description.contains("module root"),
            "should identify lib.rs as module root"
        );
        assert_eq!(
            mod_finding.unwrap().nature,
            KnowledgeNature::Convention,
            "mod declarations in lib.rs should be Convention"
        );
    }

    #[test]
    fn rust_mod_declarations_in_regular_file() {
        let file = make_rust_file(
            "src/utils.rs",
            vec![],
            vec![],
            vec![],
            RustIR {
                mod_declarations: vec![ModDeclaration {
                    name: "helpers".to_owned(),
                    line: 2,
                }],
                ..RustIR::default()
            },
        );
        let findings = detect_rust(&file);
        let mod_finding = findings
            .iter()
            .find(|f| f.description.contains("Module declarations"));
        assert!(mod_finding.is_some(), "should detect mod declarations");
        assert_eq!(
            mod_finding.unwrap().nature,
            KnowledgeNature::Observation,
            "mod declarations in non-root file should be Observation"
        );
    }

    #[test]
    fn rust_pub_use_reexports() {
        let file = make_rust_file(
            "src/lib.rs",
            vec![
                make_export("Config", false, false, 5),
                make_export("Pipeline", false, false, 6),
            ],
            vec![],
            vec![],
            RustIR::default(),
        );
        let findings = detect_rust(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("pub use re-exports")),
            "should detect pub use re-export pattern"
        );
    }

    #[test]
    fn rust_no_public_items_no_findings() {
        let file = make_rust_file(
            "src/internal.rs",
            vec![],
            vec![Function {
                name: "private_fn".to_owned(),
                is_public: false,
                is_async: false,
                line: 1,
                end_line: 5,
                parameters: vec![],
                doc_comment: None,
            }],
            vec![],
            RustIR::default(),
        );
        let findings = detect_rust(&file);
        // Should have no pub visibility finding (0 public items)
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("pub visibility")),
            "no public items should not produce pub visibility finding"
        );
    }

    // -- Python tests --------------------------------------------------------

    #[test]
    fn python_all_export_pattern() {
        let file = make_py_file(
            "src/utils.py",
            vec![
                make_export("format_date", false, false, 2),
                make_export("parse_date", false, false, 3),
            ],
            vec![],
            PythonIR {
                has_all_export: true,
                ..PythonIR::default()
            },
        );
        let findings = detect_python(&file);
        assert!(
            findings.iter().any(|f| f.description.contains("__all__")),
            "should detect __all__ export pattern"
        );
        let all_finding = findings
            .iter()
            .find(|f| f.description.contains("__all__") && f.description.contains("explicit"))
            .unwrap();
        assert_eq!(all_finding.nature, KnowledgeNature::Convention);
        assert!(all_finding.follows_convention);
    }

    #[test]
    fn python_init_file_reexport() {
        let file = make_py_file(
            "src/package/__init__.py",
            vec![make_export("MyClass", false, false, 3)],
            vec![Import {
                module: ".submodule".to_owned(),
                names: vec!["MyClass".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            PythonIR {
                is_init_file: true,
                ..PythonIR::default()
            },
        );
        let findings = detect_python(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("__init__.py")),
            "should detect __init__.py re-export pattern"
        );
    }

    #[test]
    fn python_exports_without_all() {
        let file = make_py_file(
            "src/module.py",
            vec![
                make_export("helper", false, false, 1),
                make_export("process", false, false, 10),
            ],
            vec![],
            PythonIR::default(),
        );
        let findings = detect_python(&file);
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("without __all__")),
            "should flag exports without __all__"
        );
        let no_all = findings
            .iter()
            .find(|f| f.description.contains("without __all__"))
            .unwrap();
        assert!(
            !no_all.follows_convention,
            "missing __all__ should not follow convention"
        );
        assert_eq!(no_all.nature, KnowledgeNature::Observation);
    }

    #[test]
    fn python_no_exports_no_findings() {
        let file = make_py_file("src/internal.py", vec![], vec![], PythonIR::default());
        let findings = detect_python(&file);
        assert!(findings.is_empty(), "no exports should produce no findings");
    }

    #[test]
    fn python_init_with_all_and_exports() {
        let file = make_py_file(
            "src/package/__init__.py",
            vec![
                make_export("Foo", false, false, 2),
                make_export("Bar", false, false, 3),
            ],
            vec![Import {
                module: ".foo".to_owned(),
                names: vec!["Foo".to_owned()],
                is_type_only: false,
                line: 5,
            }],
            PythonIR {
                has_all_export: true,
                is_init_file: true,
                ..PythonIR::default()
            },
        );
        let findings = detect_python(&file);
        // Should have both __all__ finding and __init__.py finding
        assert!(
            findings.iter().any(|f| f.description.contains("__all__")),
            "should detect __all__ pattern"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.description.contains("__init__.py")),
            "should detect __init__.py pattern"
        );
        // Should NOT have "without __all__" finding
        assert!(
            !findings
                .iter()
                .any(|f| f.description.contains("without __all__")),
            "should not flag missing __all__ when it exists"
        );
    }

    // -- Cross-language via trait dispatch -----------------------------------

    #[test]
    fn detect_dispatches_to_correct_language() {
        let detector = ExportPatternsDetector;

        let ts_file = make_ts_file(
            "src/mod.ts",
            vec![make_export("Foo", false, false, 1)],
            TypeScriptIR::default(),
        );
        let ts_findings = detector.detect(&ts_file);
        assert!(!ts_findings.is_empty(), "TS file should produce findings");

        let js_file = make_js_file(
            "src/mod.js",
            vec![make_export("Bar", false, false, 1)],
            JavaScriptIR::default(),
        );
        let js_findings = detector.detect(&js_file);
        assert!(!js_findings.is_empty(), "JS file should produce findings");

        let rust_file = make_rust_file(
            "src/lib.rs",
            vec![make_export("Config", false, false, 1)],
            vec![],
            vec![],
            RustIR::default(),
        );
        let rust_findings = detector.detect(&rust_file);
        assert!(
            !rust_findings.is_empty(),
            "Rust file should produce findings"
        );

        let py_file = make_py_file(
            "src/mod.py",
            vec![make_export("helper", false, false, 1)],
            vec![],
            PythonIR {
                has_all_export: true,
                ..PythonIR::default()
            },
        );
        let py_findings = detector.detect(&py_file);
        assert!(
            !py_findings.is_empty(),
            "Python file should produce findings"
        );
    }

    // -- Evidence tests ------------------------------------------------------

    #[test]
    fn evidence_capped_at_five_entries() {
        let exports: Vec<Export> = (0..10)
            .map(|i| make_export(&format!("export_{i}"), false, false, i + 1))
            .collect();
        let file = make_ts_file("src/many.ts", exports, TypeScriptIR::default());
        let findings = detect_typescript(&file);
        for finding in &findings {
            assert!(
                finding.evidence.len() <= 5,
                "evidence should be capped at 5 entries, got {}",
                finding.evidence.len()
            );
        }
    }

    #[test]
    fn detect_with_source_sets_real_snippet() {
        let detector = ExportPatternsDetector;
        // TypeScript file with a named export at line 1.
        let file = make_ts_file(
            "src/utils.ts",
            vec![make_export("myFunc", false, false, 1)],
            TypeScriptIR::default(),
        );
        let source = "export function myFunc() {}\n";

        let findings = detector.detect_with_source(&file, source);

        assert!(!findings.is_empty(), "should have at least one finding");
        let finding = findings
            .iter()
            .find(|f| f.description.contains("named exports exclusively"))
            .expect("should have named exports finding");
        assert!(!finding.evidence.is_empty(), "finding should have evidence");
        let ev = &finding.evidence[0];
        assert_eq!(ev.file, file.path);
        // Snippet must contain the actual export keyword from source.
        assert!(
            ev.snippet.contains("myFunc"),
            "snippet must contain real source keyword 'myFunc', got: {:?}",
            ev.snippet
        );
        assert!(
            !ev.snippet.starts_with("Custom "),
            "snippet must not be a synthetic format string, got: {:?}",
            ev.snippet
        );
    }
}
