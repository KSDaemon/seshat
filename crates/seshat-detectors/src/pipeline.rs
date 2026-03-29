//! Detection pipeline orchestration (ADR-6).
//!
//! Files are processed in parallel via [`rayon::par_iter()`]; all registered
//! detectors run **sequentially** per file. A failing detector logs a warning
//! and is skipped — it does not crash the pipeline.
//!
//! # Usage
//!
//! ```ignore
//! use seshat_detectors::pipeline::{all_detectors, run_all_detectors};
//! use seshat_core::DetectionConfig;
//!
//! let files = vec![/* parsed ProjectFiles */];
//! let config = DetectionConfig::default();
//! let results = run_all_detectors(&files, &config);
//! ```

use rayon::prelude::*;
use seshat_core::{ConventionFinding, DetectionConfig, DetectorResults, ProjectFile};

use crate::dependency_usage::DependencyUsageDetector;
use crate::trait_def::ConventionDetector;

/// Return all registered convention detectors.
///
/// New detectors are added here as they are implemented. The pipeline
/// invokes each detector returned by this function.
pub fn all_detectors() -> Vec<Box<dyn ConventionDetector>> {
    vec![Box::new(DependencyUsageDetector)]
}

/// Run all registered detectors on the given files.
///
/// Per ADR-6, files are processed in parallel via `rayon::par_iter()` and
/// detectors run sequentially per file. A detector that panics or returns
/// an error is logged at `warn` level and skipped for that file.
#[tracing::instrument(skip_all, fields(file_count = files.len()))]
pub fn run_all_detectors(files: &[ProjectFile], config: &DetectionConfig) -> Vec<DetectorResults> {
    let detectors = all_detectors();
    run_detectors(files, &detectors, config)
}

/// Run a specific set of detectors on the given files.
///
/// This lower-level function is useful for testing with custom detector lists.
pub fn run_detectors(
    files: &[ProjectFile],
    detectors: &[Box<dyn ConventionDetector>],
    _config: &DetectionConfig,
) -> Vec<DetectorResults> {
    files
        .par_iter()
        .map(|file| {
            let findings = run_detectors_on_file(file, detectors);
            DetectorResults {
                file_path: file.path.clone(),
                findings,
            }
        })
        .collect()
}

/// Run all applicable detectors on a single file, sequentially.
///
/// A failing detector is logged and skipped; remaining detectors still run.
fn run_detectors_on_file(
    file: &ProjectFile,
    detectors: &[Box<dyn ConventionDetector>],
) -> Vec<ConventionFinding> {
    let mut findings = Vec::new();

    for detector in detectors {
        // Skip detectors that don't support this file's language.
        if !detector.supported_languages().contains(&file.language) {
            continue;
        }

        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detector.detect(file))) {
            Ok(mut result) => findings.append(&mut result),
            Err(_) => {
                tracing::warn!(
                    detector = detector.name(),
                    file = %file.path.display(),
                    "Detector panicked; skipping for this file"
                );
            }
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{CodeEvidence, KnowledgeNature, Language, RustIR, TypeScriptIR};
    use std::path::PathBuf;

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

    struct AlwaysFindDetector;

    impl ConventionDetector for AlwaysFindDetector {
        fn name(&self) -> &'static str {
            "always_find"
        }

        fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
            vec![ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "always_find".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "always found".to_owned(),
                evidence: vec![CodeEvidence {
                    line: 1,
                    end_line: 1,
                    snippet: "example".to_owned(),
                }],
                follows_convention: true,
            }]
        }

        fn supported_languages(&self) -> &[Language] {
            Language::all()
        }
    }

    struct PanickingDetector;

    impl ConventionDetector for PanickingDetector {
        fn name(&self) -> &'static str {
            "panicking"
        }

        fn detect(&self, _file: &ProjectFile) -> Vec<ConventionFinding> {
            panic!("intentional panic for testing");
        }

        fn supported_languages(&self) -> &[Language] {
            Language::all()
        }
    }

    struct RustOnlyDetector;

    impl ConventionDetector for RustOnlyDetector {
        fn name(&self) -> &'static str {
            "rust_only"
        }

        fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
            vec![ConventionFinding {
                file_path: file.path.clone(),
                detector_name: "rust_only".to_owned(),
                nature: KnowledgeNature::Convention,
                description: "rust finding".to_owned(),
                evidence: Vec::new(),
                follows_convention: true,
            }]
        }

        fn supported_languages(&self) -> &[Language] {
            &[Language::Rust]
        }
    }

    #[test]
    fn pipeline_empty_file_list() {
        let config = DetectionConfig::default();
        let results = run_all_detectors(&[], &config);
        assert!(results.is_empty());
    }

    #[test]
    fn pipeline_no_detectors() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = Vec::new();
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config);
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_detector_on_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].findings.len(), 1);
        assert_eq!(results[0].findings[0].detector_name, "always_find");
    }

    #[test]
    fn pipeline_skips_unsupported_language() {
        let files = vec![make_ts_file("a.ts")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config);
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_rust_detector_on_rust_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].findings.len(), 1);
        assert_eq!(results[0].findings[0].detector_name, "rust_only");
    }

    #[test]
    fn failing_detector_is_skipped_others_still_run() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> =
            vec![Box::new(PanickingDetector), Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config);
        assert_eq!(results.len(), 1);
        // The panicking detector is skipped, but AlwaysFindDetector still runs.
        assert_eq!(results[0].findings.len(), 1);
        assert_eq!(results[0].findings[0].detector_name, "always_find");
    }

    #[test]
    fn pipeline_processes_multiple_files() {
        let files = vec![
            make_rust_file("a.rs"),
            make_rust_file("b.rs"),
            make_ts_file("c.ts"),
        ];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config);
        assert_eq!(results.len(), 3);
        for result in &results {
            assert_eq!(result.findings.len(), 1);
        }
    }

    #[test]
    fn all_detectors_returns_vec() {
        let detectors = all_detectors();
        assert!(
            !detectors.is_empty(),
            "should have at least one registered detector"
        );
        assert!(
            detectors.iter().any(|d| d.name() == "dependency_usage"),
            "dependency_usage detector should be registered"
        );
    }
}
