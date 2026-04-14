//! Convention detector trait definition.
//!
//! Every detector implements [`ConventionDetector`], which provides a uniform
//! interface for the detection pipeline. The trait is object-safe so that
//! detectors can be stored as `Box<dyn ConventionDetector>` and dispatched
//! dynamically at runtime.

use seshat_core::{ConventionFinding, Language, ProjectFile};

use crate::snippet::extract_snippet;

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
    /// findings with line coordinates, then fills each evidence snippet with
    /// real source lines via [`extract_snippet`].
    ///
    /// Evidence items with `line == 0` (file-level signals with no source
    /// line) are left unchanged.
    ///
    /// Override only if a detector needs fundamentally different behavior on
    /// the source-available path (not just a different `max_lines`).
    fn detect_with_source(&self, file: &ProjectFile, source: &str) -> Vec<ConventionFinding> {
        let mut findings = self.detect(file);
        let max = self.snippet_max_lines();
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
    use seshat_core::RustIR;
    use seshat_core::ir::LanguageIR;
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
}
