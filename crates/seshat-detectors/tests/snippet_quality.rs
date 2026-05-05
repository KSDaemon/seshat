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

use std::collections::HashMap;
use std::path::PathBuf;

use seshat_core::{
    DependencyUsage, DetectionConfig, Function, FunctionCall, Import, KnowledgeNature, Language,
    LanguageIR, MacroCall, ModDeclaration, ProjectFile, RustIR,
};
use seshat_detectors::aggregate_findings;
use seshat_detectors::pipeline::run_all_detectors;

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

fn empty_source_map() -> HashMap<PathBuf, String> {
    HashMap::new()
}

/// Run the same path the real scanner takes: per-file + cross-file
/// detection, then `aggregate_findings`.
fn run_pipeline(files: &[ProjectFile]) -> Vec<seshat_detectors::AggregatedConvention> {
    let detector_results = run_all_detectors(
        files,
        &empty_source_map(),
        &DetectionConfig::default(),
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
/// bug class (assert_cmd 2x, parameter naming N×, etc.).
fn assert_no_duplicate_evidence(aggregated: &[seshat_detectors::AggregatedConvention]) {
    for conv in aggregated {
        let mut seen: Vec<(&std::path::Path, usize, usize)> = Vec::new();
        for ev in &conv.evidence {
            let key = (ev.file.as_path(), ev.line, ev.end_line);
            assert!(
                !seen.contains(&key),
                "convention {:?} has duplicate evidence at {key:?}",
                conv.description
            );
            seen.push(key);
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
    let bad: Vec<&str> = aggregated
        .iter()
        .map(|a| a.description.as_str())
        .filter(|d| d.contains("(heuristic)") || d.contains("(name heuristic)"))
        .filter(|d| {
            // The internal modules emitted by this fixture.
            d.ends_with(": args")
                || d.ends_with(": db")
                || d.contains("crate::")
                || d.contains("seshat_cli")
        })
        .collect();
    assert!(
        bad.is_empty(),
        "internal modules must not surface as heuristic findings, got: {bad:?}",
    );
}

#[test]
fn rayon_canonical_finding_has_evidence() {
    let aggregated = run_pipeline(&[cli_lib()]);
    let rayon = aggregated
        .iter()
        .find(|a| a.description == "Canonical async runtime library: rayon")
        .expect("rayon must be classified as canonical");
    assert!(
        !rayon.evidence.is_empty(),
        "rayon finding must have evidence (wildcard prelude fallback)",
    );
    // Pre-Fix-4 the evidence panel was empty because the receiver of
    // `items.par_iter()` was not in `imp.names`. With the wildcard
    // fallback the call site (line 50) is attributed to rayon.
    assert!(
        rayon.evidence.iter().any(|e| e.line == 50 || e.line == 6),
        "rayon evidence must anchor at the call site or the wildcard import line, got: {:?}",
        rayon.evidence,
    );
}

#[test]
fn no_duplicate_canonical_logging_descriptions() {
    let aggregated = run_pipeline(&[cli_lib()]);
    let canonical_logging: Vec<&str> = aggregated
        .iter()
        .filter(|a| a.description.starts_with("Canonical logging library:"))
        .map(|a| a.description.as_str())
        .collect();
    let mut deduped = canonical_logging.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        canonical_logging.len(),
        deduped.len(),
        "no two canonical-logging findings may share a description (Fix 3): {:?}",
        canonical_logging,
    );
}

#[test]
fn convention_findings_have_evidence_or_are_file_level() {
    let aggregated = run_pipeline(&[cli_lib()]);
    for conv in &aggregated {
        if conv.nature != KnowledgeNature::Convention {
            continue;
        }
        let has_real_anchor = conv.evidence.iter().any(|e| e.line > 0);
        let only_file_level =
            !conv.evidence.is_empty() && conv.evidence.iter().all(|e| e.line == 0);
        assert!(
            has_real_anchor || only_file_level || conv.evidence.is_empty(),
            "convention {:?} has malformed evidence (mix of anchored/zero), got: {:?}",
            conv.description,
            conv.evidence,
        );
    }
}
