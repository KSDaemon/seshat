# PRD: Epic 2 — Code Scanning & Knowledge Graph (Stories 2.1–2.9)

## Introduction

**Type:** Feature

Implement the core scanning pipeline: file discovery, Tree-sitter AST parsing for 4 languages (Rust, TypeScript, JavaScript, Python), dependency manifest analysis, module structure detection, documentation ingestion, knowledge graph persistence with incremental re-check, and automatic database backups. This is the data pipeline that produces all intelligence consumed by downstream epics (convention detection, MCP tools, CLI reports).

**Context:** Seshat is a Rust CLI + MCP server. Epic 1 (infrastructure) is complete: 9-crate workspace, core types/IR, SQLite schema, repository CRUD, configuration system. This epic builds the `seshat-scanner` crate and wires it to `seshat-storage` via the knowledge graph. See `_bmad-output/planning-artifacts/architecture.md` for ADRs.

## Goals

- Discover source files respecting .gitignore and custom exclusions (Story 2.1)
- Parse Rust, TypeScript, JavaScript, Python into normalized IR via Tree-sitter (Stories 2.2–2.4)
- Analyze dependency manifests and cross-reference with actual code usage (Story 2.5)
- Detect module structure and build inter-module dependency graph (Story 2.6)
- Ingest documentation files as knowledge graph nodes (Story 2.7)
- Persist IR and knowledge graph in SQLite with incremental re-check on restart (Story 2.8)
- Automatic database backups with configurable retention (Story 2.9)

## User Stories

### US-001: File Discovery & .gitignore Respect (Story 2.1)

**Description:** As a developer, I want Seshat to discover all relevant source files while respecting .gitignore so that only meaningful project files are scanned.

**Acceptance Criteria:**
- [ ] `seshat-scanner` depends on the `ignore` crate for file walking
- [ ] `discover_files(root, config)` function uses `WalkBuilder` for native .gitignore support
- [ ] Files matching .gitignore patterns are excluded
- [ ] `.git/` directory is always excluded
- [ ] Hidden files/directories (starting with `.`) are excluded by default
- [ ] Custom exclude patterns from `ScanConfig.exclude_patterns` are applied
- [ ] Files exceeding `ScanConfig.max_file_size_kb` (default 512KB) are skipped with tracing::warn
- [ ] Returns `Vec<DiscoveredFile>` with path, detected language, and file size
- [ ] Language detection by file extension using `Language::extensions()`
- [ ] Files with unrecognized extensions are skipped
- [ ] Unit tests: gitignore exclusion, hidden file exclusion, size limit, custom patterns
- [ ] cargo test -p seshat-scanner passes

### US-002: Tree-sitter Parsing for Rust (Story 2.2)

**Description:** As a developer, I want Seshat to parse Rust source files into IR so that the knowledge graph has structured understanding of Rust code.

**Acceptance Criteria:**
- [ ] `seshat-scanner` depends on `tree-sitter` and `tree-sitter-rust`
- [ ] `RustParser` implements a `Parser` trait: `fn parse(path, source) -> Result<ProjectFile>`
- [ ] Extracts `imports` from `use_declaration` nodes
- [ ] Extracts `functions` from `function_item` nodes with visibility (pub/private) and async
- [ ] Extracts `types` from `struct_item`, `enum_item`, `trait_item` nodes with visibility
- [ ] Extracts `exports` (pub items)
- [ ] `LanguageIR::Rust` populated: mod_declarations, derive_macros, trait_implementations, error_types (thiserror/anyhow patterns)
- [ ] `content_hash` computed as SHA256 of file content
- [ ] Parsing errors logged as tracing::warn, not panics
- [ ] Unparseable files produce empty IR with error note (graceful degradation)
- [ ] Integration test parses tests/fixtures/rust_project/ and verifies expected IR
- [ ] cargo test -p seshat-scanner passes

### US-003: Tree-sitter Parsing for TypeScript (Story 2.3)

**Description:** As a developer, I want Seshat to parse TypeScript source files into IR so that TypeScript projects are fully understood.

**Acceptance Criteria:**
- [ ] `seshat-scanner` depends on `tree-sitter-typescript`
- [ ] `TypeScriptParser` implements Parser trait
- [ ] Extracts imports: named, default, type-only (import type)
- [ ] Extracts exports: named, default, re-exports (export { } from)
- [ ] Extracts functions, types (interfaces, type aliases, classes)
- [ ] `LanguageIR::TypeScript` populated: has_barrel_exports (index.ts detection), type_only_imports, decorators, default_export
- [ ] `.tsx` files handled without JSX breaking the parse
- [ ] Integration test parses tests/fixtures/typescript_project/
- [ ] cargo test -p seshat-scanner passes

### US-004: Tree-sitter Parsing for JavaScript (Story 2.4a)

**Description:** As a developer, I want Seshat to parse JavaScript files into IR so that JS projects are supported.

**Acceptance Criteria:**
- [ ] `seshat-scanner` depends on `tree-sitter-javascript`
- [ ] `JavaScriptParser` implements Parser trait
- [ ] Handles file extensions: .js, .jsx, .mjs, .cjs
- [ ] Detects CommonJS vs ESM module system
- [ ] Extracts `module.exports` and `require()` calls for CommonJS
- [ ] Extracts import/export statements for ESM
- [ ] `LanguageIR::JavaScript` populated: module_system, has_module_exports, require_calls
- [ ] Integration test with JS fixture files
- [ ] cargo test -p seshat-scanner passes

### US-005: Tree-sitter Parsing for Python (Story 2.4b)

**Description:** As a developer, I want Seshat to parse Python files into IR so that all four MVP languages are supported.

**Acceptance Criteria:**
- [ ] `seshat-scanner` depends on `tree-sitter-python`
- [ ] `PythonParser` implements Parser trait
- [ ] Extracts imports: `import x`, `from x import y`, grouped by stdlib/external/internal
- [ ] Extracts `__all__` exports
- [ ] Detects `__init__.py` as package init file
- [ ] Extracts type hints presence, decorator patterns
- [ ] `LanguageIR::Python` populated: has_all_export, is_init_file, type_hints_used, decorators
- [ ] Integration test with Python fixture files
- [ ] cargo test -p seshat-scanner passes

### US-006: Parser Trait and Language Dispatch (Story 2.2 infra)

**Description:** As a developer, I want a common Parser trait and language dispatch so that adding new languages requires minimal changes.

**Acceptance Criteria:**
- [ ] `Parser` trait defined: `fn parse(&self, path: &Path, source: &str) -> Result<ProjectFile>`
- [ ] `parse_file(path, source, language)` dispatches to correct parser by Language enum
- [ ] SHA256 content hash computed in shared code (not per-parser)
- [ ] Graceful degradation: if parser panics or errors, return empty ProjectFile with tracing::warn
- [ ] Unit test: dispatch selects correct parser for each Language variant
- [ ] cargo test -p seshat-scanner passes

### US-007: Dependency Manifest Analysis (Story 2.5)

**Description:** As a developer, I want Seshat to analyze dependency manifests and cross-reference with code so that the knowledge graph knows which dependencies are used.

**Acceptance Criteria:**
- [ ] `manifest.rs` in seshat-scanner parses Cargo.toml (TOML), package.json (JSON), pyproject.toml (TOML)
- [ ] Extracts declared dependencies with versions
- [ ] Cross-references actual usage: for each dependency, counts files importing from it (from parsed IR)
- [ ] Dead dependencies (declared but never imported) flagged
- [ ] Dependencies categorized by domain where detectable (http, logging, testing, validation, etc.)
- [ ] Results returned as structured data ready for knowledge graph insertion
- [ ] Unit tests for each manifest format with fixture files
- [ ] cargo test -p seshat-scanner passes

### US-008: Module Structure & Dependency Graph (Story 2.6)

**Description:** As a developer, I want Seshat to detect module structure and build a dependency graph so that the knowledge graph represents code organization.

**Acceptance Criteria:**
- [ ] Each directory containing source files detected as a module
- [ ] Import/export relationships stored as DependsOn edges between module nodes
- [ ] Module hierarchy represented via PartOf edges (submodule → parent module)
- [ ] Results returned as Vec of KnowledgeNode (Fact nature) and Vec of Edge
- [ ] Queryable: given module path, can find dependents and dependencies
- [ ] Unit test with fixture project verifying module detection and edges
- [ ] cargo test -p seshat-scanner passes

### US-009: Documentation Ingestion (Story 2.7)

**Description:** As a developer, I want Seshat to parse documentation files as knowledge sources so that project docs enrich the knowledge graph.

**Acceptance Criteria:**
- [ ] `documentation.rs` in seshat-scanner handles Markdown (.md), JSON schema (.json), OpenAPI (.yaml/.yml)
- [ ] Markdown: headings and lists extracted as Fact/Rule knowledge nodes
- [ ] JSON schemas: data structure definitions extracted as Fact nodes
- [ ] OpenAPI specs: endpoint definitions extracted as Fact nodes
- [ ] All documentation-sourced nodes tagged with source: "documentation" in ext_data
- [ ] No NLP or prose-level convention extraction — structured information only
- [ ] Unit tests for each documentation format
- [ ] cargo test -p seshat-scanner passes

### US-010: Knowledge Graph Persistence with Bincode Serialization (Story 2.8a)

**Description:** As a developer, I want parsed IR serialized with bincode and version prefix so that cached IR can be invalidated on schema changes.

**Acceptance Criteria:**
- [ ] `seshat-storage` depends on `bincode` crate
- [ ] IR serialization uses version prefix per ADR-16: first byte = IR_SCHEMA_VERSION, rest = bincode data
- [ ] Deserialization checks version byte — mismatch returns StaleIR error (triggers re-parse)
- [ ] serialize_ir and deserialize_ir functions in seshat-storage
- [ ] Unit test: roundtrip serialize/deserialize ProjectFile
- [ ] Unit test: version mismatch detected and returns error
- [ ] cargo test -p seshat-storage passes

### US-011: Scan Orchestration with Incremental Re-check (Story 2.8b)

**Description:** As a developer, I want the scan pipeline to persist results and support incremental re-check so that re-scanning is fast.

**Acceptance Criteria:**
- [ ] `scan_project(root, config, db)` orchestrates: discover → parse → store IR → build knowledge nodes/edges → store graph
- [ ] All ProjectFile IR stored in files_ir table via FileIRRepository
- [ ] All knowledge nodes stored in nodes table, all edges in edges table
- [ ] On re-scan: content_hash comparison skips unchanged files
- [ ] Changed files: re-parsed, IR + findings updated in DB
- [ ] New files: parsed and inserted
- [ ] Deleted files: IR + nodes + edges removed from DB
- [ ] Integration test: scan fixture project, verify DB contents, modify file, re-scan, verify incremental update
- [ ] cargo test passes

### US-012: Automatic Database Backups (Story 2.9)

**Description:** As a developer, I want automatic database backups so that I can recover from corruption.

**Acceptance Criteria:**
- [ ] `backup.rs` in seshat-storage implements backup logic
- [ ] Backup creates copy of .db file with timestamp suffix (e.g., .seshat.db.2026-03-26)
- [ ] Old backups beyond retention count (default: 3) are deleted
- [ ] Backup interval configurable via BackupConfig (default: 24 hours)
- [ ] Backup disableable via BackupConfig.enabled = false
- [ ] backup_if_needed(db_path, config) checks last backup time and acts accordingly
- [ ] Unit tests: backup creation, retention cleanup, disabled config skips backup
- [ ] cargo test -p seshat-storage passes

## Functional Requirements

- FR-1: File discovery uses `ignore` crate WalkBuilder for .gitignore, custom patterns, and size limits
- FR-2: Tree-sitter parsing for Rust extracts use statements, fn items, struct/enum/trait, pub visibility, derives, trait impls
- FR-3: Tree-sitter parsing for TypeScript extracts imports (named/default/type-only), exports, interfaces, classes, barrel detection
- FR-4: Tree-sitter parsing for JavaScript detects CommonJS vs ESM, extracts require/module.exports/import/export
- FR-5: Tree-sitter parsing for Python extracts imports, __all__, __init__.py detection, type hints, decorators
- FR-6: Parser trait enables language dispatch and graceful degradation on parse failure
- FR-7: Dependency manifest analysis parses Cargo.toml, package.json, pyproject.toml and cross-references with code
- FR-8: Module structure detection creates Fact nodes for modules with DependsOn and PartOf edges
- FR-9: Documentation ingestion extracts structured info from Markdown, JSON schema, OpenAPI into knowledge nodes
- FR-10: IR serialized with bincode + version prefix (ADR-16) for cache invalidation
- FR-11: Scan orchestration persists all IR, nodes, edges and supports incremental re-check via content_hash
- FR-12: Automatic backups with configurable interval and retention

## Non-Goals

- No convention detection (Epic 3)
- No CLI scan report formatting (Epic 4)
- No MCP server integration (Epic 5)
- No file watcher / real-time updates (Epic 9)
- No branch-aware snapshots (Epic 10)
- No NLP or semantic analysis of documentation prose
- No call graph extraction (deferred to M2+)

## Developer Context

The implementing agent is a **professional Rust developer** writing **idiomatic Rust code**. This means:

- Use Rust's type system fully: enums for variants, newtypes for domain IDs, `Option` for nullable, `Result` for fallible operations
- Prefer `impl Trait` and generics over dynamic dispatch unless trait objects are architecturally required
- Use `?` operator for error propagation, never `.unwrap()` in library code (only in tests and `main.rs` init)
- Derive macros where appropriate: `Debug`, `Clone`, `Serialize`, `Deserialize`, `thiserror::Error`
- Follow Rust naming conventions: `snake_case` functions, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants
- Use `#[must_use]` on functions returning important values
- Prefer `&str` over `String` in function parameters, `PathBuf`/`&Path` for file paths
- Write doc comments (`///`) on all public items
- Use `#[cfg(test)]` modules at the bottom of each file
- Respect the existing code patterns already established in the workspace (see `seshat-core` for reference)
- Run `cargo clippy` mentally — no needless clones, no redundant closures, no unused imports
- Use `rayon` for CPU-bound parallelism (scanning), `tokio` for async I/O — never mix (ADR concurrency pattern)
- Graceful degradation: one bad file must not crash the scan, one failed parser must not prevent partial analysis

## Technical Considerations

- **Tree-sitter grammars** are C dependencies compiled into the binary. Use tree-sitter-rust, tree-sitter-typescript, tree-sitter-javascript, tree-sitter-python crates.
- **ADR-6**: Files processed in parallel via rayon. Parser runs per file.
- **ADR-8**: Cross-language IR with common base + LanguageIR enum. All types defined in seshat-core.
- **ADR-15**: Use `ignore` crate (from ripgrep) for file walking, not walkdir.
- **ADR-16**: IR cache versioning with version prefix byte in bincode serialization.
- **SHA256**: Use `sha2` crate for content hashing.
- **Manifest parsing**: Use `toml` crate for Cargo.toml/pyproject.toml, `serde_json` for package.json.
- **Graceful degradation**: One bad file must not crash the scan. Log warning, produce empty IR, continue.

## Success Metrics

- All 4 language parsers produce correct IR for fixture projects
- File discovery correctly respects .gitignore and custom patterns
- Incremental re-scan skips unchanged files (verified by test)
- Backup creates/rotates files correctly
- cargo test passes across all affected crates
- Scan of 1000-file project completes in reasonable time (no performance regression)

## Open Questions

- None — all architectural decisions documented in ADRs.
