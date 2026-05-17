# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/KSDaemon/seshat/compare/seshat-scanner-v0.2.1...seshat-scanner-v0.3.0) - 2026-05-17

### <!-- 0 -->Features

- US-008 - Adversarial code review pass + cleanup
- US-002 - Populate symbol index during full scan

### <!-- 1 -->Bug Fixes

- address second adversarial review findings

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-scanner-v0.1.1...seshat-scanner-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- [US-006] Add end_line to Export and TypeDef IR structs (schema v8)
- US-010 - HEAD-change detection in run_serve
- US-009 - Wire last_scanned_commit updates in scan paths
- US-003 - BranchRepository extensions
- [US-006] - Add manifest parsing tests
- [US-003] - Persist internal names to repo_metadata in orchestrator
- US-002 - Extract Python package names from pyproject.toml in manifest.rs
- US-001 - Extract Rust crate names from Cargo.toml in manifest.rs
- Comprehensive fix for the seshat review TUI review wizard: UI layout matching design spec, left-right example navigation, convention dedup via description hash, rich summary with total/pending/precision/coverage, non-blocking event loop to prevent hang on exit, consistent branch ID, and snapshot hash for reject concurrency.
- US-002 US-003 - wire detected branch into serve flow and add branch snapshot on switch
- implement auto-scan feature
- *(call-sites)* extend call-site collection to TypeScript, JavaScript, and Python (IR v7)
- *(call-sites)* query_code_pattern returns real call-site snippets (IR v6)
- *(ir)* ModDeclaration/MacroCall in RustIR + call-site evidence for conventions
- *(detectors)* Phase 2 — real source snippets in convention evidence
- *(scanner,graph)* populate dependencies_used in all parsers, fix extract_domain_and_package
- *(scanner,graph)* improve module purpose quality
- *(scanner)* derive module purpose from doc comments and symbols
- *(scanner)* extract doc comments from AST in all language parsers
- *(ir)* add doc_comment to Function/TypeDef and file_doc to ProjectFile
- [US-003] - ScanProgress submodule variants + get_submodule_commit_hash()
- [US-002] - Submodule DB path resolution + ScanConfig field rename
- [US-003] - Scan report — Project Overview section
- [US-001] - Basic seshat scan command with clap and two-phase progress
- [US-013] - Branch code review and quality gate
- [US-008] - Add function parameter extraction to all 4 Tree-sitter parsers
- [US-005] - Git file dates collection with gix
- [US-004] - Category-to-DependencyDomain mapping rules and three-tier lookup
- [US-003] - Implement CratesIo, Npm, and PyPI registry clients
- [US-002] - Add package_metadata SQLite table and PackageRegistryClient trait
- [US-001] - Unify DependencyDomain taxonomy in seshat-core
- [US-011] - Branch code review and quality gate
- [US-012] - Incremental re-scan support
- [US-011] - Scan orchestration — initial full scan
- [US-009] - Documentation ingestion
- [US-008] - Module structure and dependency graph
- US-007 - Dependency manifest analysis
- [US-006] - Tree-sitter parsing for Python
- [US-005] - Tree-sitter parsing for JavaScript
- [US-004] - Tree-sitter parsing for TypeScript
- US-003 - Tree-sitter parsing for Rust
- US-002 - File discovery with .gitignore respect
- scaffold Rust workspace with 9 crates

### <!-- 1 -->Bug Fixes

- *(docs)* resolve broken and private intra-doc links
- *(scanner)* collapse nested if into match guards (clippy)
- *(scanner)* store files_ir paths relative to project_root (Bug #3)
- *(sync)* propagate source_map through incremental detection (Bug #2)
- address KSD code review findings for manifest parsing
- fix lint warnings and additional edge case fixes
- *(review)* address BMAD code review findings (P-1 through P-6)
- *(snippets)* populate real multi-line source snippets in convention evidence
- *(scanner)* use relative path for git_dates lookup in upsert loop
- *(scanner)* parse_markdown produces one node per H1/H2 section
- *(scanner)* rewrite discover_documentation to use WalkBuilder + gitignore

### <!-- 3 -->Dependencies

- *(deps)* bump gix 0.72 → 0.83
- *(deps)* update sha2 0.10→0.11
- *(deps)* replace deprecated serde_yaml with serde_yml 0.0.11

### <!-- 4 -->Refactor

- *(scanner)* move sentinel write into orchestrator (P19, P21)
- *(scanner,graph,core)* code review fixes — dedup deps, unify comment cleanup, fix noise filter
- *(graph,detectors)* clean up dependency detection and project context output
- *(config)* rename exclude_patterns to exclude_paths in ScanConfig
- extract shared RegistryHttpClient from copy-pasted registry clients
- unify duplicate package-to-domain classification into seshat-core
- idiomatic Rust cleanup and dependency updates

### <!-- 6 -->Tests

- pin `git init -b main` in repo-bootstrap helpers
- improve code coverage across multiple modules (Phases 1-3)
