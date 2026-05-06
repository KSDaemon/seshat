//! End-to-end snippet-quality regression test.
//!
//! Builds synthetic mini "projects" of [`ProjectFile`]s exercising every
//! bug class fixed in the snippet-quality series, runs the full detector
//! pipeline + aggregator, and asserts whole-pipeline invariants on the
//! resulting convention set.
//!
//! ## Fixture organisation
//!
//! Each Rust bug class has its own narrow fixture so the focused tests
//! load only the data they actually exercise. Reading
//! `rust_fluent_chain_fixture` is enough to understand what
//! `fluent_chain_collapses_to_single_evidence` checks; you don't have
//! to mentally separate "which bits of the kitchen-sink fixture matter
//! for this test".
//!
//! The cross-detector dedup test still needs a project that covers
//! many bug classes at once, so [`rust_combined_files`] returns the
//! per-concern fixtures as a slice and feeds the whole batch into the
//! pipeline.
//!
//! Python coverage parallels the Rust split: a `python_*_fixture`
//! family exercises the stdlib filter, flat-layout package harvesting,
//! and file-stem internal-name harvesting added in the recent
//! Python-focused fixes.
//!
//! Fixtures are constructed in-memory rather than parsed from disk —
//! seshat-detectors does not depend on the parser crate, and parsing
//! is covered by other integration suites. What this test guards is
//! the interaction between detectors, the wildcard / FQN matching in
//! `find_usage_evidence`, the cross-file internal-name filter in
//! `pipeline::run_detectors`, and the dedup in `aggregate_findings`.
//!
//! When this test fails after a future change, run
//! `seshat debug-snippets` against `crates/seshat` and `walt-chat-backend`
//! and compare descriptions / evidence — the bug almost always shows up
//! identically there.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use seshat_core::{
    DependencyUsage, DetectionConfig, Function, FunctionCall, Import, KnowledgeNature, Language,
    LanguageIR, MacroCall, ModDeclaration, ProjectFile, PythonIR, RustIR,
};
use seshat_detectors::aggregate_findings;
use seshat_detectors::pipeline::run_all_detectors;

// ===========================================================================
// Pipeline runner / shared helpers
// ===========================================================================

fn empty_source_map() -> HashMap<PathBuf, String> {
    HashMap::new()
}

/// Run the same path the real scanner takes: per-file + cross-file
/// detection, then `aggregate_findings`.
fn run_pipeline(files: &[ProjectFile]) -> Vec<seshat_detectors::AggregatedConvention> {
    let project_context = seshat_detectors::ProjectContext::from_files(files);
    let detector_results = run_all_detectors(
        files,
        &empty_source_map(),
        &DetectionConfig::default(),
        &project_context,
        None,
    );
    let findings: Vec<seshat_core::ConventionFinding> = detector_results
        .into_iter()
        .flat_map(|r| r.findings)
        .collect();
    let dates = HashMap::new();
    aggregate_findings(&findings, &DetectionConfig::default(), &dates, 0)
}

/// Helper: assert no two evidence entries within one convention point
/// to the same `(file, line, end_line)`. This is the visible-duplicates
/// bug class (assert_cmd 2x, parameter naming N×, etc.). Keyed by
/// HashSet so the helper itself does not exhibit the O(N²) anti-pattern
/// the production fix removes.
fn assert_no_duplicate_evidence(aggregated: &[seshat_detectors::AggregatedConvention]) {
    for conv in aggregated {
        let mut seen: HashSet<(&Path, usize, usize)> = HashSet::new();
        for ev in &conv.evidence {
            let key = (ev.file.as_path(), ev.line, ev.end_line);
            assert!(
                seen.insert(key),
                "convention {:?} has duplicate evidence at {key:?}",
                conv.description,
            );
        }
    }
}

// ===========================================================================
// Rust fixture building blocks
// ===========================================================================

/// Empty-IR Rust file at the given workspace path. Per-concern fixtures
/// fill in only the IR fields they need.
fn rust_file_skeleton(path: &str) -> ProjectFile {
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

/// Mutate the file's `RustIR` in place. Keeps each per-concern fixture
/// readable as "skeleton + the few fields this concern needs".
fn with_rust_ir(file: &mut ProjectFile, mutate: impl FnOnce(&mut RustIR)) {
    if let LanguageIR::Rust(ir) = &mut file.language_ir {
        mutate(ir);
    } else {
        unreachable!("rust_file_skeleton always sets LanguageIR::Rust");
    }
}

/// Fluent-chain + canonical hyphen/underscore fixture.
///
/// Exercises:
/// - **Fix 2**: three FunctionCalls all anchored at line 20 (the chain
///   start) collapse into a single evidence row in the aggregator.
/// - **Fix 5b**: `tracing_subscriber` (with underscore) classifies as
///   canonical despite the canonical-libs table listing the
///   hyphenated form. The hyphen/underscore normalisation lives in
///   `classify_rust_logging`.
fn rust_fluent_chain_fixture() -> ProjectFile {
    let mut file = rust_file_skeleton("crates/seshat-cli/src/fluent_chain.rs");
    file.imports = vec![Import {
        module: "tracing_subscriber".to_owned(),
        names: vec!["EnvFilter".to_owned()],
        is_type_only: false,
        line: 5,
    }];
    file.dependencies_used = vec![DependencyUsage {
        package: "tracing_subscriber".to_owned(),
        import_path: "tracing_subscriber::EnvFilter".to_owned(),
        line: 5,
    }];
    with_rust_ir(&mut file, |ir| {
        // Each chained method appears as its own FunctionCall sharing
        // the chain-start line. Pre-Fix-2 these produced 3 evidence
        // rows; the dedup in `usage_evidence::collapse_fluent_chain`
        // keeps only the widest (smallest start, largest end) row.
        ir.function_calls = vec![
            FunctionCall {
                callee: "tracing_subscriber::fmt".to_owned(),
                line: 20,
                end_line: 26,
                snippet: "tracing_subscriber::fmt().with_env_filter(...).init()".to_owned(),
            },
            FunctionCall {
                callee: "tracing_subscriber::with_env_filter".to_owned(),
                line: 20,
                end_line: 25,
                snippet: "tracing_subscriber::fmt().with_env_filter(...)".to_owned(),
            },
            FunctionCall {
                callee: "tracing_subscriber::init".to_owned(),
                line: 20,
                end_line: 20,
                snippet: "tracing_subscriber::fmt()".to_owned(),
            },
        ];
    });
    file
}

/// Internal-mods + heuristic-import filter fixture.
///
/// Exercises **Fix 5**: `pub mod args; pub mod db;` declarations are
/// harvested by the cross-file internal-name set so the heuristic
/// finding for the `use args::Cli` import gets dropped by the
/// pipeline's Phase 3 filter.
fn rust_internal_mods_fixture() -> ProjectFile {
    let mut file = rust_file_skeleton("crates/seshat-cli/src/internal_mods.rs");
    file.imports = vec![Import {
        module: "args".to_owned(),
        names: vec!["Cli".to_owned()],
        is_type_only: false,
        line: 7,
    }];
    file.dependencies_used = vec![DependencyUsage {
        package: "args".to_owned(),
        import_path: "args".to_owned(),
        line: 7,
    }];
    with_rust_ir(&mut file, |ir| {
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
    });
    file
}

/// Rayon wildcard prelude fixture.
///
/// Exercises **Fix 4 / R6**: the wildcard fallback (`use rayon::prelude::*`)
/// must NOT attribute receiver-style calls (`items.par_iter()`) to
/// rayon — only the namespaced-call case does. The only acceptable
/// rayon evidence is the import line itself, picked up by
/// dependency_usage's import-line fallback.
fn rust_rayon_wildcard_fixture() -> ProjectFile {
    let mut file = rust_file_skeleton("crates/seshat-cli/src/rayon_usage.rs");
    file.imports = vec![Import {
        module: "rayon::prelude".to_owned(),
        names: vec!["*".to_owned()],
        is_type_only: false,
        line: 6,
    }];
    file.dependencies_used = vec![DependencyUsage {
        package: "rayon".to_owned(),
        import_path: "rayon::prelude".to_owned(),
        line: 6,
    }];
    with_rust_ir(&mut file, |ir| {
        // Receiver-style call: pre-R6 the wildcard fallback over-
        // attributed this to rayon. After R6 the only rayon anchor
        // is the import line.
        ir.function_calls = vec![FunctionCall {
            callee: "items.par_iter".to_owned(),
            line: 50,
            end_line: 50,
            snippet: "items.par_iter().for_each(...)".to_owned(),
        }];
    });
    file
}

/// Multi-parameter function fixture for parameter-naming dedup.
///
/// Exercises **Fix 1**: a function with N parameters at the same line
/// pre-fix produced N identical evidence rows; the dedup key
/// `(file, line, end_line)` collapses them to one.
fn rust_multi_param_fixture() -> ProjectFile {
    let mut file = rust_file_skeleton("crates/seshat-cli/src/multi_param.rs");
    file.functions = vec![Function {
        name: "build_url".to_owned(),
        is_public: true,
        is_async: false,
        line: 100,
        end_line: 105,
        parameters: vec![
            "scheme".to_owned(),
            "host".to_owned(),
            "port".to_owned(),
            "path".to_owned(),
            "query".to_owned(),
        ],
        doc_comment: None,
    }];
    file
}

/// Build a Rust file at the given path that uses `tracing`. Reusable
/// across multi-file tests.
///
/// Used by [`no_duplicate_canonical_logging_across_multiple_files`] —
/// the (detector_name, description) bucket-key bug class only
/// reproduces when at least two files trigger both `dependency_usage`
/// and `logging_observability` for the same canonical library.
fn rust_tracing_file(path: &str, line_offset: usize) -> ProjectFile {
    let info_line = 10 + line_offset;
    let mut file = rust_file_skeleton(path);
    file.imports = vec![Import {
        module: "tracing".to_owned(),
        names: vec!["info".to_owned()],
        is_type_only: false,
        line: 1,
    }];
    file.dependencies_used = vec![DependencyUsage {
        package: "tracing".to_owned(),
        import_path: "tracing".to_owned(),
        line: 1,
    }];
    with_rust_ir(&mut file, |ir| {
        ir.macro_calls = vec![MacroCall {
            name: "info".to_owned(),
            line: info_line,
        }];
    });
    file
}

/// Combined Rust mini-project for cross-cutting tests. Includes one
/// file per bug class so the dedup tests have multiple detectors firing
/// across multiple files.
fn rust_combined_files() -> Vec<ProjectFile> {
    vec![
        rust_fluent_chain_fixture(),
        rust_internal_mods_fixture(),
        rust_rayon_wildcard_fixture(),
        rust_multi_param_fixture(),
    ]
}

/// Names the combined Rust fixture treats as "internal" — the
/// fixture's own workspace-crate name plus declared `mod` blocks plus
/// Rust path keywords.
///
/// Mirrors `pipeline::compute_internal_package_names` exactly:
///   - Rust workspace crates are stored in canonical (underscored) form;
///     the assertion below normalises hyphens on lookup.
///   - Rust path keywords (`crate`/`super`/`self`) belong because the
///     project has Rust files.
fn rust_combined_internal_names() -> HashSet<String> {
    let mut s = HashSet::new();
    // Workspace crate harvested from `crates/seshat-cli/...` paths.
    s.insert("seshat_cli".to_owned());
    // mod declarations from the internal_mods fixture.
    s.insert("args".to_owned());
    s.insert("db".to_owned());
    // Rust path keywords.
    s.insert("crate".to_owned());
    s.insert("super".to_owned());
    s.insert("self".to_owned());
    s
}

// ===========================================================================
// Python fixture building blocks
// ===========================================================================

/// Empty-IR Python file at the given path.
fn python_file_skeleton(path: &str) -> ProjectFile {
    ProjectFile {
        path: PathBuf::from(path),
        language: Language::Python,
        content_hash: String::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        functions: Vec::new(),
        types: Vec::new(),
        dependencies_used: Vec::new(),
        language_ir: LanguageIR::Python(PythonIR::default()),
        file_doc: None,
    }
}

/// Python module that imports a mix of stdlib (`traceback`, `logging`,
/// `unittest.mock`) and a real third-party library (`requests`).
///
/// Exercises the recent **Fix D** (Python stdlib filter) and
/// **R8** (logging detector consolidation) — stdlib imports must NOT
/// produce `(heuristic): traceback` / `(heuristic): unittest.mock`
/// findings.
fn python_stdlib_users_fixture() -> ProjectFile {
    let mut file = python_file_skeleton("src/myapp/api.py");
    file.imports = vec![
        Import {
            module: "traceback".to_owned(),
            names: vec!["format_exc".to_owned()],
            is_type_only: false,
            line: 1,
        },
        Import {
            module: "logging.config".to_owned(),
            names: vec!["dictConfig".to_owned()],
            is_type_only: false,
            line: 2,
        },
        Import {
            module: "unittest.mock".to_owned(),
            names: vec!["MagicMock".to_owned()],
            is_type_only: false,
            line: 3,
        },
        // `argparse` is a stdlib module that DOES trip
        // `classify_heuristic_domain` without the gate: lowercased
        // `argparse` contains `arg` at start-of-string → CLI domain
        // boundary match → emits "Likely CLI library (heuristic):
        // argparse". The stdlib gate is the only thing keeping this
        // out of the finding set.
        Import {
            module: "argparse".to_owned(),
            names: vec!["ArgumentParser".to_owned()],
            is_type_only: false,
            line: 4,
        },
        // A real third-party library so the file isn't completely
        // stdlib (otherwise dependency_usage may have nothing to do).
        Import {
            module: "requests".to_owned(),
            names: vec!["get".to_owned(), "post".to_owned()],
            is_type_only: false,
            line: 5,
        },
    ];
    file.dependencies_used = vec![
        DependencyUsage {
            package: "traceback".to_owned(),
            import_path: "traceback".to_owned(),
            line: 1,
        },
        DependencyUsage {
            package: "logging.config".to_owned(),
            import_path: "logging.config".to_owned(),
            line: 2,
        },
        DependencyUsage {
            package: "unittest.mock".to_owned(),
            import_path: "unittest.mock".to_owned(),
            line: 3,
        },
        DependencyUsage {
            package: "argparse".to_owned(),
            import_path: "argparse".to_owned(),
            line: 4,
        },
        DependencyUsage {
            package: "requests".to_owned(),
            import_path: "requests".to_owned(),
            line: 5,
        },
    ];
    file
}

/// Python module under `tests/` (flat-layout, NOT under `src/`) that
/// imports a same-directory helper (`from test_utils import X`).
///
/// Exercises **Fix D** flat-layout package harvesting — `tests`,
/// `test_utils` must be added to `internal_names` so they don't surface
/// as `(heuristic)` findings. Mirrors walt-chat-backend's layout where
/// `tests/`, `slm/`, `atlas/` sit at the project root.
fn python_flat_layout_test_fixture() -> ProjectFile {
    let mut file = python_file_skeleton("tests/test_api.py");
    file.imports = vec![
        // Cross-directory internal: `from tests.helpers import X`.
        // `tests` must be classified internal via flat-layout segment
        // harvesting (top-level dir, not under src/).
        Import {
            module: "tests.helpers".to_owned(),
            names: vec!["build_request".to_owned()],
            is_type_only: false,
            line: 1,
        },
        // Single-file module internal: `from test_utils import X`.
        // `test_utils` is the stem of `tests/test_utils.py` — must be
        // harvested via the file-stem branch of internal-name
        // computation.
        Import {
            module: "test_utils".to_owned(),
            names: vec!["fake_user".to_owned()],
            is_type_only: false,
            line: 2,
        },
    ];
    file.dependencies_used = vec![
        DependencyUsage {
            package: "tests.helpers".to_owned(),
            import_path: "tests.helpers".to_owned(),
            line: 1,
        },
        DependencyUsage {
            package: "test_utils".to_owned(),
            import_path: "test_utils".to_owned(),
            line: 2,
        },
    ];
    file
}

/// Companion module that establishes `test_utils.py` as a real file
/// in the project — so its stem is harvested into internal_names.
fn python_test_utils_fixture() -> ProjectFile {
    let mut file = python_file_skeleton("tests/test_utils.py");
    file.functions = vec![Function {
        name: "fake_user".to_owned(),
        is_public: true,
        is_async: false,
        line: 1,
        end_line: 3,
        parameters: vec!["name".to_owned()],
        doc_comment: None,
    }];
    file
}

/// Companion module establishing `tests/helpers.py` as a real file in
/// `tests/`. Together with [`python_test_utils_fixture`] it gives the
/// flat-layout fixture realistic project shape.
fn python_helpers_fixture() -> ProjectFile {
    let mut file = python_file_skeleton("tests/helpers.py");
    file.functions = vec![Function {
        name: "build_request".to_owned(),
        is_public: true,
        is_async: false,
        line: 1,
        end_line: 4,
        parameters: vec!["url".to_owned()],
        doc_comment: None,
    }];
    file
}

/// Combined Python mini-project for cross-cutting tests.
///
/// Shape mirrors a realistic mixed src/flat-layout repo (walt-chat-
/// backend's actual structure):
///
/// ```text
/// project_root/
/// ├── src/myapp/api.py       # python_stdlib_users_fixture
/// └── tests/
///     ├── test_api.py        # python_flat_layout_test_fixture
///     ├── helpers.py         # python_helpers_fixture
///     └── test_utils.py      # python_test_utils_fixture
/// ```
///
/// The `src/` and `tests/` siblings are essential for the flat-layout
/// internal-name harvester: `python_project_root_prefix` returns ""
/// (the longest common prefix is empty because the top-level
/// directories differ), which lets the harvester walk the full
/// segment list `["tests", "test_api.py"]` and add `tests` to
/// `internal_names`. A fixture with files only under `tests/` would
/// instead collapse the project root to `tests/`, hiding the very
/// behaviour we want to test.
fn python_combined_files() -> Vec<ProjectFile> {
    vec![
        python_stdlib_users_fixture(),
        python_flat_layout_test_fixture(),
        python_helpers_fixture(),
        python_test_utils_fixture(),
    ]
}

// ===========================================================================
// Tests — Rust
// ===========================================================================

/// Cross-cutting test: every detector running on the combined fixture
/// must agree on `(file, line, end_line)` keys — no two evidence
/// entries within one convention may share the same anchor. Pre-fix
/// this caught fluent-chain duplicates AND parameter-naming
/// duplicates AND cross-detector dupes simultaneously.
#[test]
fn no_duplicate_evidence_across_detectors() {
    let files = rust_combined_files();
    let aggregated = run_pipeline(&files);
    assert!(
        !aggregated.is_empty(),
        "fixture must produce at least one convention",
    );
    assert_no_duplicate_evidence(&aggregated);
}

/// Fix 1: a 5-parameter function emits exactly one parameter-naming
/// evidence row (one per function), not five (one per parameter).
#[test]
fn parameter_naming_emits_one_evidence_per_function() {
    let aggregated = run_pipeline(&[rust_multi_param_fixture()]);
    let param_conv = aggregated
        .iter()
        .find(|a| a.description.contains("Parameter naming"))
        .expect("parameter-naming convention must be emitted");
    let entries_at_function_line: Vec<_> = param_conv
        .evidence
        .iter()
        .filter(|e| e.line == 100)
        .collect();
    assert_eq!(
        entries_at_function_line.len(),
        1,
        "5-param function must produce 1 evidence row, got: {:?}",
        param_conv.evidence,
    );
}

/// Fix 2 + Fix 5b: the chained `tracing_subscriber::fmt().with_env_filter().init()`
/// call collapses to a single evidence row at the chain-start line, AND
/// the underscored package name still classifies as canonical.
#[test]
fn fluent_chain_collapses_to_single_evidence() {
    let aggregated = run_pipeline(&[rust_fluent_chain_fixture()]);
    let tracing_sub = aggregated
        .iter()
        .find(|a| a.description == "Canonical logging library: tracing_subscriber")
        .expect(
            "tracing_subscriber must classify as canonical (hyphen / underscore normalisation)",
        );
    let entries_at_chain_start: Vec<_> = tracing_sub
        .evidence
        .iter()
        .filter(|e| e.line == 20)
        .collect();
    assert_eq!(
        entries_at_chain_start.len(),
        1,
        "fluent-chain must collapse to a single evidence row, got: {:?}",
        tracing_sub.evidence,
    );
}

/// Fix 5: heuristic findings whose subject matches a project-internal
/// name (workspace crate, declared `mod`) must be filtered by the
/// pipeline's Phase 3 pass. Runs against the combined fixture so the
/// internal-name set is the realistic "all crates + all mods" union.
#[test]
fn no_heuristic_findings_for_internal_modules() {
    let aggregated = run_pipeline(&rust_combined_files());
    let internal = rust_combined_internal_names();

    // For every heuristic finding, parse out the subject using the SAME
    // marker-anchored helper the Phase 3 filter uses. An earlier version
    // of this test reimplemented `rsplit_once(": ")`-based parsing and
    // would have silently diverged from production whenever the marker
    // format changed (e.g. extra colon-space pairs in subjects like
    // `(heuristic): foo: subpath`).
    for conv in &aggregated {
        let desc = conv.description.as_str();
        let Some(subject) = seshat_detectors::pipeline::heuristic_subject_package(desc) else {
            continue;
        };
        let head = seshat_core::top_level_module(subject);
        let normalised = head.replace('-', "_");
        assert!(
            !internal.contains(head) && !internal.contains(&normalised),
            "internal name {head:?} must not surface as heuristic finding {desc:?}",
        );
    }
}

/// R6: with the wildcard fallback narrowed to namespaced calls only,
/// `items.par_iter()` no longer over-attributes to rayon. The only
/// rayon evidence in this fixture is the import line itself.
#[test]
fn rayon_canonical_finding_anchors_at_import_line_only() {
    let aggregated = run_pipeline(&[rust_rayon_wildcard_fixture()]);
    let rayon = aggregated
        .iter()
        .find(|a| a.description == "Canonical async runtime library: rayon")
        .expect("rayon must be classified as canonical");
    assert!(
        !rayon.evidence.is_empty(),
        "rayon finding must have evidence",
    );
    let actual_lines: HashSet<usize> = rayon.evidence.iter().map(|e| e.line).collect();
    let expected: HashSet<usize> = [6].into_iter().collect();
    assert_eq!(
        actual_lines, expected,
        "rayon evidence must anchor only at the wildcard import line; got {actual_lines:?}",
    );
}

/// Fix 3: a 3-file project all using `tracing` must produce ONE
/// `Canonical logging library: tracing` finding, not separate ones
/// from `dependency_usage` and `logging_observability` keyed by
/// detector name. Single-file fixtures don't reproduce this — we need
/// at least two files to exercise the bucket-key dedup.
#[test]
fn no_duplicate_canonical_logging_across_multiple_files() {
    let files = vec![
        rust_tracing_file("crates/a/src/lib.rs", 0),
        rust_tracing_file("crates/b/src/lib.rs", 5),
        rust_tracing_file("crates/c/src/lib.rs", 10),
    ];
    let aggregated = run_pipeline(&files);
    let canonical_logging: Vec<&str> = aggregated
        .iter()
        .filter(|a| a.description.starts_with("Canonical logging library:"))
        .map(|a| a.description.as_str())
        .collect();
    assert!(
        !canonical_logging.is_empty(),
        "expected at least one Canonical logging library finding",
    );
    let mut deduped = canonical_logging.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        canonical_logging.len(),
        deduped.len(),
        "no two canonical-logging findings may share a description (Fix 3): {canonical_logging:?}",
    );
}

/// Fix 6: a Convention-nature finding with empty evidence is exactly
/// the bug class addressed by the EvidenceState gate. Allowing it
/// would let regressions pass silently.
///
/// Legitimate evidence shapes (matching `aggregate_findings`):
/// - All rows anchored at line > 0 (call sites, import lines, derive
///   macros) — no file-level signal accumulated.
/// - All rows line == 0 — pure file-level convention (e.g. naming).
/// - Anchored rows followed by exactly ONE file-level row at the tail
///   — the aggregator pushes a single composite summary AFTER any
///   anchored evidence when both kinds exist for the same convention.
///   That tail row carries `line == 0` and either an empty `file`
///   (synthetic composite from `build_file_level_composite`) or a
///   real path (singleton pass-through with a per-file descriptor).
///
/// Empty evidence, OR an interleaved mix (a line-0 row sitting BEFORE
/// any line-`>`0 row), fails the assertion.
#[test]
fn convention_findings_have_anchored_or_file_level_evidence() {
    let aggregated = run_pipeline(&rust_combined_files());
    for conv in &aggregated {
        if conv.nature != KnowledgeNature::Convention {
            continue;
        }
        assert!(
            !conv.evidence.is_empty(),
            "convention {:?} has empty evidence (Fix 6 regression)",
            conv.description,
        );
        // The first non-anchored row (if any) must be at the tail and
        // must be the only such row — i.e. anchored prefix, then 0 or
        // 1 file-level rows.
        let first_file_level = conv.evidence.iter().position(|e| e.line == 0);
        if let Some(idx) = first_file_level {
            let trailing_file_level: usize =
                conv.evidence[idx..].iter().filter(|e| e.line == 0).count();
            let interleaved_anchored: usize =
                conv.evidence[idx..].iter().filter(|e| e.line > 0).count();
            assert!(
                trailing_file_level == 1 && interleaved_anchored == 0,
                "convention {:?} has malformed evidence shape \
                 (expected: anchored prefix then ≤1 file-level tail): {:?}",
                conv.description,
                conv.evidence,
            );
        }
    }
}

// ===========================================================================
// Tests — Python
// ===========================================================================

/// Helper: extract the `top_level_module`-equivalent head of a
/// heuristic finding's subject, matching the Phase 3 filter logic.
///
/// Uses the production marker-anchored parser
/// `seshat_detectors::pipeline::heuristic_subject_package` so the test
/// extraction can never drift from prod. Two implementations would
/// silently disagree the moment description format changes; one shared
/// implementation cannot.
fn heuristic_subject_head(desc: &str) -> Option<&str> {
    let subject = seshat_detectors::pipeline::heuristic_subject_package(desc)?;
    Some(seshat_core::top_level_module(subject))
}

/// Fix D #1: Python stdlib modules (`traceback`, `logging.config`,
/// `unittest.mock`, `inspect`, …) imported in a Python file must NOT
/// produce `(heuristic)` findings — `is_python_stdlib_module` short-
/// circuits both the logging detector's `is_heuristic_logging_name`
/// and the dependency_usage detector's `classify_heuristic_domain`
/// (where `traceback`'s `trace` substring would otherwise match the
/// "logging" keyword bucket).
///
/// Pre-fix walt-chat-backend surfaced
/// `Likely logging library (heuristic): traceback` and
/// `Testing-related dependency (heuristic): unittest.mock`.
#[test]
fn python_stdlib_imports_dont_produce_heuristic_findings() {
    let aggregated = run_pipeline(&python_combined_files());
    // Each entry MUST appear as an import in `python_combined_files`,
    // and each one MUST trip a heuristic classifier without the
    // stdlib gate — otherwise the assertion is vacuous (passes even
    // if the gate is removed). See the fixture for the trip rationale
    // per module:
    //   - traceback     — `trace` keyword (logging)
    //   - logging.config — `log` substring (logging)
    //   - unittest.mock — `test`/`mock` keywords (testing)
    //   - argparse      — `arg` keyword (cli)
    let stdlib_subjects = ["traceback", "logging.config", "unittest.mock", "argparse"];
    for conv in &aggregated {
        let desc = conv.description.as_str();
        let Some(subject) = seshat_detectors::pipeline::heuristic_subject_package(desc) else {
            continue;
        };
        for stdlib in stdlib_subjects {
            assert!(
                subject != stdlib,
                "Python stdlib module {stdlib:?} must not surface as heuristic finding {desc:?}",
            );
        }
    }
}

/// Fix D #2: a Python file at `tests/test_api.py` doing
/// `from tests.helpers import X` must NOT trigger
/// `(heuristic): tests` — `tests` is a flat-layout internal package
/// (top-level dir, not under `src/`) and the longest-common-prefix
/// harvester adds it to `internal_names`.
///
/// Two-part assertion:
/// 1. Positive control — `tests` IS in the harvested internal-name
///    set. Without this, a regression that broke the harvester (e.g.
///    a future change that drops directory segments) would let the
///    second assertion pass vacuously: no heuristic for `tests`
///    because none ever fires upstream.
/// 2. Filter assertion — no heuristic finding has `tests` as its
///    head package in the aggregated pipeline output.
#[test]
fn python_flat_layout_internal_packages_filtered() {
    let files = python_combined_files();

    // (1) Harvester must capture `tests` as internal.
    let context = seshat_detectors::ProjectContext::from_files(&files);
    assert!(
        context.internal_names.contains("tests"),
        "positive control failed: flat-layout harvester must mark 'tests' as \
         internal; got internal_names = {:?}",
        context.internal_names,
    );

    // (2) Phase 3 filter must drop heuristic findings whose head is `tests`.
    let aggregated = run_pipeline(&files);
    for conv in &aggregated {
        let desc = conv.description.as_str();
        let Some(head) = heuristic_subject_head(desc) else {
            continue;
        };
        assert!(
            head != "tests",
            "flat-layout internal package 'tests' must not surface as heuristic finding {desc:?}",
        );
    }
}

/// Fix D #3: a Python file at `tests/test_utils.py` makes
/// `test_utils` an importable top-level module via the
/// flat-layout file-stem harvester. A sibling file doing
/// `from test_utils import X` must NOT trigger
/// `(heuristic): test_utils`.
///
/// Two-part assertion (see `python_flat_layout_internal_packages_filtered`
/// for the positive-control rationale).
#[test]
fn python_file_stem_internal_names_filtered() {
    let files = python_combined_files();

    let context = seshat_detectors::ProjectContext::from_files(&files);
    assert!(
        context.internal_names.contains("test_utils"),
        "positive control failed: file-stem harvester must mark 'test_utils' \
         as internal; got internal_names = {:?}",
        context.internal_names,
    );

    let aggregated = run_pipeline(&files);
    for conv in &aggregated {
        let desc = conv.description.as_str();
        let Some(head) = heuristic_subject_head(desc) else {
            continue;
        };
        assert!(
            head != "test_utils",
            "file-stem internal name 'test_utils' must not surface as heuristic finding {desc:?}",
        );
    }
}
