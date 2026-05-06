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
//! let results = run_all_detectors(&files, &source_map, &config, &ProjectContext::default(), None);
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

/// Project-wide context computed once per scan.
///
/// Holds the precomputed data shared across all detectors and the
/// pipeline's post-processing phases — currently only the project-
/// internal name set used by the Phase 3 heuristic-noise filter, but
/// designed to grow as more cross-cutting facts accumulate (workspace
/// member list, manifest-derived metadata, project root, etc.).
///
/// The orchestrator builds this once via [`ProjectContext::from_files`]
/// and passes it into [`run_all_detectors`] / [`run_detectors`].  This
/// replaces the previous "compute_internal_package_names every time
/// run_detectors is called" pattern: every `seshat scan`, every warm-
/// tier cycle, every test invocation used to rescan all file paths
/// from scratch.
#[derive(Debug, Default, Clone)]
pub struct ProjectContext {
    /// Names treated as project-internal — workspace crate names,
    /// `mod` declarations, top-level Python packages, plus the Rust
    /// path keywords when the project contains Rust files.
    pub internal_names: HashSet<String>,
}

impl ProjectContext {
    /// Build the context from the full list of parsed project files.
    ///
    /// O(n) over file count plus the per-file IR scan in
    /// [`compute_internal_package_names`]. Callers should construct
    /// once per scan / warm-tier cycle and pass by reference.
    pub fn from_files(files: &[ProjectFile]) -> Self {
        Self {
            internal_names: compute_internal_package_names(files),
        }
    }
}

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
    context: &ProjectContext,
    on_progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> Vec<DetectorResults> {
    let detectors = all_detectors();
    run_detectors(files, source_map, &detectors, config, context, on_progress)
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
    context: &ProjectContext,
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
    // matches "log", and so on.
    //
    // The internal-name set lives on `context` and is precomputed once
    // by the orchestrator — we no longer rebuild it on every pipeline
    // call. Dispatch on `FindingKind::Heuristic` is structural; the
    // subject package is still parsed out of the description.
    if !context.internal_names.is_empty() {
        for entry in &mut results {
            entry.findings.retain(|f| match f.kind {
                seshat_core::FindingKind::Heuristic => {
                    match heuristic_subject_package(&f.description) {
                        Some(pkg) => !package_is_internal(pkg, &context.internal_names),
                        None => true,
                    }
                }
                _ => true,
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

/// Path segments that look like vendored / build / cache directories.
///
/// The Python flat-layout harvester adds every directory segment between
/// the project root and a file as an internal-package name. That works
/// well for the project's own subtrees (`tests/`, `slm/`, `atlas/`),
/// but if a scan accidentally includes a vendored dependency tree
/// (`vendor/django/forms.py`, `node_modules/foo/bar.js`,
/// `.venv/lib/python3.12/site-packages/...`) every directory below
/// these "container" segments would pollute `internal_names` and silence
/// legitimate heuristic findings for those very third-party packages
/// (e.g. `django` would suddenly be "internal").
///
/// This list is the conservative cross-language denylist of
/// universally-recognised "not your code" directories. The scanner
/// already excludes most of these by default, but seshat must be
/// defensive: a user-configured scan that includes them shouldn't
/// silently corrupt the internal-name set.
const VENDORED_DIR_NAMES: &[&str] = &[
    ".git",
    ".tox",
    ".venv",
    ".pytest_cache",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "vendor",
    "venv",
];

/// True if `segment` matches a directory that should NOT contribute names
/// to `internal_names`. See [`VENDORED_DIR_NAMES`] for the policy.
fn is_vendored_dir(segment: &str) -> bool {
    VENDORED_DIR_NAMES.contains(&segment)
}

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

    // For Python flat layouts (no src/), we need the project root to
    // pick out top-level package directories. Compute it once as the
    // longest common prefix of all Python file paths.
    let py_root = python_project_root_prefix(files);

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
                // Skip files under vendored / build / cache directories
                // — those are not the project's own code and adding
                // their segments to `internal_names` would suppress
                // legitimate heuristic findings for the third-party
                // packages they shadow (e.g. a scan that pulled in
                // `vendor/django/forms.py` must NOT mark `django` as
                // internal).
                if path.split(['/', '\\']).any(is_vendored_dir) {
                    continue;
                }
                // src-layout: top-level package directly after `src/`.
                if let Some(name) = segment_after(&path, PYTHON_SRC_LAYOUT_MARKER) {
                    names.insert(name.to_owned());
                }
                // Flat-layout: every directory segment between the
                // project root and the file PLUS the file's own
                // module name (stem). Directory segments pick up
                // `tests/`, `scripts/`, project-internal packages
                // outside `src/` (walt's `slm/`, `atlas/`), AND
                // nested helper directories like
                // `tests/test_utils/`. The file stem covers
                // module-level helpers like `test_utils.py` that
                // get imported as `from test_utils import ...`.
                if let Some(root) = py_root.as_deref() {
                    if let Some(rel) = strip_path_prefix(&path, root) {
                        let segments: Vec<&str> =
                            rel.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
                        if segments.len() > 1 {
                            for seg in &segments[..segments.len() - 1] {
                                names.insert((*seg).to_owned());
                            }
                        }
                        if let Some(last) = segments.last() {
                            if let Some(stem) = last.strip_suffix(".py") {
                                if !stem.is_empty() && stem != "__init__" {
                                    names.insert(stem.to_owned());
                                }
                            }
                        }
                    }
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

/// Strip `root` from `path` at a path-segment boundary.
///
/// Plain `str::strip_prefix` is byte-aligned: `"src_legacy/x.py".strip_prefix("src")`
/// returns `Some("_legacy/x.py")`, polluting the harvested segment list
/// with `_legacy`. This wrapper additionally requires that the byte
/// after the stripped prefix is a separator (or the prefix consumed the
/// entire path), so a partial-name root match is rejected.
///
/// **Separator-agnostic:** `/` and `\` are treated as equivalent during
/// the prefix comparison. `python_project_root_prefix` always joins the
/// computed root with `/`, but on Windows the input paths can use `\`.
/// A naive `path.strip_prefix(root)` would byte-fail on the very first
/// separator and return `None`, silently skipping the entire flat-layout
/// harvest on Windows scans. Walking the prefix byte-by-byte and treating
/// the two separator codepoints as equivalent removes that platform
/// trap without requiring callers to pre-normalise.
///
/// Returns `None` when `path` does not start with `root` at a segment
/// boundary. Empty `root` always returns `Some(path)` so the
/// "harvest from top" fall-through still works.
fn strip_path_prefix<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if root.is_empty() {
        return Some(path);
    }
    let path_bytes = path.as_bytes();
    let root_bytes = root.as_bytes();
    if path_bytes.len() < root_bytes.len() {
        return None;
    }
    // Byte-by-byte compare. Separator characters (`/` / `\`) match
    // each other; everything else must be byte-equal. Both separators
    // are single-byte ASCII so this stays at UTF-8 boundaries.
    for (i, &rb) in root_bytes.iter().enumerate() {
        let pb = path_bytes[i];
        let bytes_equal = pb == rb;
        let separators_match = matches!((pb, rb), (b'/' | b'\\', b'/' | b'\\'),);
        if !bytes_equal && !separators_match {
            return None;
        }
    }
    let rest = &path[root_bytes.len()..];
    match rest.as_bytes().first() {
        None => Some(rest),
        Some(b'/') | Some(b'\\') => Some(&rest[1..]),
        _ => None,
    }
}

/// Compute the project root for Python flat-layout package harvesting
/// as one segment ABOVE the longest common path-segment prefix shared
/// by every Python file.
///
/// Returns `None` only when the project has zero Python files.
///
/// Why segment-based and one-above the common prefix:
///
/// 1. Comparing path SEGMENTS (not characters) prevents unrelated
///    directories with a shared character prefix from being treated as a
///    common parent. Earlier `chars().zip()` logic gave `src/x.py` and
///    `src_legacy/y.py` a common `src`, no separator, fall-through
///    root `""` — only correct by accident. Worse, `proj_a/x.py` and
///    `proj_b/y.py` produced `proj_` as a phantom common prefix.
///
/// 2. Dropping the LAST common segment is what makes single-subdirectory
///    projects work. If every file lives under `tests/` (only `tests/`
///    files in scope), the segment-prefix ends at `tests` — using that
///    as root would `strip_prefix("tests")` from every path, leaving
///    only the file-stems, and `tests` itself never enters
///    `internal_names`. A `from tests.helpers import X` would then
///    leak past the Phase 3 filter. Stripping one above means root is
///    `""`, the harvester walks `tests/foo.py` from the start, and
///    `tests` IS captured as an internal package. The same logic
///    applies to `src/myapp/api.py` + `src/myapp/db.py` (root becomes
///    `src`, harvester picks up `myapp` correctly).
///
/// Empty-root case (paths share no common directory at all, e.g.
/// `"src/..."` vs `"tests/..."`) is preserved.
fn python_project_root_prefix(files: &[ProjectFile]) -> Option<String> {
    let mut iter = files.iter().filter(|f| {
        matches!(f.language, Language::Python)
            && !f
                .path
                .to_string_lossy()
                .split(['/', '\\'])
                .any(is_vendored_dir)
    });
    let first = iter.next()?.path.to_string_lossy().to_string();
    let mut common: Vec<&str> = first.split(['/', '\\']).collect();

    for f in iter {
        let path = f.path.to_string_lossy();
        let segments: Vec<&str> = path.split(['/', '\\']).collect();
        let n = common
            .iter()
            .zip(segments.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(n);
        if common.is_empty() {
            break;
        }
    }

    // Drop the final common segment so the root sits ABOVE the deepest
    // shared directory, ensuring that directory itself is harvested.
    if !common.is_empty() {
        common.pop();
    }
    Some(common.join("/"))
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
///
/// Iterator-based to avoid a fresh `Vec<&str>` allocation per call.
/// `compute_internal_package_names` calls this once per file per
/// language scan; on a 700-file project that's 700 small Vecs of no
/// real value over a streaming windowed walk.
fn segment_after<'a>(path: &'a str, marker: &str) -> Option<&'a str> {
    let mut prev: Option<&'a str> = None;
    for seg in path.split(['/', '\\']) {
        if let Some(p) = prev {
            if p == marker && !seg.is_empty() {
                return Some(seg);
            }
        }
        prev = Some(seg);
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
///
/// Public so integration tests can use the same parser the Phase 3
/// filter uses — keeping production and assertion logic in lockstep.
/// Splitting on `": "` (as some early tests did) silently diverges when
/// a description gains extra colon-space pairs; this marker-anchored
/// scan stays correct.
pub fn heuristic_subject_package(desc: &str) -> Option<&str> {
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
        let results = run_all_detectors(
            &[],
            &empty_source_map(),
            &config,
            &ProjectContext::default(),
            None,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn pipeline_no_detectors() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = Vec::new();
        let config = DetectionConfig::default();
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_detector_on_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(AlwaysFindDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
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
        let results = run_detectors(
            &files,
            &source_map,
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
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
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
        assert_eq!(results.len(), 1);
        // detect() returns empty snippet
        assert_eq!(results[0].findings[0].evidence[0].snippet, "");
    }

    #[test]
    fn pipeline_skips_unsupported_language() {
        let files = vec![make_ts_file("a.ts")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
        assert_eq!(results.len(), 1);
        assert!(results[0].findings.is_empty());
    }

    #[test]
    fn pipeline_runs_rust_detector_on_rust_file() {
        let files = vec![make_rust_file("a.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(RustOnlyDetector)];
        let config = DetectionConfig::default();
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
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
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
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
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            None,
        );
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

        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &config,
            &ProjectContext::default(),
            Some(&cb),
        );
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
            language_ir: LanguageIR::Python(seshat_core::PythonIR::default()),
            file_doc: None,
        }
    }

    /// Regression: when every Python file lives under one subdirectory
    /// (e.g. all under `tests/`), the project root must sit ABOVE that
    /// directory so the directory itself is captured as an internal
    /// package. An earlier longest-common-prefix-based root walked into
    /// `tests/` and silently dropped `tests` from `internal_names` —
    /// `from tests.helpers import X` then leaked past the Phase 3 filter.
    #[test]
    fn internal_names_captures_single_subdir_python_package() {
        let files = vec![
            make_python_file("tests/test_a.py"),
            make_python_file("tests/test_b.py"),
            make_python_file("tests/helpers.py"),
        ];
        let names = compute_internal_package_names(&files);
        assert!(
            names.contains("tests"),
            "single-subdir flat layout must keep the subdir name as internal; got {names:?}",
        );
        assert!(names.contains("test_a"));
        assert!(names.contains("test_b"));
        assert!(names.contains("helpers"));
    }

    /// Same case for src-layout: when every file lives under
    /// `src/myapp/`, `myapp` itself must be in internal_names so
    /// `from myapp.api import X` is filtered.
    #[test]
    fn internal_names_captures_single_src_package() {
        let files = vec![
            make_python_file("src/myapp/api.py"),
            make_python_file("src/myapp/db.py"),
            make_python_file("src/myapp/__init__.py"),
        ];
        let names = compute_internal_package_names(&files);
        assert!(
            names.contains("myapp"),
            "src-layout single package must capture the package name; got {names:?}",
        );
        assert!(names.contains("api"));
        assert!(names.contains("db"));
    }

    /// Regression: char-prefix common-prefix logic produced `proj_` as a
    /// phantom common parent for unrelated sibling directories sharing
    /// a substring. Segment-prefix logic must NOT collapse them.
    #[test]
    fn internal_names_no_phantom_substring_prefix() {
        let files = vec![
            make_python_file("proj_a/x.py"),
            make_python_file("proj_b/y.py"),
        ];
        let names = compute_internal_package_names(&files);
        // Both top-level dirs must be captured. The phantom-prefix bug
        // would have produced root `proj_` and skipped both.
        assert!(names.contains("proj_a"));
        assert!(names.contains("proj_b"));
        assert!(names.contains("x"));
        assert!(names.contains("y"));
    }

    /// Regression: byte-aligned `strip_prefix` would treat
    /// `src_legacy/` as a continuation of root `src`. Segment-aware
    /// strip rejects the partial-name match and falls back gracefully.
    #[test]
    fn strip_path_prefix_rejects_partial_segment_match() {
        // The harvester never builds a non-segment-aligned root with the
        // current `python_project_root_prefix`, but the helper must
        // still defend against it for any future caller.
        assert_eq!(strip_path_prefix("src_legacy/x.py", "src"), None);
        assert_eq!(strip_path_prefix("src/foo/x.py", "src"), Some("foo/x.py"),);
        assert_eq!(strip_path_prefix("src", "src"), Some(""));
        assert_eq!(strip_path_prefix("anything", ""), Some("anything"));
        assert_eq!(strip_path_prefix(r"src\foo\x.py", "src"), Some(r"foo\x.py"));
    }

    /// Regression: on Windows, paths use `\` and the computed root uses
    /// `/` (joined from segments). A naive byte-exact strip would fail
    /// at the first separator → `None` → entire flat-layout walk
    /// skipped. The helper must treat the two separators as equivalent
    /// during the prefix comparison.
    #[test]
    fn strip_path_prefix_accepts_mixed_separators_in_multi_segment_root() {
        // Path uses `\`, root uses `/` — common on Windows.
        assert_eq!(
            strip_path_prefix(r"proj\tests\sub\a.py", "proj/tests"),
            Some(r"sub\a.py"),
        );
        // The reverse: path uses `/`, root uses `\` (defensive — root
        // construction never produces this today, but the symmetry
        // should hold).
        assert_eq!(
            strip_path_prefix("proj/tests/sub/a.py", r"proj\tests"),
            Some("sub/a.py"),
        );
        // Mixed within a single path AND a single root.
        assert_eq!(
            strip_path_prefix(r"proj/tests\sub/a.py", r"proj\tests"),
            Some("sub/a.py"),
        );
        // Partial-segment match still rejected even with mixed separators.
        assert_eq!(
            strip_path_prefix(r"proj\tests_legacy\a.py", "proj/tests"),
            None,
        );
    }

    /// End-to-end Windows-path regression: the harvester must capture
    /// directory segments and file stems regardless of which separator
    /// the input uses. Pre-fix, multi-segment Windows roots (`proj\sub\`)
    /// silently produced empty `internal_names` because
    /// `strip_path_prefix` failed at the first `\` byte.
    #[test]
    fn internal_names_handles_windows_paths_in_multi_segment_root() {
        let files = vec![
            make_python_file(r"proj\tests\sub\test_a.py"),
            make_python_file(r"proj\tests\sub\test_b.py"),
            make_python_file(r"proj\tests\sub\helpers.py"),
        ];
        let names = compute_internal_package_names(&files);
        // `sub` must be captured as the directory above the files.
        assert!(
            names.contains("sub"),
            "Windows-style path `proj\\tests\\sub\\*` must capture `sub` as internal; got {names:?}",
        );
        assert!(names.contains("test_a"));
        assert!(names.contains("test_b"));
        assert!(names.contains("helpers"));
    }

    /// Regression: a scan that includes vendored / build / cache
    /// directories must NOT pull their segments into `internal_names`.
    /// If `vendor/django/forms.py` ended up tagging `django` as
    /// internal, a `from django.urls import path` heuristic finding
    /// would be silently suppressed even though `django` is the
    /// canonical third-party package the user installed.
    #[test]
    fn internal_names_skips_vendored_directories() {
        let files = vec![
            // Real project files.
            make_python_file("src/myapp/api.py"),
            make_python_file("tests/test_api.py"),
            // Vendored / build / cache files that must NOT contribute.
            make_python_file("vendor/django/forms.py"),
            make_python_file("node_modules/foo/index.py"),
            make_python_file(".venv/lib/python3.12/site-packages/requests/api.py"),
            make_python_file("__pycache__/api.cpython-312.py"),
            make_python_file("build/lib/myapp/api.py"),
            make_python_file("dist/wheel/myapp/api.py"),
        ];
        let names = compute_internal_package_names(&files);
        // Real project segments still captured.
        assert!(names.contains("myapp"));
        assert!(names.contains("tests"));
        assert!(names.contains("api"));
        assert!(names.contains("test_api"));
        // Vendored segments must NOT be captured.
        for vendored in [
            "django",
            "forms",
            "node_modules",
            "vendor",
            ".venv",
            "venv",
            "site-packages",
            "requests",
            "__pycache__",
            "build",
            "dist",
        ] {
            assert!(
                !names.contains(vendored),
                "vendored segment {vendored:?} leaked into internal_names: {names:?}",
            );
        }
    }

    /// Empty-root case: when paths share no common parent at all, the
    /// harvester must walk every path's leading segment from the top.
    #[test]
    fn internal_names_empty_root_harvests_from_top() {
        let files = vec![
            make_python_file("src/myapp/api.py"),
            make_python_file("tests/test_api.py"),
        ];
        let names = compute_internal_package_names(&files);
        assert!(names.contains("src"));
        assert!(names.contains("myapp"));
        assert!(names.contains("tests"));
        assert!(names.contains("api"));
        assert!(names.contains("test_api"));
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
                    kind: FindingKind::Heuristic,
                }]
            }
            fn supported_languages(&self) -> &[Language] {
                Language::all()
            }
        }

        let files = vec![make_rust_file("crates/seshat-cli/src/lib.rs")];
        let detectors: Vec<Box<dyn ConventionDetector>> = vec![Box::new(InternalHeuristicDetector)];
        let cfg = DetectionConfig::default();
        let context = ProjectContext::from_files(&files);
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &cfg,
            &context,
            None,
        );
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
        let results = run_detectors(
            &files,
            &empty_source_map(),
            &detectors,
            &cfg,
            &ProjectContext::default(),
            None,
        );
        let total: usize = results.iter().map(|r| r.findings.len()).sum();
        assert_eq!(total, 1, "canonical lib finding must survive the filter");
    }
}
