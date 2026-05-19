# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/KSDaemon/seshat/compare/seshat-scanner-v0.3.1...seshat-scanner-v0.3.2) - 2026-05-19

### <!-- 0 -->Features

- Orchestrator writes workspace_crates per-branch
- Implement parse_pnpm_workspace_yaml() for pnpm monorepos
- Implement extract_js_package_names() in manifest.rs
- Add js_monorepo test fixture
- Wire expand_glob_member() into extract_crate_names() and update tests
- Add expand_glob_member() helper with unit tests

### <!-- 1 -->Bug Fixes

- harden JS/TS workspace extraction against unsafe patterns and lenient parsing
- *(scanner)* post-review hardening of glob workspace-member expansion

## [0.3.1](https://github.com/KSDaemon/seshat/compare/seshat-scanner-v0.2.1...seshat-scanner-v0.3.1) - 2026-05-17

### <!-- 0 -->Features

- Adversarial code review pass + cleanup
- Populate symbol index during full scan

### <!-- 1 -->Bug Fixes

- address second adversarial review findings

## [0.3.0](https://github.com/KSDaemon/seshat/compare/seshat-scanner-v0.2.1...seshat-scanner-v0.3.0) - 2026-05-17

### <!-- 0 -->Features

- Adversarial code review pass + cleanup
- Populate symbol index during full scan

### <!-- 1 -->Bug Fixes

- address second adversarial review findings

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-scanner-v0.1.1...seshat-scanner-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- Add end_line to Export and TypeDef IR structs (schema v8)
- HEAD-change detection in run_serve
- Wire last_scanned_commit updates in scan paths
- BranchRepository extensions
- Add manifest parsing tests
- Persist internal names to repo_metadata in orchestrator
- Extract Python package names from pyproject.toml in manifest.rs
- Extract Rust crate names from Cargo.toml in manifest.rs
- Comprehensive fix for the seshat review TUI review wizard: UI layout matching design spec, left-right example navigation, convention dedup via description hash, rich summary with total/pending/precision/coverage, non-blocking event loop to prevent hang on exit, consistent branch ID, and snapshot hash for reject concurrency.
- wire detected branch into serve flow and add branch snapshot on switch
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
- ScanProgress submodule variants + get_submodule_commit_hash()
- Submodule DB path resolution + ScanConfig field rename
- Scan report — Project Overview section
- Basic seshat scan command with clap and two-phase progress
- Branch code review and quality gate
- Add function parameter extraction to all 4 Tree-sitter parsers
- Git file dates collection with gix
- Category-to-DependencyDomain mapping rules and three-tier lookup
- Implement CratesIo, Npm, and PyPI registry clients
- Add package_metadata SQLite table and PackageRegistryClient trait
- Unify DependencyDomain taxonomy in seshat-core
- Branch code review and quality gate
- Incremental re-scan support
- Scan orchestration — initial full scan
- Documentation ingestion
- Module structure and dependency graph
- Dependency manifest analysis
- Tree-sitter parsing for Python
- Tree-sitter parsing for JavaScript
- Tree-sitter parsing for TypeScript
- Tree-sitter parsing for Rust
- File discovery with .gitignore respect
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
