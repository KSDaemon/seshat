//! Detection pipeline orchestration (ADR-6).
//!
//! Files are processed in parallel via `rayon::par_iter()`; all registered
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

use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use seshat_core::{ConventionFinding, DetectionConfig, DetectorResults, ProjectFile};

use crate::dependency_usage::DependencyUsageDetector;
use crate::error_handling::ErrorHandlingDetector;
use crate::export_patterns::ExportPatternsDetector;
use crate::file_structure::FileStructureDetector;
use crate::import_organization::ImportOrganizationDetector;
use crate::logging_observability::LoggingObservabilityDetector;
use crate::naming::NamingConventionsDetector;
use crate::test_patterns::TestPatternsDetector;
use crate::trait_def::ConventionDetector;

/// Return all registered convention detectors.
///
/// New detectors are added here as they are implemented. The pipeline
/// invokes each detector returned by this function.
pub fn all_detectors() -> Vec<Box<dyn ConventionDetector>> {
    vec![
        Box::new(DependencyUsageDetector),
        Box::new(ErrorHandlingDetector),
        Box::new(ExportPatternsDetector),
        Box::new(FileStructureDetector),
        Box::new(ImportOrganizationDetector),
        Box::new(LoggingObservabilityDetector),
        Box::new(NamingConventionsDetector),
        Box::new(TestPatternsDetector),
    ]
}

/// Run all registered detectors on the given files.
///
/// Per ADR-6, files are processed in parallel via `rayon::par_iter()` and
/// detectors run sequentially per file. A detector that panics or returns
/// an error is logged at `warn` level and skipped for that file.
///
/// The optional `on_progress` callback receives `(done, total)` after each
/// file completes Phase 1 (per-file) detection. The callback must be `Sync`
/// because rayon invokes it from multiple threads.
#[tracing::instrument(skip_all, fields(file_count = files.len()))]
pub fn run_all_detectors(
    files: &[ProjectFile],
    config: &DetectionConfig,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Vec<DetectorResults> {
    let detectors = all_detectors();
    run_detectors(files, &detectors, config, on_progress)
}

/// Run a specific set of detectors on the given files.
///
/// This lower-level function is useful for testing with custom detector lists.
/// After per-file detection, it runs each detector's
/// [`detect_cross_file`](ConventionDetector::detect_cross_file) method and
/// merges the resulting findings into the per-file results.
///
/// The optional `on_progress` callback receives `(done, total)` after each
/// file completes Phase 1 (per-file) detection.
pub fn run_detectors(
    files: &[ProjectFile],
    detectors: &[Box<dyn ConventionDetector>],
    _config: &DetectionConfig,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Vec<DetectorResults> {
    let total = files.len();
    let done_counter = AtomicUsize::new(0);

    // Phase 1: per-file detection (parallel).
    let mut results: Vec<DetectorResults> = files
        .par_iter()
        .map(|file| {
            let findings = run_detectors_on_file(file, detectors);
            let done = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(cb) = on_progress {
                cb(done, total);
            }
            DetectorResults {
                file_path: file.path.clone(),
                findings,
            }
        })
        .collect();

    // Phase 2: cross-file detection (sequential per detector).
    // Each detector's detect_cross_file() returns findings tagged with
    // file_path; we merge them into the corresponding DetectorResults.
    for detector in detectors {
        let cross_findings = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            detector.detect_cross_file(files)
        })) {
            Ok(findings) => findings,
            Err(_) => {
                tracing::warn!(
                    detector = detector.name(),
                    "Detector panicked during cross-file detection; skipping"
                );
                continue;
            }
        };

        for finding in cross_findings {
            // Try to merge into an existing DetectorResults entry for this file.
            if let Some(entry) = results
                .iter_mut()
                .find(|r| r.file_path == finding.file_path)
            {
                entry.findings.push(finding);
            } else {
                // File not seen in per-file phase — create a new entry.
                results.push(DetectorResults {
                    file_path: finding.file_path.clone(),
                    findings: vec![finding],
                });
            }
        }
    }

    results
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
        let results = run_all_detectors(&[], &config, None);
        assert!(results.is_empty());
    }

    #[test]
    fn pipeline_no_detectors() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = Vec::new();
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config, None);
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_detector_on_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].findings.len(), 1);
        assert_eq!(results[0].findings[0].detector_name, "always_find");
    }

    #[test]
    fn pipeline_skips_unsupported_language() {
        let files = vec![make_ts_file("a.ts")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config, None);
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_rust_detector_on_rust_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &detectors, &config, None);
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
        let results = run_detectors(&files, &detectors, &config, None);
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
        let results = run_detectors(&files, &detectors, &config, None);
        assert_eq!(results.len(), 3);
        for result in &results {
            assert_eq!(result.findings.len(), 1);
        }
    }

    #[test]
    fn progress_callback_receives_correct_values() {
        use std::sync::Mutex;

        let files = vec![
            make_rust_file("a.rs"),
            make_rust_file("b.rs"),
            make_ts_file("c.ts"),
        ];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();

        let progress_log: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
        let cb = |done: usize, total: usize| {
            progress_log.lock().unwrap().push((done, total));
        };

        let results = run_detectors(&files, &detectors, &config, Some(&cb));
        assert_eq!(results.len(), 3);

        let log = progress_log.lock().unwrap();
        assert_eq!(log.len(), 3, "should have 3 progress callbacks");
        // All entries should have total == 3.
        for (_, total) in log.iter() {
            assert_eq!(*total, 3);
        }
        // done values should cover 1, 2, 3 (order may vary due to rayon).
        let mut done_values: Vec<usize> = log.iter().map(|(done, _)| *done).collect();
        done_values.sort();
        assert_eq!(done_values, vec![1, 2, 3]);
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
        assert!(
            detectors.iter().any(|d| d.name() == "error_handling"),
            "error_handling detector should be registered"
        );
        assert!(
            detectors.iter().any(|d| d.name() == "import_organization"),
            "import_organization detector should be registered"
        );
        assert!(
            detectors.iter().any(|d| d.name() == "naming_conventions"),
            "naming_conventions detector should be registered"
        );
        assert!(
            detectors.iter().any(|d| d.name() == "export_patterns"),
            "export_patterns detector should be registered"
        );
        assert!(
            detectors
                .iter()
                .any(|d| d.name() == "logging_observability"),
            "logging_observability detector should be registered"
        );
        assert!(
            detectors.iter().any(|d| d.name() == "test_patterns"),
            "test_patterns detector should be registered"
        );
        assert!(
            detectors.iter().any(|d| d.name() == "file_structure"),
            "file_structure detector should be registered"
        );
    }
}
