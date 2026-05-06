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
//! use std::collections::HashMap;
//!
//! let files = vec![/* parsed ProjectFiles */];
//! let source_map = HashMap::new(); // or populated for changed files
//! let config = DetectionConfig::default();
//! let results = run_all_detectors(&files, &source_map, &config, None);
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use seshat_core::{
    ConventionFinding, DetectionConfig, DetectorResults, Language, LanguageIR, ProjectFile,
    top_level_module,
};

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
/// `source_map` contains the raw source for new/changed files only. When a
/// file's path is present in the map, `detect_with_source` is called so
/// detectors can extract real code snippets. Unchanged files (absent from the
/// map) fall back to `detect` (IR-only, empty snippets).
///
/// The optional `on_progress` callback receives `(done, total)` after each
/// file completes Phase 1 (per-file) detection. The callback must be `Sync`
/// because rayon invokes it from multiple threads.
#[tracing::instrument(skip_all, fields(file_count = files.len()))]
pub fn run_all_detectors(
    files: &[ProjectFile],
    source_map: &HashMap<PathBuf, String>,
    config: &DetectionConfig,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Vec<DetectorResults> {
    let detectors = all_detectors();
    run_detectors(files, source_map, &detectors, config, on_progress)
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
    source_map: &HashMap<PathBuf, String>,
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
            let source = source_map.get(&file.path).map(String::as_str);
            if source.is_none() && !source_map.is_empty() {
                tracing::debug!(
                    path = %file.path.display(),
                    source_map_sample = ?source_map.keys().take(3).collect::<Vec<_>>(),
                    "source_map lookup missed — path key mismatch?"
                );
            }
            let findings = run_detectors_on_file(file, source, detectors);
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

    // Phase 3: drop heuristic findings whose subject package is a
    // project-internal module / workspace crate. These ("Likely X library
    // (heuristic)" / "Possible logging library (name heuristic)") fire on
    // string-pattern matches over `dependencies_used` and `imports`, and
    // a project's own internal modules trip them — `seshat_cli` matches
    // "cli", `validate_approach` matches "valid", `crate::call_logger`
    // matches "log", and so on. We compute the set once from all files
    // and filter centrally so individual detectors do not need
    // cross-file context.
    let internal_names = compute_internal_package_names(files);
    if !internal_names.is_empty() {
        for entry in &mut results {
            entry
                .findings
                .retain(|f| match heuristic_subject_package(&f.description) {
                    Some(pkg) => !package_is_internal(pkg, &internal_names),
                    None => true,
                });
        }
    }

    results
}

/// Cargo workspace member directory: every Rust workspace member crate
/// lives under `crates/{name}/...` by convention. Standard layout, NOT
/// project-specific — used identically by hundreds of OSS Rust
/// workspaces.
const RUST_WORKSPACE_MARKER: &str = "crates";

/// Python `src-layout` marker: importable packages live under `src/{pkg}/...`
/// in projects following the [src-layout] convention. Universal Python
/// best-practice ([PEP 518]/[PEP 621]-era), NOT project-specific.
///
/// [src-layout]: https://packaging.python.org/en/latest/discussions/src-layout-vs-flat-layout/
const PYTHON_SRC_LAYOUT_MARKER: &str = "src";

/// Rust path keywords reserved by the language: `use crate::...`,
/// `use super::...`, `use self::...`. Always treated as internal when
/// the project has Rust files.
const RUST_PATH_KEYWORDS: &[&str] = &["crate", "super", "self"];

/// Compute the set of project-internal module / workspace crate names.
///
/// Seshat is a project-agnostic tool: it must not bake in the names of
/// any specific repository. This helper derives the internal-name set
/// purely from STANDARD LAYOUT CONVENTIONS shared across the language
/// ecosystems we support — Cargo workspace `crates/{name}` directories,
/// Python `src-layout` packages, mod declarations.
///
/// Used to filter heuristic findings whose subject package is part of
/// the project itself (false positives like `seshat_cli` being flagged
/// as a "Likely CLI library").
///
/// Sources:
/// - **Rust**: workspace crate names harvested from path segments
///   `crates/{name}/...`, stored in canonical (underscored) form so the
///   set is not bloated with both `seshat-cli` and `seshat_cli`.
///   `package_is_internal` normalises hyphens on lookup. Plus every
///   `mod {name};` declaration encountered in any Rust file.
/// - **Python**: top-level package names harvested from `src/{pkg}/...`
///   paths.
/// - The Rust path keywords `crate`, `super`, `self` are only inserted
///   when the project actually contains Rust files — otherwise they
///   pollute the filter for pure-Python or pure-JS projects.
///
/// `pub(crate)` rather than `pub`: this is a pipeline-internal helper
/// with no caller outside the crate. Promote to `pub` only when an
/// external caller materialises.
pub(crate) fn compute_internal_package_names(files: &[ProjectFile]) -> HashSet<String> {
    let mut names: HashSet<String> = HashSet::new();
    let mut has_rust = false;

    for file in files {
        let path = file.path.to_string_lossy();

        match file.language {
            Language::Rust => {
                has_rust = true;
                // Match `crates/{name}/` anywhere in the path so the
                // logic works for both relative (used in fixtures) and
                // absolute (real scans of workspace projects on disk)
                // file paths.
                if let Some(name) = segment_after(&path, RUST_WORKSPACE_MARKER) {
                    names.insert(canonicalise_pkg_name(name));
                }
                if let LanguageIR::Rust(ref ir) = file.language_ir {
                    for md in &ir.mod_declarations {
                        names.insert(md.name.clone());
                    }
                }
            }
            Language::Python => {
                // Top-level package directly after `src/`. Works for both
                // absolute and relative paths. Layouts that omit `src/`
                // are out of scope for this filter — those projects do
                // not exhibit the heuristic-noise bug we are addressing.
                if let Some(name) = segment_after(&path, PYTHON_SRC_LAYOUT_MARKER) {
                    names.insert(name.to_owned());
                }
            }
            Language::TypeScript | Language::JavaScript => {
                // Out of scope for the heuristic-noise bug class — TS/JS
                // package internalness is captured via relative paths
                // (`./`, `../`) at parse time, before dependencies_used
                // is built.
            }
        }
    }

    if has_rust {
        for kw in RUST_PATH_KEYWORDS {
            names.insert((*kw).to_owned());
        }
    }

    names
}

/// Canonical package-name form used for the internal-names set.
///
/// Rust crates may be declared with hyphens in `Cargo.toml`
/// (`seshat-cli`) but referenced with underscores in `use` paths
/// (`use seshat_cli::...`). Store one canonical form only; the
/// `package_is_internal` lookup normalises subject candidates the same
/// way.  Avoids allocating a fresh String when the input is already
/// hyphen-free.
fn canonicalise_pkg_name(name: &str) -> String {
    if name.contains('-') {
        name.replace('-', "_")
    } else {
        name.to_owned()
    }
}

/// Extract the path component immediately after `marker` in a path.
/// Accepts both `/` (POSIX) and `\` (Windows) separators so the harvest
/// works on every platform `seshat scan` runs on. Returns `None` if
/// `marker` is absent or is followed by an empty/missing segment.
///
/// Works for both relative (`crates/foo/src/lib.rs`) and absolute
/// (`/Users/x/proj/crates/foo/src/lib.rs`) inputs.
///
/// The earlier implementation used `iter.next()?` inside a while-let,
/// which aborted the entire scan on the first marker that lacked a
/// successor — silently skipping later occurrences. This version walks
/// to completion and only returns `Some` on the first valid match.
fn segment_after<'a>(path: &'a str, marker: &str) -> Option<&'a str> {
    let segments: Vec<&str> = path.split(['/', '\\']).collect();
    for window in segments.windows(2) {
        if window[0] == marker && !window[1].is_empty() {
            return Some(window[1]);
        }
    }
    None
}

/// Heuristic-marker prefixes the detectors emit verbatim. Splitting on
/// these specific markers (rather than on a generic `": "`) keeps the
/// extraction robust if the description ever contains additional
/// colon-space pairs — e.g. a future "Possible X library (heuristic):
/// foo: bar" would be parsed correctly as subject `foo: bar`.
const HEURISTIC_MARKERS: &[&str] = &["(heuristic): ", "(name heuristic): "];

/// Extract the subject package from a heuristic finding's description.
///
/// Heuristic findings carry one of these markers:
/// - `"... (heuristic): {pkg}"`
/// - `"... (name heuristic): {pkg}"`
///
/// Returns `None` for non-heuristic findings (canonical libs, style,
/// conflicts, etc.) so they are never filtered.
fn heuristic_subject_package(desc: &str) -> Option<&str> {
    for marker in HEURISTIC_MARKERS {
        if let Some(idx) = desc.find(marker) {
            let start = idx + marker.len();
            return Some(desc[start..].trim());
        }
    }
    None
}

/// Is `pkg` part of the project itself?
///
/// Compares the leading segment (extracted via the shared
/// [`top_level_module`] helper, which handles `::`, `.`, `/`, ` `)
/// against the internal-names set after canonicalising hyphens.
/// Avoids the `replace('-', "_")` allocation when the head has no
/// hyphens.
fn package_is_internal(pkg: &str, internal: &HashSet<String>) -> bool {
    let head = top_level_module(pkg);
    if head.is_empty() {
        return false;
    }
    if internal.contains(head) {
        return true;
    }
    if head.contains('-') {
        return internal.contains(&head.replace('-', "_"));
    }
    false
}

/// Run all applicable detectors on a single file, sequentially.
///
/// When `source` is `Some`, calls `detect_with_source` for real snippets.
/// When `source` is `None` (unchanged file), calls `detect` (IR-only).
/// A failing detector is logged and skipped; remaining detectors still run.
fn run_detectors_on_file(
    file: &ProjectFile,
    source: Option<&str>,
    detectors: &[Box<dyn ConventionDetector>],
) -> Vec<ConventionFinding> {
    if source.is_none() {
        tracing::warn!(
            path = %file.path.display(),
            "No source available for file — snippets will be empty"
        );
    }
    let mut findings = Vec::new();

    for detector in detectors {
        // Skip detectors that don't support this file's language.
        if !detector.supported_languages().contains(&file.language) {
            continue;
        }

        let result = match source {
            Some(src) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                detector.detect_with_source(file, src)
            })),
            None => {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detector.detect(file)))
            }
        };

        match result {
            Ok(mut r) => findings.append(&mut r),
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
    use seshat_core::{AnchorKind, FindingKind};
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
                    file: file.path.clone(),
                    line: 1,
                    end_line: 1,
                    snippet: String::new(),
                    snippet_start_line: 0,
                    anchor: AnchorKind::CallSite,
                }],
                follows_convention: true,
                kind: FindingKind::Other,
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
                kind: FindingKind::Other,
            }]
        }

        fn supported_languages(&self) -> &[Language] {
            &[Language::Rust]
        }
    }

    fn empty_source_map() -> HashMap<PathBuf, String> {
        HashMap::new()
    }

    #[test]
    fn pipeline_empty_file_list() {
        let config = DetectionConfig::default();
        let results = run_all_detectors(&[], &empty_source_map(), &config, None);
        assert!(results.is_empty());
    }

    #[test]
    fn pipeline_no_detectors() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = Vec::new();
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, None);
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_detector_on_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].findings.len(), 1);
        assert_eq!(results[0].findings[0].detector_name, "always_find");
    }

    #[test]
    fn pipeline_uses_detect_with_source_when_source_present() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let mut source_map = HashMap::new();
        source_map.insert(PathBuf::from("a.rs"), "fn main() {}".to_owned());
        let results = run_detectors(&files, &source_map, &detectors, &config, None);
        assert_eq!(results.len(), 1);
        // The provided detect_with_source fills the snippet from real source.
        // Evidence has line:1 → extract_snippet returns the first line.
        assert_eq!(results[0].findings[0].evidence[0].snippet, "fn main() {}");
    }

    #[test]
    fn pipeline_uses_detect_when_source_absent() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, None);
        assert_eq!(results.len(), 1);
        // detect() returns empty snippet
        assert_eq!(results[0].findings[0].evidence[0].snippet, "");
    }

    #[test]
    fn pipeline_skips_unsupported_language() {
        let files = vec![make_ts_file("a.ts")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, None);
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_rust_detector_on_rust_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, None);
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
        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, None);
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
        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, None);
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

        let results = run_detectors(&files, &empty_source_map(), &detectors, &config, Some(&cb));
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

    // -- Internal-name harvesting & heuristic filter (Fix 5) ---------------

    #[test]
    fn internal_names_collects_workspace_crates_from_paths() {
        let files = vec![
            make_rust_file("crates/seshat-cli/src/lib.rs"),
            make_rust_file("crates/seshat-detectors/src/pipeline.rs"),
        ];
        let names = compute_internal_package_names(&files);
        // Canonical (underscored) form only — package_is_internal
        // normalises on lookup.
        assert!(names.contains("seshat_cli"));
        assert!(names.contains("seshat_detectors"));
        // Hyphenated form must NOT bloat the set.
        assert!(!names.contains("seshat-cli"));
        assert!(!names.contains("seshat-detectors"));
        // package_is_internal still recognises the hyphenated form.
        assert!(package_is_internal("seshat-cli", &names));
        assert!(package_is_internal("seshat_cli", &names));
    }

    /// Real seshat scans use absolute paths
    /// (`/Users/.../seshat/crates/seshat-cli/src/lib.rs`); the marker
    /// extractor must locate `crates/{name}` regardless of leading
    /// segments.
    #[test]
    fn internal_names_works_on_absolute_paths() {
        let files = vec![make_rust_file(
            "/Users/dev/projects/seshat/crates/seshat-cli/src/lib.rs",
        )];
        let names = compute_internal_package_names(&files);
        assert!(names.contains("seshat_cli"));
        assert!(package_is_internal("seshat-cli", &names));
    }

    /// Rust path keywords `crate` / `super` / `self` are Rust-specific.
    /// Inserting them unconditionally pollutes the filter for projects
    /// that contain no Rust files (pure-Python, pure-JS).
    #[test]
    fn internal_names_omits_rust_keywords_for_non_rust_project() {
        let files = vec![ProjectFile {
            path: PathBuf::from("src/waltchat/web/api/app.py"),
            language: Language::Python,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
            file_doc: None,
        }];
        let names = compute_internal_package_names(&files);
        assert!(names.contains("waltchat"));
        assert!(!names.contains("crate"));
        assert!(!names.contains("super"));
        assert!(!names.contains("self"));
    }

    #[test]
    fn internal_names_includes_rust_keywords_for_rust_project() {
        let files = vec![make_rust_file("crates/foo/src/lib.rs")];
        let names = compute_internal_package_names(&files);
        assert!(names.contains("crate"));
        assert!(names.contains("super"));
        assert!(names.contains("self"));
    }

    #[test]
    fn internal_names_collects_mod_declarations() {
        use seshat_core::ModDeclaration;
        let mut file = make_rust_file("crates/seshat-cli/src/lib.rs");
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.mod_declarations = vec![
                ModDeclaration {
                    name: "args".to_owned(),
                    line: 1,
                },
                ModDeclaration {
                    name: "db".to_owned(),
                    line: 2,
                },
            ];
        }
        let names = compute_internal_package_names(&[file]);
        assert!(names.contains("args"));
        assert!(names.contains("db"));
    }

    #[test]
    fn internal_names_collects_python_top_level_packages() {
        let files = vec![
            ProjectFile {
                path: PathBuf::from("src/waltchat/web/api/app.py"),
                language: Language::Python,
                content_hash: String::new(),
                imports: Vec::new(),
                exports: Vec::new(),
                functions: Vec::new(),
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
                file_doc: None,
            },
            ProjectFile {
                path: PathBuf::from("/Users/dev/walt-chat/src/atlas/db/connector.py"),
                language: Language::Python,
                content_hash: String::new(),
                imports: Vec::new(),
                exports: Vec::new(),
                functions: Vec::new(),
                types: Vec::new(),
                dependencies_used: Vec::new(),
                language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
                file_doc: None,
            },
        ];
        let names = compute_internal_package_names(&files);
        assert!(names.contains("waltchat"));
        assert!(names.contains("atlas"));
    }

    #[test]
    fn segment_after_finds_marker() {
        assert_eq!(
            segment_after("/abs/path/crates/foo/src/lib.rs", "crates"),
            Some("foo"),
        );
        assert_eq!(
            segment_after("crates/foo/src/lib.rs", "crates"),
            Some("foo"),
        );
        assert_eq!(segment_after("src/foo/bar.py", "src"), Some("foo"));
        assert_eq!(segment_after("src/lib.rs", "crates"), None);
        assert_eq!(segment_after("crates/", "crates"), None);
    }

    /// Regression: the previous `iter.next()?` inside a while-let
    /// returned None on the FIRST marker that lacked a successor, even
    /// when a later occurrence of the marker did have one. Now the
    /// scan walks to completion and the second `crates/` is found.
    #[test]
    fn segment_after_does_not_abort_on_first_terminal_marker() {
        let result = segment_after("/proj/old_crates/crates/seshat-cli/src/lib.rs", "crates");
        assert_eq!(result, Some("seshat-cli"));
    }

    /// Windows uses `\` as separator. `to_string_lossy` on a Windows
    /// PathBuf doesn't normalise to `/`, so the harvest must accept
    /// both separators or workspace-internal-name detection silently
    /// breaks on Windows.
    #[test]
    fn segment_after_accepts_windows_separators() {
        assert_eq!(
            segment_after(r"C:\Users\dev\proj\crates\foo\src\lib.rs", "crates"),
            Some("foo"),
        );
        // Mixed separators (rare, but possible from path joins on
        // Windows where some parts use `/`).
        assert_eq!(
            segment_after(r"C:\proj/crates\foo/src\lib.rs", "crates"),
            Some("foo"),
        );
    }

    /// `heuristic_subject_package` must not be confused by descriptions
    /// containing a `: ` somewhere AFTER the heuristic marker. Anchoring
    /// on the marker prefix instead of `rsplit_once(": ")` keeps the
    /// extraction stable even for hypothetical future descriptions.
    #[test]
    fn heuristic_subject_anchored_on_marker_not_last_colon() {
        // Synthetic case: subject contains an extra ": " — extraction
        // anchors on the marker, not on the trailing colon-space.
        assert_eq!(
            heuristic_subject_package("Likely X library (heuristic): foo: subpath"),
            Some("foo: subpath"),
        );
        // Marker placement before another colon-space pair must still
        // produce the entire post-marker tail.
        assert_eq!(
            heuristic_subject_package("Possible logging library (name heuristic): a.b.c"),
            Some("a.b.c"),
        );
    }

    #[test]
    fn heuristic_subject_extraction() {
        assert_eq!(
            heuristic_subject_package("Likely CLI library (heuristic): seshat_cli"),
            Some("seshat_cli"),
        );
        assert_eq!(
            heuristic_subject_package(
                "Possible logging library (name heuristic): crate::call_logger"
            ),
            Some("crate::call_logger"),
        );
        // Non-heuristic findings must not be parsed.
        assert_eq!(
            heuristic_subject_package("Canonical logging library: tracing"),
            None,
        );
    }

    #[test]
    fn package_is_internal_handles_paths_and_normalisation() {
        let mut internal = HashSet::new();
        internal.insert("seshat_cli".to_owned());
        internal.insert("waltchat".to_owned());

        // Hyphen normalisation.
        assert!(package_is_internal("seshat-cli", &internal));
        // Dotted Python path: leading segment is the project package.
        assert!(package_is_internal(
            "waltchat.web.api.services.schema_inspector",
            &internal,
        ));
        // Rust ::-prefixed internal path.
        let mut with_log_ob = internal.clone();
        with_log_ob.insert("crate".to_owned());
        assert!(package_is_internal(
            "crate::logging_observability",
            &with_log_ob,
        ));
        // External lib stays external.
        assert!(!package_is_internal("tracing", &internal));
    }

    /// Heuristic findings whose subject package matches a workspace crate
    /// must be filtered out of the run_detectors result.
    #[test]
    fn run_detectors_drops_heuristic_for_internal_workspace_crate() {
        struct InternalHeuristicDetector;
        impl ConventionDetector for InternalHeuristicDetector {
            fn name(&self) -> &'static str {
                "h"
            }
            fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
                vec![ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "h".to_owned(),
                    nature: KnowledgeNature::Observation,
                    description: "Likely CLI library (heuristic): seshat_cli".to_owned(),
                    evidence: Vec::new(),
                    follows_convention: true,
                    kind: FindingKind::Other,
                }]
            }
            fn supported_languages(&self) -> &[Language] {
                Language::all()
            }
        }

        let files = vec![make_rust_file("crates/seshat-cli/src/lib.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(InternalHeuristicDetector)];
        let cfg = DetectionConfig::default();
        let results = run_detectors(&files, &empty_source_map(), &detectors, &cfg, None);
        assert!(
            results.iter().all(|r| r.findings.is_empty()),
            "heuristic referencing an internal workspace crate must be filtered, got: {:?}",
            results,
        );
    }

    /// Non-heuristic findings (canonical libs, style, conflicts) must be
    /// preserved even when their description mentions an internal name.
    #[test]
    fn run_detectors_keeps_non_heuristic_findings() {
        struct CanonicalDetector;
        impl ConventionDetector for CanonicalDetector {
            fn name(&self) -> &'static str {
                "c"
            }
            fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding> {
                vec![ConventionFinding {
                    file_path: file.path.clone(),
                    detector_name: "c".to_owned(),
                    nature: KnowledgeNature::Convention,
                    description: "Canonical logging library: tracing".to_owned(),
                    evidence: Vec::new(),
                    follows_convention: true,
                    kind: FindingKind::Other,
                }]
            }
            fn supported_languages(&self) -> &[Language] {
                Language::all()
            }
        }

        let files = vec![make_rust_file("crates/seshat-cli/src/lib.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(CanonicalDetector)];
        let cfg = DetectionConfig::default();
        let results = run_detectors(&files, &empty_source_map(), &detectors, &cfg, None);
        let total: usize = results.iter().map(|r| r.findings.len()).sum();
        assert_eq!(total, 1, "canonical lib finding must survive the filter");
    }
}
