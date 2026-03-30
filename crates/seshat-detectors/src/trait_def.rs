//! Convention detector trait definition.
//!
//! Every detector implements [`ConventionDetector`], which provides a uniform
//! interface for the detection pipeline. The trait is object-safe so that
//! detectors can be stored as `Box<dyn ConventionDetector>` and dispatched
//! dynamically at runtime.

use seshat_core::{ConventionFinding, Language, ProjectFile};

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
/// ```ignore
/// struct MyDetector;
///
/// impl ConventionDetector for MyDetector {
///     fn name(&self) -> &'static str { "my_detector" }
///
///     fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
///         // analyze `file` and return findings
///         Vec::new()
///     }
///
///     fn supported_languages(&self) -> &[Language] {
///         Language::all()
///     }
/// }
/// ```
pub trait ConventionDetector: Send + Sync {
    /// A unique, snake_case name for this detector (e.g. `"dependency_usage"`).
    fn name(&self) -> &'static str;

    /// Analyze a single file and return any findings.
    ///
    /// Implementations should never panic; errors should be handled internally
    /// and an empty `Vec` returned when the file cannot be analyzed.
    fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding>;

    /// Analyze multiple files together for cross-file convention detection.
    ///
    /// This method receives **all** parsed files and can perform import-graph
    /// analysis, wrapper/facade detection, or any other cross-file pattern
    /// recognition. The default implementation returns an empty `Vec`,
    /// making this method opt-in for detectors that need it.
    ///
    /// The pipeline calls this **once** per detector after all per-file
    /// [`detect`](ConventionDetector::detect) calls have completed.
    fn detect_cross_file(&self, _files: &[ProjectFile]) -> Vec<ConventionFinding> {
        Vec::new()
    }

    /// The set of languages this detector can handle.
    ///
    /// The pipeline only invokes [`detect`](ConventionDetector::detect) when
    /// the file's language is in this set.
    fn supported_languages(&self) -> &[Language];
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::RustIR;
    use seshat_core::ir::LanguageIR;
    use std::path::PathBuf;

    /// Verify that the trait is object-safe by constructing a `Box<dyn>`.
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

    #[test]
    fn trait_is_object_safe() {
        let detector: Box<dyn ConventionDetector> = Box::new(StubDetector);
        assert_eq!(detector.name(), "stub");
    }

    #[test]
    fn stub_returns_no_findings() {
        let detector = StubDetector;
        let file = ProjectFile {
            path: PathBuf::from("test.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        };
        let findings = detector.detect(&file);
        assert!(findings.is_empty());
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
}
