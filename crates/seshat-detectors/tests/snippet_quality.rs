//! End-to-end snippet-quality regression test.
//!
//! Builds a synthetic mini "project" of [`ProjectFile`]s exercising every
//! bug class fixed in the snippet-quality series, runs the full detector
//! pipeline + aggregator, and asserts whole-pipeline invariants on the
//! resulting convention set.
//!
//! The fixture is constructed in-memory rather than parsed from disk —
//! seshat-detectors does not depend on the parser crate, and parsing is
//! covered by other integration suites. What this test guards is the
//! interaction between detectors, the wildcard / FQN matching in
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
    LanguageIR, MacroCall, ModDeclaration, ProjectFile, RustIR,
};
use seshat_detectors::aggregate_findings;
use seshat_detectors::pipeline::run_all_detectors;

/// Names this fixture treats as "internal" — the fixture's own
/// workspace-crate name plus its declared `mod` blocks.  Used to
/// validate that the heuristic-noise filter drops findings whose
/// subject is one of these without hard-coding the same string twice.
///
/// Mirrors `pipeline::compute_internal_package_names` exactly:
///   - Rust workspace crates are stored in canonical (underscored) form;
///     the assertion below normalises hyphens on lookup.
///   - Rust path keywords (`crate`/`super`/`self`) belong because the
///     fixture has Rust files.
fn internal_names_in_fixture() -> HashSet<String> {
    let mut s = HashSet::new();
    // Workspace crate harvested from `crates/seshat-cli/...` path.
    s.insert("seshat_cli".to_owned());
    // mod declarations from cli_lib().
    s.insert("args".to_owned());
    s.insert("db".to_owned());
    // Rust path keywords (project has Rust files).
    s.insert("crate".to_owned());
    s.insert("super".to_owned());
    s.insert("self".to_owned());
    s
}

/// Build a Rust file at `crates/seshat-cli/src/lib.rs` that exercises
/// several bug classes at once:
///
/// - `use tracing_subscriber::EnvFilter` + a fluent chain → Fix 2
///   (chain collapsed to one evidence) AND Fix 5b (underscored package
///   recognised as canonical, no heuristic).
/// - `pub mod args; pub mod db;` declarations → Fix 5 (heuristic findings
///   for the internal `args` / `db` module names must be filtered).
/// - `use rayon::prelude::*` + a `vec.par_iter()` call → Fix 4 (wildcard
///   prelude attributes the call to rayon).
fn cli_lib() -> ProjectFile {
    let path = PathBuf::from("crates/seshat-cli/src/lib.rs");
    let imports = vec![
        Import {
            module: "tracing_subscriber".to_owned(),
            names: vec!["EnvFilter".to_owned()],
            is_type_only: false,
            line: 5,
        },
        Import {
            module: "rayon::prelude".to_owned(),
            names: vec!["*".to_owned()],
            is_type_only: false,
            line: 6,
        },
        Import {
            module: "args".to_owned(),
            names: vec!["Cli".to_owned()],
            is_type_only: false,
            line: 7,
        },
    ];
    let dependencies_used = vec![
        DependencyUsage {
            package: "tracing_subscriber".to_owned(),
            import_path: "tracing_subscriber::EnvFilter".to_owned(),
            line: 5,
        },
        DependencyUsage {
            package: "rayon".to_owned(),
            import_path: "rayon::prelude".to_owned(),
            line: 6,
        },
        DependencyUsage {
            package: "args".to_owned(),
            import_path: "args".to_owned(),
            line: 7,
        },
    ];
    // Mod declarations harvested by the cross-file internal-name filter.
    let mod_declarations = vec![
        ModDeclaration {
            name: "args".to_owned(),
            line: 1,
        },
        ModDeclaration {
            name: "db".to_owned(),
            line: 2,
        },
    ];
    // The fluent chain: each chained method appears as its own
    // FunctionCall with the same start `line`. Fix 2 collapses these.
    let function_calls = vec![
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
        // rayon prelude usage: receiver `items` is not in any import's
        // names, but the wildcard fallback attributes it.
        FunctionCall {
            callee: "items.par_iter".to_owned(),
            line: 50,
            end_line: 50,
            snippet: "items.par_iter().for_each(...)".to_owned(),
        },
    ];
    let macro_calls = Vec::<MacroCall>::new();

    ProjectFile {
        path,
        language: Language::Rust,
        content_hash: String::new(),
        imports,
        exports: Vec::new(),
        // A multi-parameter function — Fix 1 must collapse N evidence
        // rows down to one per function, not one per parameter.
        functions: vec![Function {
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
        }],
        types: Vec::new(),
        dependencies_used,
        language_ir: LanguageIR::Rust(RustIR {
            mod_declarations,
            derive_macros: Vec::new(),
            trait_implementations: Vec::new(),
            error_types: Vec::new(),
            macro_calls,
            function_calls,
        }),
        file_doc: None,
    }
}

/// Build a Rust file at the given path that uses `tracing`.
///
/// Used by the multi-file fixture below to reproduce the original
/// "two convention nodes for the same library" bug class — where
/// `dependency_usage` and `logging_observability` both emitted
/// "Canonical logging library: tracing" and the aggregator keyed by
/// `(detector_name, description)` kept them separate.  The bug is
/// only observable when at least two files trigger both detectors.
fn tracing_file(path: &str, line_offset: usize) -> ProjectFile {
    let info_line = 10 + line_offset;
    ProjectFile {
        path: PathBuf::from(path),
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
        dependencies_used: vec![DependencyUsage {
            package: "tracing".to_owned(),
            import_path: "tracing".to_owned(),
            line: 1,
        }],
        language_ir: LanguageIR::Rust(RustIR {
            mod_declarations: Vec::new(),
            derive_macros: Vec::new(),
            trait_implementations: Vec::new(),
            error_types: Vec::new(),
            macro_calls: vec![MacroCall {
                name: "info".to_owned(),
                line: info_line,
            }],
            function_calls: Vec::new(),
        }),
        file_doc: None,
    }
}

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

#[test]
fn no_duplicate_evidence_across_detectors() {
    let aggregated = run_pipeline(&[cli_lib()]);
    assert!(
        !aggregated.is_empty(),
        "fixture must produce at least one convention"
    );
    assert_no_duplicate_evidence(&aggregated);
}

#[test]
fn parameter_naming_emits_one_evidence_per_function() {
    let aggregated = run_pipeline(&[cli_lib()]);
    let param_conv = aggregated
        .iter()
        .find(|a| a.description.contains("Parameter naming"))
        .expect("parameter-naming convention must be emitted");
    // The fixture function has 5 parameters at the same line.
    // Pre-Fix-1 this produced 5 identical evidence rows; now it
    // collapses to a single row per function.
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

#[test]
fn fluent_chain_collapses_to_single_evidence() {
    let aggregated = run_pipeline(&[cli_lib()]);
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

#[test]
fn no_heuristic_findings_for_internal_modules() {
    let aggregated = run_pipeline(&[cli_lib()]);
    let internal = internal_names_in_fixture();

    // For every heuristic finding, parse out the subject (text after the
    // last ": ") and assert it is NOT in the fixture's internal-name set.
    // This replaces the previous hard-coded substring filter, so a future
    // fixture change that adds a `mod foo;` declaration is automatically
    // covered.
    for conv in &aggregated {
        let desc = conv.description.as_str();
        let is_heuristic = desc.contains("(heuristic)") || desc.contains("(name heuristic)");
        if !is_heuristic {
            continue;
        }
        let Some((_, subject)) = desc.rsplit_once(": ") else {
            continue;
        };
        // Match the pipeline's package-internal check: leading segment by
        // "::" or ".", normalised on hyphens.
        let head = subject
            .split("::")
            .next()
            .unwrap_or(subject)
            .split('.')
            .next()
            .unwrap_or(subject);
        let normalised = head.replace('-', "_");
        assert!(
            !internal.contains(head) && !internal.contains(&normalised),
            "internal name {head:?} must not surface as heuristic finding {desc:?}",
        );
    }
}

#[test]
fn rayon_canonical_finding_anchors_at_import_line_only() {
    // After R6 narrowed the wildcard fallback to namespaced calls only,
    // rayon has no callable anchor in this fixture — the only rayon-
    // related signal is `use rayon::prelude::*` at line 6, picked up
    // via dependency_usage's import-line fallback.  Line 50
    // (`items.par_iter()`) and line 20 (over-attributed
    // `tracing_subscriber::fmt`) MUST NOT appear: the former needs a
    // real receiver-name match, the latter was a regression class.
    let aggregated = run_pipeline(&[cli_lib()]);
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

/// Multi-file fixture: three Rust files all use `tracing`. Reproduces the
/// pre-Fix-3 bug class where `dependency_usage` and `logging_observability`
/// each emitted "Canonical logging library: tracing" and the aggregator
/// kept the two as separate convention nodes (because `(detector_name,
/// description)` is the bucket key). A single-file fixture cannot trigger
/// this because one finding per detector per file collapses trivially.
#[test]
fn no_duplicate_canonical_logging_across_multiple_files() {
    let files = vec![
        tracing_file("crates/a/src/lib.rs", 0),
        tracing_file("crates/b/src/lib.rs", 5),
        tracing_file("crates/c/src/lib.rs", 10),
    ];
    let aggregated = run_pipeline(&files);
    let canonical_logging: Vec<&str> = aggregated
        .iter()
        .filter(|a| a.description.starts_with("Canonical logging library:"))
        .map(|a| a.description.as_str())
        .collect();
    assert!(
        !canonical_logging.is_empty(),
        "expected at least one Canonical logging library finding"
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

#[test]
fn convention_findings_have_anchored_or_file_level_evidence() {
    // Tightened post-review: a Convention nature with empty evidence is
    // exactly the bug Fix 6 closes.  Allowing it would let a regression
    // pass silently.  Legitimate cases:
    //   - All evidence rows are anchored at line > 0 (call sites,
    //     import lines, derive macros).
    //   - All evidence rows are line == 0 (file-level signals such as
    //     "File naming: snake_case convention").
    // A mix, or empty evidence, fails the assertion.
    let aggregated = run_pipeline(&[cli_lib()]);
    for conv in &aggregated {
        if conv.nature != KnowledgeNature::Convention {
            continue;
        }
        assert!(
            !conv.evidence.is_empty(),
            "convention {:?} has empty evidence (Fix 6 regression)",
            conv.description,
        );
        let all_anchored = conv.evidence.iter().all(|e| e.line > 0);
        let all_file_level = conv.evidence.iter().all(|e| e.line == 0);
        assert!(
            all_anchored || all_file_level,
            "convention {:?} mixes anchored and file-level evidence: {:?}",
            conv.description,
            conv.evidence,
        );
    }
}
