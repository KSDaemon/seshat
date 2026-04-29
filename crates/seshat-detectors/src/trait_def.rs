//! Convention detector trait definition.
//!
//! Every detector implements [`ConventionDetector`], which provides a uniform
//! interface for the detection pipeline. The trait is object-safe so that
//! detectors can be stored as `Box<dyn ConventionDetector>` and dispatched
//! dynamically at runtime.

use seshat_core::{ConventionFinding, Language, ProjectFile};

use crate::snippet::extract_snippet;
use crate::usage_evidence::find_usage_evidence_for_file;

/// A pluggable convention detector.
///
/// Each detector analyzes a single [`ProjectFile`] and returns zero or more
/// [`ConventionFinding`]s. Detectors declare which languages they support via
/// [`supported_languages`](ConventionDetector::supported_languages); the
/// pipeline skips detectors whose language set does not include the file's
/// language.
///
/// # Implementing a detector
///
/// Implement [`name`], [`detect`], and [`supported_languages`]. That is all
/// that is required — [`detect_with_source`] is provided automatically via the
/// template-method pattern: it calls [`detect`] then fills in real source
/// snippets using [`snippet_max_lines`].
///
/// ```ignore
/// struct MyDetector;
///
/// impl ConventionDetector for MyDetector {
///     fn name(&self) -> &'static str { "my_detector" }
///
///     fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
///         // analyze `file` using IR and return findings with snippet: String::new()
///         Vec::new()
///     }
///
///     fn supported_languages(&self) -> &[Language] {
///         Language::all()
///     }
/// }
/// ```
///
/// If a detector needs more than the default 10 lines per snippet, override
/// [`snippet_max_lines`]:
///
/// ```ignore
/// fn snippet_max_lines(&self) -> usize { 20 }
/// ```
pub trait ConventionDetector: Send + Sync {
    /// A unique, snake_case name for this detector (e.g. `"dependency_usage"`).
    fn name(&self) -> &'static str;

    /// Analyze a single file using IR only — no source access.
    ///
    /// Called for unchanged files loaded from the DB (no source in memory).
    /// Evidence snippets must be `String::new()` on this path — they will be
    /// filled in by [`detect_with_source`] when source is available.
    ///
    /// Implementations should never panic; errors should be handled internally
    /// and an empty `Vec` returned when the file cannot be analyzed.
    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding>;

    /// Maximum number of source lines to include per evidence snippet.
    ///
    /// The default is `10`. Override to `20` for detectors that need to
    /// capture wider context (e.g. multi-line import blocks).
    fn snippet_max_lines(&self) -> usize {
        10
    }

    /// Analyze a single file with access to the raw source content.
    ///
    /// **Provided via template-method pattern** — calls [`detect`] to get
    /// findings with line coordinates, then:
    ///
    /// 1. Attempts a **call-site upgrade**: cross-references the file's imports
    ///    against its function/macro calls via [`find_usage_evidence_for_file`].
    ///    If call-site evidence is found, each finding's non-zero-line evidence
    ///    items are replaced with the call-site evidence so that snippets point
    ///    to actual usage rather than import lines.
    /// 2. Fills each retained evidence snippet with real source lines via
    ///    [`extract_snippet`].
    ///
    /// Evidence items with `line == 0` (file-level signals with no source
    /// line) are left unchanged by both steps.
    ///
    /// Override only if a detector needs fundamentally different behavior on
    /// the source-available path (not just a different `max_lines`).
    fn detect_with_source(&self, file: &ProjectFile, source: &str) -> Vec<ConventionFinding> {
        let mut findings = self.detect(file);
        let max = self.snippet_max_lines();

        // Step 1: attempt call-site upgrade.
        //
        // Cross-reference the file's imports against its function/macro calls.
        // If any matched call sites are found, replace non-zero-line evidence in
        // each finding with the call-site evidence so downstream consumers see
        // actual usage lines rather than import-declaration lines.
        let call_sites = find_usage_evidence_for_file(file, max);
        if !call_sites.is_empty() {
            for finding in &mut findings {
                // Partition: keep line-0 (file-level) evidence unchanged;
                // replace line > 0 evidence with call-site evidence.
                let file_level: Vec<_> =
                    finding.evidence.drain(..).filter(|e| e.line == 0).collect();
                finding.evidence = file_level;
                finding.evidence.extend(call_sites.iter().cloned());
            }
        }

        // Step 2: extract real source snippets for all source-anchored evidence.
        for finding in &mut findings {
            for evidence in &mut finding.evidence {
                if evidence.line > 0 {
                    // line > 0  →  source-anchored evidence: extract real code lines.
                    //
                    // When end_line == line (IR item has no range info — e.g. an
                    // import or dependency reference that occupies one line in the
                    // AST), extend the snippet window to `max` lines so callers
                    // get enough context to understand the surrounding code.
                    // When end_line > line (e.g. a function or type with a known
                    // span), honour the range but always cap at `line + max - 1`
                    // so a 2 000-line impl block doesn't produce a 2 000-line
                    // snippet — `snippet_max_lines` must be respected in both
                    // branches.
                    let cap = evidence.line + max.saturating_sub(1);
                    let effective_end = if evidence.end_line <= evidence.line {
                        cap
                    } else {
                        evidence.end_line.min(cap)
                    };
                    evidence.snippet = extract_snippet(source, evidence.line, effective_end, max);
                }
                // line == 0  →  file-level signal (e.g. file naming convention,
                // file structure).  The snippet was already set by detect() to a
                // meaningful description (e.g. "config_service [snake_case]") and
                // must NOT be overwritten here — there is no source line to extract.
                // This contract is relied upon by NamingConventionsDetector and
                // FileStructureDetector, both of which emit line:0 evidence with
                // a pre-populated snippet.
            }
        }
        findings
    }

    /// Analyze multiple files together for cross-file convention detection.
    ///
    /// This method receives **all** parsed files and can perform import-graph
    /// analysis, wrapper/facade detection, or any other cross-file pattern
    /// recognition. The default implementation returns an empty `Vec`,
    /// making this method opt-in for detectors that need it.
    ///
    /// The pipeline calls this **once** per detector after all per-file
    /// detection calls have completed.
    fn detect_cross_file(&self, _files: &[ProjectFile]) -> Vec<ConventionFinding> {
        Vec::new()
    }

    /// The set of languages this detector can handle.
    ///
    /// The pipeline only invokes detection when the file's language is in
    /// this set.
    fn supported_languages(&self) -> &[Language];
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{CodeEvidence, FunctionCall, Import, KnowledgeNature, MacroCall, RustIR};
    use std::path::PathBuf;

    /// Minimal detector — only implements the three required methods.
    /// Verify that the trait is object-safe and that the provided
    /// `detect_with_source` works without any override.
    struct StubDetector;

    impl ConventionDetector for StubDetector {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn detect(&self, _file: &ProjectFile) -> Vec<ConventionFinding> {
            Vec::new()
        }

        fn supported_languages(&self) -> &[Language] {
            Language::all()
        }
    }

    fn make_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("test.rs"),
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

    #[test]
    fn trait_is_object_safe() {
        let detector: Box<dyn ConventionDetector> = Box::new(StubDetector);
        assert_eq!(detector.name(), "stub");
    }

    #[test]
    fn stub_returns_no_findings() {
        let findings = StubDetector.detect(&make_file());
        assert!(findings.is_empty());
    }

    #[test]
    fn provided_detect_with_source_works_without_override() {
        // detect_with_source is provided — no override needed on StubDetector.
        let findings = StubDetector.detect_with_source(&make_file(), "fn foo() {}");
        assert!(findings.is_empty());
    }

    #[test]
    fn default_snippet_max_lines_is_ten() {
        assert_eq!(StubDetector.snippet_max_lines(), 10);
    }

    #[test]
    fn stub_supports_all_languages() {
        let detector = StubDetector;
        assert_eq!(detector.supported_languages().len(), 4);
        assert!(detector.supported_languages().contains(&Language::Rust));
        assert!(
            detector
                .supported_languages()
                .contains(&Language::TypeScript)
        );
        assert!(
            detector
                .supported_languages()
                .contains(&Language::JavaScript)
        );
        assert!(detector.supported_languages().contains(&Language::Python));
    }

    /// Detector that returns a finding with a very large end_line to verify
    /// that detect_with_source caps the snippet at snippet_max_lines.
    struct LargeSpanDetector;

    impl ConventionDetector for LargeSpanDetector {
        fn name(&self) -> &'static str {
            "large_span"
        }

        fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
            use seshat_core::{CodeEvidence, KnowledgeNature};
            vec![seshat_core::ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "large_span".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "large span test".to_owned(),
                evidence: vec![CodeEvidence {
                    file: file.path.clone(),
                    line: 1,
                    end_line: 2000, // huge span — must be capped
                    snippet: String::new(),
                }],
                follows_convention: true,
            }]
        }

        fn supported_languages(&self) -> &[Language] {
            Language::all()
        }
    }

    #[test]
    fn detect_with_source_caps_snippet_at_max_lines() {
        // Regression test for P-2: when end_line >> line, the snippet must
        // still be capped at snippet_max_lines (default 10).
        let file = make_file();
        // Build a 50-line source string.
        let source: String = (1..=50).map(|i| format!("line {i}\n")).collect();

        let findings = LargeSpanDetector.detect_with_source(&file, &source);
        assert_eq!(findings.len(), 1);
        let snippet = &findings[0].evidence[0].snippet;
        let line_count = snippet.lines().count();
        assert!(
            line_count <= 10,
            "snippet must be capped at snippet_max_lines=10, got {line_count} lines: {snippet:?}"
        );
    }

    // -----------------------------------------------------------------------
    // detect_with_source call-site upgrade tests (US-008)
    // -----------------------------------------------------------------------

    /// Detector that returns evidence pointing to the import line (line 1).
    /// Used to verify the call-site upgrade replaces import-line evidence.
    struct ImportLineDetector;

    impl ConventionDetector for ImportLineDetector {
        fn name(&self) -> &'static str {
            "import_line"
        }

        fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
            vec![ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "import_line".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "uses tracing".to_owned(),
                // Evidence points at the import line (line 1) — the upgrade
                // should replace it with the actual call site.
                evidence: vec![CodeEvidence {
                    file: file.path.clone(),
                    line: 1,
                    end_line: 1,
                    snippet: String::new(),
                }],
                follows_convention: true,
            }]
        }

        fn supported_languages(&self) -> &[Language] {
            Language::all()
        }
    }

    /// Build a Rust file with:
    ///  - An import of `tracing::{info}` at line 1
    ///  - A macro call `info!` at line 5 (the call site)
    fn make_file_with_callsite() -> ProjectFile {
        let mut file = ProjectFile {
            path: PathBuf::from("src/handler.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports: vec![Import {
                module: "tracing".to_owned(),
                names: vec!["info".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![MacroCall {
                name: "info".to_owned(),
                line: 5,
            }];
        }
        file
    }

    #[test]
    fn detect_with_source_upgrades_import_line_evidence_to_call_site() {
        // Source: line 1 is the import, line 5 is the info! call.
        let source = "use tracing::info;\nfn foo() {\n    let x = 1;\n    let y = 2;\n    info!(\"hello\");\n}\n";
        let file = make_file_with_callsite();

        let findings = ImportLineDetector.detect_with_source(&file, source);
        assert_eq!(findings.len(), 1);
        // The upgrade should have replaced line-1 (import) evidence with
        // line-5 (call site) evidence.
        assert_eq!(findings[0].evidence.len(), 1);
        assert_eq!(
            findings[0].evidence[0].line, 5,
            "evidence should point to call site at line 5, not import at line 1"
        );
        // The snippet should contain the info! call, not the import.
        assert!(
            findings[0].evidence[0].snippet.contains("info!"),
            "snippet should contain the info! call, got: {:?}",
            findings[0].evidence[0].snippet
        );
    }

    #[test]
    fn detect_with_source_preserves_line_zero_evidence_during_upgrade() {
        // A detector that returns a mix of line-0 (file-level) and line-1
        // (import-line) evidence. The upgrade should remove line-1 and add
        // call-site evidence, but line-0 must be preserved.
        struct MixedEvidenceDetector;
        impl ConventionDetector for MixedEvidenceDetector {
            fn name(&self) -> &'static str {
                "mixed"
            }
            fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
                vec![ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "mixed".to_owned(),
                    nature: KnowledgeNature::Convention,
                    description: "mixed evidence".to_owned(),
                    evidence: vec![
                        CodeEvidence {
                            file: file.path.clone(),
                            line: 0, // file-level — must be preserved
                            end_line: 0,
                            snippet: "file-level note".to_owned(),
                        },
                        CodeEvidence {
                            file: file.path.clone(),
                            line: 1, // import line — replaced by call site
                            end_line: 1,
                            snippet: String::new(),
                        },
                    ],
                    follows_convention: true,
                }]
            }
            fn supported_languages(&self) -> &[Language] {
                Language::all()
            }
        }

        let source = "use tracing::info;\nfn foo() {\n    let x = 1;\n    let y = 2;\n    info!(\"hello\");\n}\n";
        let file = make_file_with_callsite();

        let findings = MixedEvidenceDetector.detect_with_source(&file, source);
        assert_eq!(findings.len(), 1);
        // Should have: 1 file-level evidence + 1 call-site evidence
        assert_eq!(findings[0].evidence.len(), 2);

        // File-level evidence (line 0) must be preserved with original snippet.
        let file_level = findings[0]
            .evidence
            .iter()
            .find(|e| e.line == 0)
            .expect("file-level evidence should be preserved");
        assert_eq!(file_level.snippet, "file-level note");

        // Call-site evidence (line 5) must be present.
        let call_site = findings[0]
            .evidence
            .iter()
            .find(|e| e.line == 5)
            .expect("call-site evidence at line 5 should be present");
        assert!(
            call_site.snippet.contains("info!"),
            "call-site snippet should contain info!, got: {:?}",
            call_site.snippet
        );
    }

    #[test]
    fn detect_with_source_retains_original_evidence_when_no_call_sites() {
        // When the file has no matching call sites (no imports or no function
        // calls), detect_with_source must leave the original evidence unchanged.
        let source = "use tracing::info;\nfn foo() {\n}\n";
        // File with imports but NO macro_calls — no call-site evidence.
        let mut file = make_file();
        file.imports = vec![Import {
            module: "tracing".to_owned(),
            names: vec!["info".to_owned()],
            is_type_only: false,
            line: 1,
        }];

        let findings = ImportLineDetector.detect_with_source(&file, source);
        assert_eq!(findings.len(), 1);
        // No call sites found → original import-line evidence retained.
        assert_eq!(findings[0].evidence.len(), 1);
        assert_eq!(
            findings[0].evidence[0].line, 1,
            "original import-line evidence should be retained when no call sites match"
        );
        // Snippet should be extracted from source at line 1.
        assert!(
            findings[0].evidence[0].snippet.contains("tracing"),
            "snippet at line 1 should contain the import: {:?}",
            findings[0].evidence[0].snippet
        );
    }

    #[test]
    fn detect_with_source_upgrade_uses_call_site_for_function_call() {
        // TypeScript file: import of { logger } from winston, call site at line 10.
        use seshat_core::{TypeScriptIR, ir::LanguageIR};

        struct TsImportLineDetector;
        impl ConventionDetector for TsImportLineDetector {
            fn name(&self) -> &'static str {
                "ts_import_line"
            }
            fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
                vec![ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "ts_import_line".to_owned(),
                    nature: KnowledgeNature::Convention,
                    description: "uses winston".to_owned(),
                    evidence: vec![CodeEvidence {
                        file: file.path.clone(),
                        line: 1,
                        end_line: 1,
                        snippet: String::new(),
                    }],
                    follows_convention: true,
                }]
            }
            fn supported_languages(&self) -> &[Language] {
                Language::all()
            }
        }

        let mut file = ProjectFile {
            path: PathBuf::from("src/service.ts"),
            language: Language::TypeScript,
            content_hash: String::new(),
            imports: vec![Import {
                module: "winston".to_owned(),
                names: vec!["logger".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        };
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![FunctionCall {
                callee: "logger.info".to_owned(),
                line: 10,
                end_line: 10,
                snippet: String::new(),
            }];
        }

        // Source: 10 lines with logger.info at line 10.
        let source: String = (1..=9)
            .map(|i| format!("// line {i}\n"))
            .chain(std::iter::once("logger.info(\"started\");\n".to_owned()))
            .collect();

        let findings = TsImportLineDetector.detect_with_source(&file, &source);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].evidence[0].line, 10,
            "TS evidence should be upgraded from import line 1 to call-site line 10"
        );
        assert!(
            findings[0].evidence[0].snippet.contains("logger.info"),
            "snippet should contain logger.info call: {:?}",
            findings[0].evidence[0].snippet
        );
    }
}
