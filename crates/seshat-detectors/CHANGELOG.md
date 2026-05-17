# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/KSDaemon/seshat/compare/seshat-detectors-v0.2.1...seshat-detectors-v0.3.0) - 2026-05-17

### <!-- 1 -->Bug Fixes

- *(detectors)* post-review polish
- *(detectors)* collapse per-file naming-percentage buckets into language-wide ones
- *(detectors)* composite header verb reflects bucket adoption ratio
- *(detectors)* merge wrapper-facade convention and violators into one bucket
- *(detectors)* collapse non-conforming file-naming under the dominant convention
- *(detectors)* enrich cross-file findings with source snippets

### <!-- 5 -->Documentation

- *(detectors)* drop historical framing from comments

### <!-- 6 -->Tests

- *(detectors)* pin enrichment + naming description contracts

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-detectors-v0.1.1...seshat-detectors-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- [US-006] Add end_line to Export and TypeDef IR structs (schema v8)
- *(detectors)* collapse multi-file file-level evidence into one composite row
- *(core)* add FindingKind + AnchorKind enums for structured dispatch
- *(detectors/import_organization)* show group order as snippet header
- US-007 - Integration verification with real detectors
- US-004 - Populate macro call snippets in usage_evidence
- US-003 - Include leading context when filling empty snippets
- US-002 - Stop overwriting pre-populated snippets in detect_with_source
- US-001 - Add snippet_start_line field to CodeEvidence
- [US-008] - Wire into detect_with_source in trait_def.rs
- [US-007] - Integrate into dependency_usage detector
- [US-006] - Integrate into error_handling detector
- [US-005] - Integrate into test_patterns detector
- [US-004] - Integrate into logging_observability detector
- [US-003] - Extend find_usage_evidence for all 4 languages
- [US-001, US-002] - Create usage_evidence.rs utility module with tests
- tui review wizard
- *(call-sites)* extend call-site collection to TypeScript, JavaScript, and Python (IR v7)
- *(naming)* file naming evidence snippet shows stem+case for AI context
- *(ir)* ModDeclaration/MacroCall in RustIR + call-site evidence for conventions
- *(detectors)* Phase 2 — real source snippets in convention evidence
- *(ir)* add doc_comment to Function/TypeDef and file_doc to ProjectFile
- [US-013] - Branch code review and quality gate
- [US-012] - Fix dead code: use JavaScriptIR module_system in export detector
- [US-011] - Heuristic fallbacks for testing and dependency usage detectors
- [US-010] - Heuristic fallbacks for error handling and logging detectors
- [US-009] - Parameter naming analysis in naming detector
- [US-008] - Add function parameter extraction to all 4 Tree-sitter parsers
- [US-007] - Wrapper/facade convention detection via import graph
- [US-006] - Convention trend computation with P90 percentile
- [US-001] - Unify DependencyDomain taxonomy in seshat-core
- [US-011] - Branch code review and quality gate
- US-009 - File structure detector — directory organization patterns
- [US-008] - Test patterns detector — framework, file placement, naming conventions
- US-007 - Logging and observability detector — library and structured vs unstructured preference
- [US-006] - Export patterns detector — default vs named, barrel exports, pub/mod
- [US-005] - Naming conventions detector
- [US-004] - Error handling detector — error types, propagation, wrapping
- US-003 - Import organization detector — grouping and ordering patterns
- [US-001] ConventionDetector trait and detection pipeline with confidence scoring
- US-010 - Python fixture project with known conventions
- [US-009] - TypeScript fixture project with known conventions
- US-008 - Rust fixture project with known conventions
- scaffold Rust workspace with 9 crates

### <!-- 1 -->Bug Fixes

- *(docs)* resolve broken and private intra-doc links
- *(detectors/confidence)* disclose marker-only status in all-markers composite header
- *(detectors/pipeline)* harvest from top for single-file Python projects
- *(detectors/pipeline)* make strip_path_prefix separator-agnostic for Windows paths
- *(detectors/confidence)* collapse whitespace-only singleton FileLevel snippets
- *(detectors/confidence)* correct composite header & truncation in all-markers fallback
- *(detectors/pipeline)* denylist vendored / build / cache dirs in Python harvester
- *(detectors/pipeline)* segment-aware Python project root + safer prefix strip
- *(detectors)* Python stdlib heuristic skip + flat-layout root prefix correctness
- *(detectors/file_structure)* suppress "Uses 'tests/' directory" — owned by test_patterns
- *(detectors/confidence)* collapse single-file FileLevel evidence with empty snippet into composite
- *(detectors/confidence)* drop Python __init__.py markers from composite file sample
- *(detectors/confidence)* composite descriptor takes only first line of multi-line snippets
- *(detectors/confidence)* smart-sample composite snippet (cap 20, round-robin across path subtrees)
- *(detectors/file_structure)* drop redundant path duplication in composite descriptors
- *(detectors,core)* extend Python heuristic filtering — stdlib skip, flat-layout root harvesting, file-stem internal names
- *(detectors/test_patterns)* word-boundary check for testing keyword heuristic
- *(detectors/test_patterns)* set Heuristic kind on heuristic findings; rename framework finding to avoid duplicate-look with canonical lib
- *(detectors)* narrow wildcard fallback to namespaced calls only
- *(detectors)* segment_after handles Windows separators + walks every marker; heuristic subject anchored on marker
- *(detectors)* classify_rust_logging accepts both hyphen and underscore package spellings
- *(detectors)* make internal-name extractor work on absolute paths
- *(detectors)* consolidate import-grouping convention descriptions
- *(detectors)* drop dep findings without an anchor; fall back to import line; recognise ["*"] wildcards
- *(detectors)* drop heuristic findings whose subject is a project-internal module
- *(detectors)* match wildcard prelude imports for method/free calls
- *(detectors)* make dependency_usage the sole source of "Canonical X library" findings
- *(detectors)* collapse fluent-chain evidence overlap in find_usage_evidence
- *(detectors)* dedup evidence in naming detectors and aggregate_findings
- *(detectors,cli)* snippet quality round 1 — TUI, FQN matching, heuristic word boundary, debug command
- *(detectors)* improve evidence snippets for serde and test frameworks
- *(detectors)* fix edge cases and review comments
- *(review)* address BMAD code review findings (P-1 through P-6)
- *(snippets)* populate real multi-line source snippets in convention evidence

### <!-- 2 -->Performance

- *(detectors/usage_evidence)* single-pass matches_import for namespaced calls
- *(detectors/confidence)* drop redundant group-vec allocation in select_from_pool
- *(detectors/confidence)* borrow group keys instead of allocating String per row
- *(detectors/pipeline)* segment_after streams windows, no Vec allocation
- *(detectors/logging_observability)* guard hyphen→underscore alloc in classify_rust_logging
- *(detectors)* O(1) evidence dedup via parallel HashSet in aggregate_findings

### <!-- 4 -->Refactor

- *(core,detectors)* hoist word-boundary keyword scan into seshat-core
- *(detectors,graph,cli)* introduce ProjectContext, compute internal-name set once
- *(detectors)* set proper AnchorKind on emit sites; trait_def policy uses anchor enum
- *(detectors)* migrate emit sites to proper FindingKind; pipeline filter switches to enum dispatch
- *(detectors)* name layout markers as language conventions, document audit
- *(detectors)* collapse logging_observability per-language paths + ASCII-safe heuristic
- *(core,detectors)* hoist `top_level_module` helper into seshat-core
- *(detectors)* canonical internal-name set, pub(crate), conditional Rust keywords
- *(detectors)* naming.rs MAX_EVIDENCE const + HashSet via use
- *(graph,detectors)* clean up dependency detection and project context output
- unify duplicate package-to-domain classification into seshat-core

### <!-- 5 -->Documentation

- *(detectors/pipeline)* mark heuristic_subject_package as test-only API

### <!-- 6 -->Tests

- *(detectors)* replace brittle string-equality assertions with shape matches
- *(detectors)* add positive control to Python internal-name filter tests
- *(detectors)* replace `inspect` tripwire with `argparse` in stdlib gate test
- *(detectors)* document anchored-prefix + file-level-tail evidence shape
- *(detectors)* align snippet_quality.rs with production heuristic-subject parser
- *(detectors)* split snippet_quality fixture per concern + add Python e2e coverage
- *(detectors)* tighten under-asserting tests in snippet-quality suite
- *(detectors)* add snippet-quality e2e regression suite
- cover detect_cross_file default, detect_cursor_targets, run_claude_mcp_remove
