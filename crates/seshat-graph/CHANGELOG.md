# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/KSDaemon/seshat/compare/seshat-graph-v0.3.1...seshat-graph-v0.3.2) - 2026-05-19

### <!-- 0 -->Features

- Graph reads workspace_crates per-branch

### <!-- 1 -->Bug Fixes

- *(graph)* fold backslash separators in normalize_path for Windows

## [0.3.1](https://github.com/KSDaemon/seshat/compare/seshat-graph-v0.2.1...seshat-graph-v0.3.1) - 2026-05-17

### <!-- 0 -->Features

- render definition snippets per-language
- Adversarial code review pass + cleanup
- Aggregate call_sites by file in query_code_pattern
- Return blast_radius per symbol match
- Return dependent_files per symbol match in query_code_pattern
- Route symbol matching through symbol_definitions index

### <!-- 1 -->Bug Fixes

- address second adversarial review findings
- *(graph)* harden validate_approach gating + focus_area semantics
- *(graph)* bound validate_approach payload and harden focus_area semantics

### <!-- 5 -->Documentation

- *(graph)* drop intra-doc links to private items

## [0.3.0](https://github.com/KSDaemon/seshat/compare/seshat-graph-v0.2.1...seshat-graph-v0.3.0) - 2026-05-17

### <!-- 0 -->Features

- render definition snippets per-language
- Adversarial code review pass + cleanup
- Aggregate call_sites by file in query_code_pattern
- Return blast_radius per symbol match
- Return dependent_files per symbol match in query_code_pattern
- Route symbol matching through symbol_definitions index

### <!-- 1 -->Bug Fixes

- address second adversarial review findings
- *(graph)* harden validate_approach gating + focus_area semantics
- *(graph)* bound validate_approach payload and harden focus_area semantics

### <!-- 5 -->Documentation

- *(graph)* drop intra-doc links to private items

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-graph-v0.1.1...seshat-graph-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- Add criterion bench for map_diff_impact perf budget
- Surface content-level granularity in map_diff_impact MCP wiring and logs
- Rewrite compute_affected_symbols to hunk-level granularity
- Blob-aware change enumeration for diff_impact
- Add hunk extraction primitive using gix::diff::blob
- Add end_line to Export and TypeDef IR structs (schema v8)
- Add performance guard test for transitive query_dependencies
- Add depth parameter to query_dependencies graph-layer API
- Build cycle-safe BFS core for transitive dependents
- *(graph)* per-state decision counts in query_project_context (S8/FR-27)
- Migrate MCP record/update/remove_decision to DecisionRepository
- Update persist_conventions auto-scan dedup
- *(core)* add FindingKind + AnchorKind enums for structured dispatch
- Add graph resolution tests with dynamic internal names
- Update function signatures in dependencies.rs for dynamic internal names
- Add load_internal_names() and remove hardcoded WORKSPACE_CRATES
- Add edge case and correctness tests
- Add map_diff_impact() orchestrating function
- Add compute_convention_risks()
- Add query_dependencies_batch() and compute_affected_symbols()
- Define DiffImpact types and get_changed_files()
- drop file lists from query_project_context modules
- Integration verification with real detectors
- Update all intermediate structs and construction sites
- Add snippet_start_line field to CodeEvidence
- Comprehensive fix for the seshat review TUI review wizard: UI layout matching design spec, left-right example navigation, convention dedup via description hash, rich summary with total/pending/precision/coverage, non-blocking event loop to prevent hang on exit, consistent branch ID, and snapshot hash for reject concurrency.
- tui review wizard
- *(call-sites)* extend call-site collection to TypeScript, JavaScript, and Python (IR v7)
- *(call-sites)* query_code_pattern returns real call-site snippets (IR v6)
- *(naming)* file naming evidence snippet shows stem+case for AI context
- *(detectors)* Phase 2 — real source snippets in convention evidence
- *(graph)* add detection module with shared convention_to_node and run_detection_cycle
- *(graph)* populate submodules field in query_project_context
- *(scanner,graph)* populate dependencies_used in all parsers, fix extract_domain_and_package
- *(scanner,graph)* improve module purpose quality
- *(ir)* add doc_comment to Function/TypeDef and file_doc to ProjectFile
- *(graph)* ModuleInfo returns name + files + purpose from ext_data
- *(epic8)* replace HTTP embedding providers with built-in fastembed-rs
- Vector search integration into query_code_pattern
- validate_approach graph module with graduated response and evidence gating
- query_dependencies graph module — IR search and blast radius
- query_code_pattern graph module — IR search and scoring
- update_decision and remove_decision MCP tools
- record_decision MCP tool
- query_convention MCP tool
- query_project_context MCP tool
- Golden files computation and per-file convention compliance
- FTS5 migration and full-text search index management
- Branch code review and quality gate
- Cross-reference code conventions vs documentation
- scaffold Rust workspace with 9 crates

### <!-- 1 -->Bug Fixes

- *(graph)* use sort_by_key with Reverse (clippy 1.95)
- *(docs)* resolve broken and private intra-doc links
- *(graph)* honor FR-B6 — Hunk::ALL fallback for Conflicted files
- drop #[serde(default)] on fields newly added in this branch
- *(graph)* diamond `via` tie-break uses lex on joined chain string
- *(graph)* distinguish NotFound from PermissionDenied in read_disk_file_bytes
- *(graph)* propagate BFS truncation flag into DependencyData.truncated
- *(graph)* debug_assert depth bounds in compute_transitive_dependents
- *(mcp)* backwards-compat shim for legacy id/node_id envelope (H3)
- *(graph,mcp)* state-gate update/remove_decision against TUI rows (H2)
- *(graph,storage)* recompute description_hash on update_decision (H1)
- resolve rebase conflicts with main — wire internal_names through query_dependencies_batch and cross-crate tests
- address KSD code review findings for manifest parsing
- filter affected_symbols to only symbols actually imported by name
- prevent crate:: imports from matching files in other crates
- correct total_dependents in blast_radius_summary
- deduplicate affected_symbols by (name, file)
- replace hand-rolled walk_dir with WalkBuilder to respect .gitignore
- update query_dependencies_batch to use LoadedIR API
- *(debt)* move is_finite check before f32 cast in cosine_similarity
- *(debt)* expand STOP_WORDS to include pronouns, auxiliary verbs, and spec-matching entries
- *(debt)* add seshat_bin to WORKSPACE_CRATES
- *(debt)* add source/nature/category fields to DecisionEntry and ObservationEntry
- *(debt)* deterministic SuffixIndex resolution + truncated false positive
- remove redundant file path comments from synthetic snippets in query_code_pattern
- golden files dedup, dependency pollution, FTS5 search, and status branch display
- fix lint warnings and additional edge case fixes
- *(review)* address BMAD code review findings (P-1 through P-6)
- *(query_dependencies)* accept relative paths when IR stores absolute paths
- *(snippets)* populate real multi-line source snippets in convention evidence
- *(graph)* filter query_modules to module_structure nodes only, add DISTINCT
- *(graph/validate_approach)* keyword threshold, contradiction dedup, batch N+1 query
- *(graph/dependencies)* boundary check, is_likely_internal precision, collect all imports
- *(graph/code_pattern)* NaN guard, memory limits, merge fix, name normalization
- eliminate double query_convention call in validate_approach
- eliminate SQL injection and deduplicate keyword search in validate_approach

### <!-- 2 -->Performance

- *(graph)* defer ChangedFileWithBlobs construction in enumerate_changes_with_blobs
- *(graph)* use BTreeSet for per-target import_names dedup in build_reverse_adjacency
- hoist prepared statement out of loop and use HashSet for O(1) dedup in find_contradictions

### <!-- 3 -->Dependencies

- *(deps)* bump gix 0.72 → 0.83

### <!-- 4 -->Refactor

- *(graph)* rename AffectedSymbol.dependent_count to transitive_dependent_count
- *(graph)* wrap compute_affected_symbols return into AffectedSymbolsResult
- *(graph)* demote SuffixIndex and BFS helpers to pub(crate)
- *(mcp,graph)* proper DECISION_NOT_FOUND code, drop H3 legacy id/node_id shim (P35)
- *(graph)* tidy decisions/conventions queries (P7-P16)
- *(detectors,graph,cli)* introduce ProjectContext, compute internal-name set once
- move next_steps generation to MCP handler
- KSD code review fixes — dedup, idioms, bugs, spec compliance, edge cases
- *(mcp)* Phase 1 response cleanup — remove noise, tighten schema
- *(watcher,scan)* eliminate detection pipeline duplication
- *(scanner,graph,core)* code review fixes — dedup deps, unify comment cleanup, fix noise filter
- *(graph,detectors)* clean up dependency detection and project context output
- extract parameterized truncate_snippet_to() to seshat-core
- add GraphError::query() shorthand to reduce error wrapping boilerplate
- extract duplicated test helpers (insert_ir, insert_convention_node)
- improve Rust idioms in dependencies.rs
- use chrono in decisions.rs, fix misleading ISO-8601 comment
- replace manual Hinnant calendar in backup.rs with chrono
- extract handler error helpers + shared test_conn (findings 6,7,9,10)
- deduplicate code across graph/mcp crates (code review findings)

### <!-- 5 -->Documentation

- *(graph)* clarify the "lazy" gix::open comment in compute_affected_symbols
- update Epic 7 spec from code review findings, record deferred items

### <!-- 6 -->Tests

- *(graph)* lock depth=1 ⊆ depth=2 invariant for query_dependencies
- *(graph)* cover the `base = Some(commit)` branch of enumerate_changes_with_blobs
- *(graph)* drop single-shot Instant assert in diff_impact_bench
- *(graph)* add hunks_empty_new_* test for pure-deletion of full file
- *(graph)* decouple transitive_perf from MAX_DEPENDENTS literal
- add coverage for envelope, fts, instructions, and uninstall edge cases
