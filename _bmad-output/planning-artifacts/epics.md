---
stepsCompleted: [1, 2, 3, 4]
inputDocuments: [prd.md, architecture.md, ux-design-specification.md]
---

# Seshat - Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for Seshat, decomposing the requirements from the PRD, Architecture, and UX Design Specification into implementable stories.

## Requirements Inventory

### Functional Requirements

- **FR1-FR12, FR55** [M0]: Scanning & Indexing — Tree-sitter parsing, dependency manifests, module detection, documentation ingestion, .gitignore respect, graceful degradation, SQLite storage
- **FR13-FR20, FR56** [M0/M3]: Knowledge Graph — 2D typing (Nature×Weight), typed edges, confidence scoring, Decision reasoning, branch snapshots, branch switch, GC
- **FR21-FR30** [M0/M1/M2]: Convention Detection — 8 detectors (dependency usage, imports, error handling, naming, exports, logging, tests, file structure), language-aware weighting, cross-reference with docs
- **FR31-FR39** [M1]: MCP Server & Tools — stdio/SSE/HTTP, query_project_context, query_convention, structured JSON, informative errors
- **FR40-FR46** [M0/M1/M2/M3]: CLI Interface — scan, serve, status, review (TUI), init, --version
- **FR47-FR48, FR57-FR62** [M1/M2]: Multi-Repo & Submodules — path-based ID, child graphs, auto-scope, explicit scope, submodule metadata
- **FR49-FR52** [M1/M2/M0]: Search & Data — FTS5, optional vector, backups, config
- **FR53-FR54** [M0]: Configuration — optional config file, zero-config defaults

**Total: 73 FRs** (M0: 27, M1: 24, M2: 10, M3: 12) — FR63-FR70 added 2026-03-30 from competitive analysis, FR71-FR72 added 2026-04-03 for call logging dogfooding

### Non-Functional Requirements

- **NFR1-NFR11**: Performance — scan <60s/100kLOC, parallel scanning, MCP P95 <1s, hot tier <1s, warm tier <30s, branch switch <2s, memory <500MB/100MB, DB <50MB
- **NFR12-NFR17**: Reliability — crash rate <1/1000, graceful degradation, transactional writes, interrupted scan recovery, daily backups, no resource leaks
- **NFR18-NFR20**: Observability — structured logging (tracing), configurable verbosity, tool call logging
- **NFR21-NFR27**: Integration — MCP compliance, consistent JSON, cross-platform, git compat, Tree-sitter compat, terminal compat, SQLite compat
- **NFR28-NFR29**: Compatibility — auto DB migration from any version
- **NFR30-NFR34**: Maintainability & DX — modular detectors, thin MCP layer, test coverage, self-scan CI, fast builds

**Total: 34 NFRs**

### Additional Requirements (Architecture)

- **ARCH-1 through ARCH-6**: Infrastructure — workspace setup (9 crates), CI/CD, conventional commits, pre-commit hooks, refinery migrations
- **ARCH-7 through ARCH-13**: Core patterns — IR cache versioning, SQLite connection management, startup/shutdown sequence, RepoRegistry, cross-reference logic, input validation, response envelope
- **ARCH-14 through ARCH-23**: Code patterns — type-safe IDs, test helpers, error types per crate, version string, DetectorResult type, scan/serve interaction, file walker (ignore crate), concurrency, transactions, serialization

**Total: 23 ARCH requirements**

### UX Design Requirements

- **UX-DR1 through UX-DR14**: CLI scan report (two-phase progress, project overview, conventions, submodules, next steps, verbose mode)
- **UX-DR15 through UX-DR33**: TUI review wizard (layout, key bindings, search/filter, review summary, precision diagnostic)
- **UX-DR34 through UX-DR39**: Serve command output (startup, shutdown)
- **UX-DR40 through UX-DR44**: Status command output (projects tree, watcher, server)
- **UX-DR45 through UX-DR51**: Init command (auto-detect clients, config snippets, $PWD paths)
- **UX-DR52**: Version output
- **UX-DR53 through UX-DR59**: Error patterns and verbosity levels
- **UX-DR60 through UX-DR61**: XDG data directory
- **UX-DR62 through UX-DR86**: MCP response schemas (envelope, all 5 tools, input validation errors)
- **UX-DR87 through UX-DR89**: General CLI formatting (level:message pattern, section headers, bordered boxes)

**Total: 89 UX-DRs**

### FR Coverage Map

| FR | Epic | Brief |
|----|------|-------|
| FR1 | 2 | Scan project directory |
| FR2 | 2 | Tree-sitter AST parsing (4 languages) |
| FR3 | 2 | Dependency manifest analysis |
| FR4 | 2 | Dependency graphs from AST |
| FR5 | 2 | Module structure detection |
| FR6 | 4 | Analysis report after scan |
| FR7 | 10 | Incremental updates (hot/warm) |
| FR8 | 10 | File watcher real-time |
| FR9 | 10 | Bulk change detection |
| FR10 | 2 | SQLite storage |
| FR11 | 2 | Documentation file ingestion |
| FR12 | 2 | Graceful skip unparseable files |
| FR13 | 2 | 2D knowledge node typing |
| FR14 | 2 | Typed graph edges |
| FR15 | 2 | Confidence scoring |
| FR16 | 12 | Interactive convention review |
| FR17 | 11 | Per-branch snapshots |
| FR18 | 11 | Instant branch switch |
| FR19 | 11 | Background sync after switch |
| FR20 | 11 | GC deleted branches |
| FR21 | 3 | Dependency usage detector |
| FR22 | 3 | Import organization detector |
| FR23 | 3 | Error handling detector |
| FR24 | 3 | Naming conventions detector |
| FR25 | 3 | Export patterns detector |
| FR26 | 3 | Logging patterns detector |
| FR27 | 3 | Test patterns detector |
| FR28 | 3 | File structure detector |
| FR29 | 3 | Language-aware weighting |
| FR30 | 3 | Cross-reference code vs docs |
| FR31 | 5 | MCP server (stdio operational; SSE/HTTP deferred to Epic 6) |
| FR32 | 5 | query_project_context tool |
| FR33 | 5 | query_convention tool |
| FR34 | 7 | query_code_pattern tool |
| FR35 | 7 | validate_approach tool |
| FR36 | 7 | query_dependencies tool |
| FR37 | 7 | Proactive duplicate detection |
| FR38 | 5 | Structured JSON responses |
| FR39 | 5 | Informative errors for unscanned repos |
| FR40 | 4 | seshat scan command |
| FR41 | 5 | seshat serve command |
| FR42 | 6 | seshat status command (implemented in Epic 6 US-011) |
| FR43 | 12 | seshat review TUI |
| FR44 | 12 | Review search/filter |
| FR45 | 12 | Precision self-diagnostic |
| FR46 | 9 | seshat init command |
| FR47 | 6 | Multi-repo namespace isolation |
| FR48 | 6 | Independent knowledge graphs per repo |
| FR49 | 5 | FTS5 full-text search |
| FR50 | 7 | Optional vector search |
| FR51 | 2 | Automatic DB backups |
| FR52 | 2 | Configurable backup settings |
| FR53 | 1 | Optional config file |
| FR54 | 1 | Zero-config defaults |
| FR55 | 2 | .gitignore respect |
| FR56 | 2 | Decision reasoning storage |
| FR57 | 6 | Physical path as repo ID |
| FR58 | 6 | Submodule detection + child graphs |
| FR59 | 6 | Auto-scope by file path |
| FR60 | 7 | Explicit scope parameter |
| FR61 | 6 | Default scope = root project |
| FR62 | 6 | Submodule relationship metadata |
| FR63 | 3.5 | Convention trend indicators |
| FR64 | 5 | Golden files (convention-compliant exemplars) |
| FR65 | 5 | record_decision tool |
| FR66 | 5 | update/remove_decision tools |
| FR67 | 3.5 | Wrapper/facade detection |
| FR68 | 3.5 | Package registry metadata categorization |
| FR69 | 5 | metadata.next_steps in MCP responses |
| FR70 | 7 | Evidence gating (ready + what_would_help) |
| FR71 | 6.5 | MCP call logging to JSONL file |
| FR72 | 6.5 | Call log opt-in via CLI flag and config |

**Coverage: 72/73 FRs mapped.** (FR4 descoped to dependency graphs for M0; call graphs deferred to M2+)

## Epic List

### Epic 1: Development Infrastructure & Project Bootstrap **[COMPLETED]**
Seshat project is set up with Rust workspace, 9 crates, CI/CD pipeline, pre-commit hooks, and database migrations — enabling systematic development of all features.

**Status:** All 7 stories (1.1-1.7) implemented via Ralph Loop. Workspace scaffolded, core types defined, SQLite schema with migrations, repository CRUD, config system, CI/CD pipeline, and test fixtures all operational.

**FRs covered:** FR53, FR54
**ARCH covered:** ARCH-1 through ARCH-6, ARCH-14 through ARCH-17, ARCH-21 through ARCH-23
**NFR covered:** NFR28, NFR29, NFR30, NFR32, NFR33, NFR34

### Epic 2: Code Scanning & Knowledge Graph **[COMPLETED]**
Developer can scan a project directory and Seshat builds a knowledge graph with parsed code, detected modules, dependencies, and documentation — the foundation of all intelligence.

**Status:** All 9 stories (2.1-2.9) implemented via Ralph Loop. File discovery, Tree-sitter parsing (4 languages), dependency manifest analysis, module structure detection, documentation ingestion, knowledge graph persistence with incremental re-check, and automatic backups all operational.

**FRs covered:** FR1, FR2, FR3, FR4, FR5, FR10, FR11, FR12, FR55, FR13, FR14, FR15, FR56, FR51, FR52
**ARCH covered:** ARCH-7, ARCH-8, ARCH-18, ARCH-20
**NFR covered:** NFR1, NFR2, NFR3, NFR9, NFR11, NFR12, NFR13, NFR14, NFR15, NFR16, NFR27
**UX-DR covered:** UX-DR60, UX-DR61

### Epic 3: Convention Detection Engine **[COMPLETED]**
Seshat can automatically detect coding conventions from scanned code — import patterns, error handling, naming, dependencies, and more — assigning confidence scores and cross-referencing with documentation.

**Status:** All 10 stories (3.1-3.10) implemented via Ralph Loop. ConventionDetector trait, detection pipeline, and all 8 detectors (dependency usage, imports, error handling, naming, exports, logging, tests, file structure) plus cross-reference logic all operational. 1,077 tests passing.

**FRs covered:** FR21, FR22, FR23, FR24, FR25, FR26, FR27, FR28, FR29, FR30
**ARCH covered:** ARCH-11
**NFR covered:** NFR30
*Stories span M0 (first 3 detectors), M1 (3 more), M2 (final 2). Each story is standalone.*

### Epic 3.5: Competitive Analysis Retrofit (Added 2026-03-30) **[COMPLETED]**
Retrofit existing implemented code (Epics 1-3) with improvements from competitive analysis + detector hardcode review. Adds unified dependency taxonomy, package registry metadata, git date collection, convention trends, wrapper detection, function parameter extraction, and heuristic fallbacks across all detectors.

**Status:** All 7 stories (3.5.1-3.5.7) implemented via BMad Dev Story workflow. Unified DependencyDomain taxonomy, package registry metadata (crates.io/npm/PyPI), git file dates via gix, P90 trend computation, wrapper/facade detection via import graph, function parameter extraction (4 parsers), and heuristic fallbacks for all detectors all operational.

**FRs covered:** FR63, FR67, FR68
**ARCH covered:** ADR-24, ADR-25, ADR-28, ADR-29
**Depends on:** Epics 1-3 (already implemented)
**Blocks:** Epics 4-5 (new features depend on enriched data)
*7 stories: 3.5.1-3.5.7*

### Epic 4: CLI Scan Report & First Impression **[COMPLETED]**
Developer can run `seshat scan <path>` and see a beautiful, informative analysis report showing what Seshat discovered about their project — the "wow moment".

**Status:** All 4 stories (4.1-4.4) implemented via Ralph Loop. `seshat scan <path>` with clap, indicatif two-phase progress, owo-colors output, project overview with language bar charts, conventions with confidence tiers/trends, next steps, verbosity control (--quiet/--verbose), NO_COLOR support, shared formatting utilities all operational.

**Post-implementation UX polish (2026-04-01):**
- All scan phases now use uniform braille spinners (discovery, git history, scanning, module graph, manifests/docs, analysis) — no progress bars, no elapsed time for sub-second ops
- 35+ convention descriptions across 6 detectors fixed for proper aggregation (per-file counts removed)
- Submodule exclusion: `.gitmodules` parsing, `--include-submodules` flag, exclusion info in summary
- Report alignment: dynamic column width (30-60), alphabetical secondary sort, UTF-8 safe truncation
- ScanProgress enum expanded to 10 variants covering entire pipeline

**FRs covered:** FR6, FR40
**UX-DR covered:** UX-DR1 through UX-DR14, UX-DR52 through UX-DR59, UX-DR87 through UX-DR89

### Epic 5: MCP Server, Serve Command & Core Tools **[COMPLETED]**
Developer can start Seshat as MCP server via `seshat serve` and AI agent can connect and query project context and conventions — the core value proposition. Includes LLM-sourced decision recording, golden files, and next-step hints.

**Status:** All stories (5.1-5.8) implemented. Stories 5.1-5.7 via Ralph Loop (12 user stories — see `.ralph/tasks/prd-mcp-server-core-tools.md`). Story 5.8 (smart DB discovery + forward-compatible tool schemas) implemented separately. MCP server operational on stdio transport with 5 tools (query_project_context, query_convention, record_decision, update_decision, remove_decision). Convention persistence to nodes table, FTS5 search, golden files, response envelope, agent protocol documentation, smart DB discovery (cwd → git root → single DB → error) — all operational. 100+ unit tests.

**Note:** SSE/HTTP transports declared in ServerConfig but not wired — stdio only for M1. SSE/HTTP deferred to future epic (daemon mode).

**FRs covered:** FR31 (stdio only), FR32, FR33, FR38, FR39, FR41, FR49, FR61, FR64, FR65, FR66, FR69
**ARCH covered:** ARCH-9, ARCH-12, ARCH-13, ADR-27
**NFR covered:** NFR4, NFR5, NFR10, NFR17, NFR18, NFR19, NFR20, NFR21, NFR22, NFR23, NFR26
**UX-DR covered:** UX-DR34 through UX-DR39, UX-DR62 through UX-DR72, UX-DR84 through UX-DR86

### Epic 6: Submodule Support & Scoped Queries **[COMPLETED]**
Submodules scanned automatically into separate .db files. AI agent queries and writes routed to correct knowledge graph via scope detection (file_path auto-detect or explicit scope parameter). `seshat status` shows project tree with submodules. Daemon mode / multi-project serving deferred.

**Status:** All 12 user stories (US-001 through US-012) implemented via Ralph Loop (see `.ralph/tasks/prd-submodule-support-scoped-queries.md`). Submodule DB isolation, parallel scanning, change detection (commit_hash compare), scope resolution module, file_path auto-scope in all 5 tools, serve with submodule connections, `seshat status` command, and repo parameter activation — all operational.

**FRs covered:** FR42, FR48, FR57, FR58, FR59, FR61, FR62
**ARCH covered:** ARCH-10 (revised: submodule-aware, not multi-repo), ARCH-19
**UX-DR covered:** UX-DR8, UX-DR40 through UX-DR44
**PRD:** `.ralph/tasks/prd-submodule-support-scoped-queries.md`

### Epic 6.5: MCP Call Logging for Dogfooding **[COMPLETED]**
Purpose-built JSONL call logger for analyzing MCP tool usage during dogfooding. Opt-in via `--call-log` CLI flag or config.

**Status:** All 5 user stories (US-001 through US-005) implemented via Ralph Loop (see `.ralph/tasks/prd-mcp-call-logging.md`). CallLogEntry types, CallLogger struct (append-only JSONL writer with session ID + seq counter), McpServer integration via `execute_tool` helper, `--call-log` CLI flag with XDG default, and integration tests — all operational. Enabled locally via `opencode.json` logging to `.seshat/call-log.jsonl`.

**FRs covered:** FR71, FR72
**ARCH covered:** ADR-30
**PRD:** `.ralph/tasks/prd-mcp-call-logging.md`

### Epic 7: Advanced MCP Tools — Validate, Patterns, Dependencies, Diff Impact
AI agent can validate approaches before coding, find code patterns by functionality, and analyze dependencies — the killer features that differentiate Seshat. Includes evidence gating (`ready`/`whatWouldHelp`).

**Status:** All 4 stories (7.1-7.4) implemented. `query_code_pattern`, `validate_approach`, proactive duplicate detection, and `query_dependencies` — all operational. Code review completed 2026-04-07 (deferred items documented). Built-in embedding support wired through MCP for semantic search. Merged to main.

**FRs covered:** FR34, FR35, FR36, FR37, FR50, FR60, FR70
**ARCH covered:** ADR-26
**UX-DR covered:** UX-DR73 through UX-DR83

### Epic 8: Built-in Embeddings & Semantic Search Quality **[COMPLETED]**
Replace HTTP embedding providers (Ollama, OpenAI) with a zero-config built-in provider. Improve semantic search quality via richer embedding context.

**Status:** All 2 stories (8.1-8.2) implemented via BMad workflow. `fastembed-rs` built-in provider (all-MiniLM-L6-v2, 384-dim), richer embedding text (signature + body snippet + file imports), HTTP providers removed. Code review findings (D20-D22) deferred. Merged to main.

**FRs covered:** FR50 (vector search provider)
**ARCH covered:** ADR-26 (revised)

### Epic 9: CLI Utilities — Init, Update, Uninstall **[MOSTLY COMPLETE]**
Developer can install, update, and uninstall Seshat integration for AI coding agents.

**Note:** `seshat status` was implemented as part of Epic 6 (US-011).

**Status:** Stories 9.1 (init), 9.2 (agent instructions), 9.4 (uninstall), and 9.5 (auto-scan) — all **COMPLETE**. Story 9.3 (update check) — **DEFERRED** (requires GitHub Actions release pipeline planning). Total: ~2,600 lines of implementation code + 40+ integration tests.

**FRs covered:** FR46, FR71, FR72
**UX-DR covered:** UX-DR45 through UX-DR51

### Epic 10: File Watcher & Incremental Updates **[COMPLETED]**
Seshat watches the project directory for changes and updates the knowledge graph incrementally — hot tier for code structure, warm tier for convention aggregates. No manual re-scan needed.

**FRs covered:** FR7, FR8, FR9
**NFR covered:** NFR6, NFR7

### Epic 11: Branch-Aware Knowledge Graph
Seshat maintains per-branch snapshots of the knowledge graph. Switching branches instantly switches context. Background sync catches up. Garbage collection cleans deleted branches.

**FRs covered:** FR17, FR18, FR19, FR20
**NFR covered:** NFR8

### Epic 12: Interactive Convention Review (TUI)
Developer can interactively review detected conventions via TUI wizard — confirm, reject, partially confirm. Search/filter by keyword. Precision self-diagnostic shows calibration quality.

**FRs covered:** FR16, FR43, FR44, FR45
**UX-DR covered:** UX-DR15 through UX-DR33

---

## Epic 1: Development Infrastructure & Project Bootstrap [COMPLETED]

Seshat project is set up with Rust workspace, 9 crates, CI/CD pipeline, pre-commit hooks, and database migrations — enabling systematic development of all features.

> All stories in this epic were implemented via Ralph Loop (see `.ralph/tasks/prd-epic1-infrastructure.md`).

### Story 1.1: Initialize Rust Workspace with Crate Scaffolding [COMPLETED]

As a **Seshat developer**,
I want a properly structured Rust workspace with all 9 crates scaffolded,
So that I can begin implementing features in isolated, well-defined modules.

**Acceptance Criteria:**

**Given** a fresh clone of the Seshat repository
**When** I run `cargo build`
**Then** the workspace compiles with all 9 crates (seshat-core, seshat-scanner, seshat-detectors, seshat-storage, seshat-graph, seshat-mcp, seshat-watcher, seshat-cli, seshat-bin)
**And** each crate has a `lib.rs` with module-level `//!` doc comment describing its purpose
**And** each crate has an `error.rs` with a crate-specific error type using `thiserror`
**And** inter-crate dependencies in `Cargo.toml` match the architectural dependency graph (no cycles)
**And** `seshat-bin` has `[[bin]] name = "seshat"` and a `main.rs` that compiles

### Story 1.2: Core Types & Traits [COMPLETED]

As a **Seshat developer**,
I want the foundational types defined in `seshat-core`,
So that all crates share a consistent type system from the start.

**Acceptance Criteria:**

**Given** the `seshat-core` crate
**When** I inspect the public API
**Then** `ir.rs` defines `ProjectFile`, `LanguageIR` enum (Rust, TypeScript, JavaScript, Python), `Import`, `Export`, `Function`, `TypeDef`, `DependencyUsage`
**And** `knowledge.rs` defines `KnowledgeNode`, `KnowledgeNature` enum (Fact, Convention, Observation, Decision, Preference), `KnowledgeWeight` enum (Rule, Strong, Moderate, Weak, Info)
**And** `edge.rs` defines `Edge`, `EdgeType` enum (RelatedTo, Updates, Contradicts, PartOf, DependsOn, Implements)
**And** `ids.rs` defines newtype IDs: `NodeId(i64)`, `EdgeId(i64)`, `BranchId(String)`
**And** `detector_result.rs` defines `ConventionFinding`, `CodeEvidence`, `DetectorResults`
**And** `config.rs` defines `ScanConfig`, `DetectionConfig`, `ServerConfig` — all implement `Default`
**And** `test_helpers.rs` exports factory functions behind `"test-helpers"` feature flag
**And** all types derive `Debug`, `Clone`, `Serialize`, `Deserialize` where appropriate
**And** all structs use `#[serde(rename_all = "snake_case")]`

### Story 1.3: SQLite Schema & Database Migrations [COMPLETED]

As a **Seshat developer**,
I want the initial SQLite schema and migration infrastructure,
So that knowledge graph data can be persisted reliably.

**Acceptance Criteria:**

**Given** the `seshat-storage` crate with `refinery` configured
**When** `Database::open(path)` is called
**Then** migrations are auto-applied via `embed_migrations!`
**And** `V1__initial_schema.sql` creates tables: `nodes` (with `branch_id`, `nature`, `weight`, `confidence`, `adoption_count`, `total_count`, `description`, `ext_data` JSON), `edges` (with `source_id`, `target_id`, `edge_type`, `branch_id`, `weight`, `metadata`), `files_ir` (with `branch_id`, `file_path`, `language`, `content_hash`, `ir_data` BLOB, `updated_at`), `metadata` (for repo info, schema version)
**And** proper indexes exist: `idx_nodes_branch_id`, `idx_nodes_nature`, `idx_edges_source_id`, `idx_edges_target_id`, `idx_files_ir_branch_path`
**And** `Database` struct wraps `Arc<Mutex<Connection>>` for writes and read-only connections for queries
**And** all writes use transactions
**And** unit tests verify migration applies cleanly on fresh DB and re-opening existing DB is idempotent

### Story 1.4: Repository Traits & Basic CRUD [COMPLETED]

As a **Seshat developer**,
I want repository traits and SQLite implementations for nodes, edges, files_ir, and branches,
So that other crates can persist and query data through a clean interface.

**Acceptance Criteria:**

**Given** the `seshat-storage` crate
**When** I use the repository interfaces
**Then** `NodeRepository` trait supports: `insert`, `get_by_id`, `find_by_nature`, `find_by_branch`, `update`, `delete`
**And** `EdgeRepository` trait supports: `insert`, `find_by_source`, `find_by_target`, `find_by_type`, `delete`
**And** `FileIRRepository` trait supports: `upsert` (insert or update by path+branch), `get_by_path`, `get_by_branch`, `delete_by_path`, `check_content_hash`
**And** `BranchRepository` trait supports: `create_snapshot` (full copy), `switch_branch`, `delete_branch`, `list_branches`, `get_current_branch`
**And** SQLite implementations pass all CRUD tests
**And** `create_snapshot` copies all nodes + edges + files_ir with new branch_id
**And** transactions are used for all multi-row operations

### Story 1.5: Configuration System [COMPLETED]

As a **Seshat developer**,
I want a configuration loading system that reads `seshat.toml` with sensible defaults,
So that Seshat works zero-config out of the box but is customizable.

**Acceptance Criteria:**

**Given** the `seshat-bin` crate
**When** Seshat starts without a config file
**Then** all config values use defaults from `Default` trait implementations
**When** a `seshat.toml` file exists in the project root or XDG config directory
**Then** values from the file override defaults
**And** config sections supported: `[scan]`, `[detection]`, `[server]`, `[watcher]`, `[backup]`, `[cache]`, `[embedding]`
**And** `seshat.toml.example` exists in repo root with all options commented out and default values documented
**And** environment variables can override config file values (e.g., `SESHAT_LOG` for log level)

### Story 1.6: CI/CD Pipeline & Developer Tooling [COMPLETED]

As a **Seshat developer**,
I want CI/CD pipelines, pre-commit hooks, and conventional commit enforcement,
So that code quality is automated and releases are consistent.

**Acceptance Criteria:**

**Given** the repository with `.github/workflows/`
**When** a PR is opened
**Then** `ci.yml` runs: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo doc --no-deps`, conventional commit validation
**And** `lint-workflows.yml` runs `actionlint` on changes to `.github/workflows/` only
**And** `.pre-commit-config.yaml` configures: trailing-whitespace, end-of-file-fixer, check-yaml, check-toml, check-merge-conflict, conventional-pre-commit (commit-msg stage), cargo fmt, cargo clippy
**And** `release.yml` uses `release-plz` for: version bump, CHANGELOG.md generation, git tag, GitHub Release with cross-compiled binaries
**And** `build.rs` in `seshat-bin` captures git commit hash for `seshat --version`

### Story 1.7: Test Fixtures & Reference Projects [COMPLETED]

As a **Seshat developer**,
I want reference test projects with known conventions,
So that integration tests can verify scanning and detection against expected results.

**Acceptance Criteria:**

**Given** `tests/fixtures/` directory
**When** I inspect the fixture projects
**Then** `rust_project/` contains: a small Rust project with known patterns (thiserror errors, tracing logging, grouped imports, test files)
**And** `typescript_project/` contains: a small TS project with known patterns (barrel exports, ESM imports, Jest tests, custom error classes)
**And** `python_project/` contains: a small Python project with known patterns (stdlib logging, grouped imports, pytest, type hints)
**And** each fixture project has a `expected_conventions.json` documenting what detectors should find
**And** fixture projects are small (<50 files each) but representative of real patterns
**And** `seshat-detectors/tests/fixtures/` contains individual sample files for unit-level detector testing

---

## Epic 2: Code Scanning & Knowledge Graph [COMPLETED]

Developer can scan a project directory and Seshat builds a knowledge graph with parsed code, detected modules, dependencies, and documentation.

> All stories in this epic were implemented via Ralph Loop (see `.ralph/tasks/prd-epic2-scanning.md`).

### Story 2.1: File Discovery & .gitignore Respect [COMPLETED]

As a **developer**,
I want Seshat to discover all relevant source files while respecting .gitignore,
So that only meaningful project files are scanned.

**Acceptance Criteria:**

**Given** a project directory with `.gitignore` excluding `node_modules/`, `target/`, `__pycache__/`
**When** Seshat discovers files for scanning
**Then** files matching .gitignore patterns are excluded
**And** files in `.git/` directory are always excluded
**And** hidden files/directories (starting with `.`) are excluded by default
**And** the `ignore` crate `WalkBuilder` is used for native gitignore support
**And** custom exclude patterns from `seshat.toml` `[scan].exclude_patterns` are applied
**And** files exceeding `max_file_size_kb` (default: 512KB) are skipped with a warning
**And** discovery phase reports total file count before parsing begins

### Story 2.2: Tree-sitter Parsing for Rust [COMPLETED]

As a **developer**,
I want Seshat to parse Rust source files into IR,
So that the knowledge graph contains structured understanding of Rust code.

**Acceptance Criteria:**

**Given** a Rust source file
**When** Seshat parses it with Tree-sitter
**Then** a `ProjectFile` IR is produced with: `imports` (use statements), `functions` (fn items with visibility), `types` (struct, enum, trait definitions), `exports` (pub items)
**And** `LanguageIR::Rust` contains: pub visibility info, mod structure, trait implementations, derive macros, error types (thiserror/anyhow patterns)
**And** `content_hash` (SHA256) is computed for change detection
**And** parsing errors are logged as warnings, not panics
**And** unparseable files produce a partial IR or empty IR with error note
**And** integration test parses `tests/fixtures/rust_project/` and verifies expected IR output

### Story 2.3: Tree-sitter Parsing for TypeScript [COMPLETED]

As a **developer**,
I want Seshat to parse TypeScript source files into IR,
So that TypeScript projects are fully understood.

**Acceptance Criteria:**

**Given** a TypeScript source file (`.ts`, `.tsx`)
**When** Seshat parses it
**Then** `ProjectFile` IR captures: imports (named, default, type-only), exports (named, default, re-exports), functions, types (interfaces, type aliases, classes), dependency usage
**And** `LanguageIR::TypeScript` contains: default vs named exports, barrel exports (index.ts detection), decorators, type-only imports
**And** `.tsx` files are handled (JSX elements don't break parsing)
**And** integration test parses `tests/fixtures/typescript_project/`

### Story 2.4: Tree-sitter Parsing for JavaScript & Python [COMPLETED]

As a **developer**,
I want Seshat to parse JavaScript and Python files into IR,
So that all four MVP languages are supported.

**Acceptance Criteria:**

**Given** JavaScript files (`.js`, `.jsx`, `.mjs`, `.cjs`)
**When** parsed
**Then** `LanguageIR::JavaScript` captures: CommonJS vs ESM detection, `module.exports`, require() calls, export patterns

**Given** Python files (`.py`)
**When** parsed
**Then** `LanguageIR::Python` captures: `__all__`, `__init__.py` conventions, type hints, decorator patterns, import grouping

**And** integration tests for both languages using fixture projects

### Story 2.5: Dependency Manifest Analysis [COMPLETED]

As a **developer**,
I want Seshat to analyze dependency manifests and cross-reference with code,
So that the knowledge graph knows which dependencies are actually used.

**Acceptance Criteria:**

**Given** a project with `Cargo.toml`, `package.json`, or `pyproject.toml`
**When** Seshat scans the project
**Then** all declared dependencies are extracted with versions
**And** actual usage cross-referenced: for each dependency, count files importing from it
**And** dead dependencies (declared but never imported) flagged
**And** dependencies categorized by domain where detectable (http, logging, testing, etc.)
**And** results stored as `Fact` knowledge nodes with `DependsOn` edges

### Story 2.6: Module Structure & Dependency Graph [COMPLETED]

As a **developer**,
I want Seshat to understand module structure and build a dependency graph,
So that the knowledge graph represents how code is organized and interconnected.

**Acceptance Criteria:**

**Given** a scanned project
**When** the knowledge graph is built
**Then** each directory with source files detected as a module
**And** import/export relationships stored as `DependsOn` edges
**And** module hierarchy represented via `PartOf` edges
**And** dependency graph queryable: "what depends on module X?" and "what does X depend on?"

### Story 2.7: Documentation Ingestion [COMPLETED]

As a **developer**,
I want Seshat to parse Markdown, JSON schemas, and OpenAPI specs as knowledge sources,
So that project documentation enriches the knowledge graph.

**Acceptance Criteria:**

**Given** a project with `README.md`, `CODING_GUIDELINES.md`, `openapi.yaml`, or JSON schema files
**When** Seshat scans
**Then** Markdown headings, lists, and key-value patterns extracted as `Fact`/`Rule` nodes
**And** OpenAPI specs produce `Fact` nodes about API endpoints
**And** JSON schemas produce `Fact` nodes about data structures
**And** documentation-sourced nodes have `source: "documentation"`
**And** prose-level convention extraction (NLP) NOT attempted — structured information only

### Story 2.8: Knowledge Graph Persistence & Incremental Re-check [COMPLETED]

As a **developer**,
I want parsed IR and knowledge graph persisted in SQLite with incremental re-check on restart,
So that re-scanning from scratch is not needed.

**Acceptance Criteria:**

**Given** a completed scan
**When** all files are parsed and detectors have run
**Then** all `ProjectFile` IR serialized (bincode with version prefix per ADR-16) in `files_ir` table
**And** all knowledge nodes in `nodes` table, all edges in `edges` table

**Given** Seshat restarts on a previously scanned project
**When** incremental re-check runs
**Then** `content_hash` comparison skips unchanged files
**And** changed files re-parsed, IR + findings updated
**And** new files parsed and inserted
**And** deleted files have IR + nodes + edges removed

### Story 2.9: Automatic Database Backups [COMPLETED]

As a **developer**,
I want Seshat to automatically backup the database,
So that I can recover from corruption without losing more than 24 hours of data.

**Acceptance Criteria:**

**Given** Seshat is running or a scan completes
**When** backup interval elapsed (default: 24 hours)
**Then** backup copy of `.db` created with timestamp suffix
**And** old backups beyond retention (default: 3) deleted
**And** configurable via `seshat.toml` `[backup]` section
**And** disableable via `enabled = false`

---

## Epic 3: Convention Detection Engine [COMPLETED]

Seshat can automatically detect coding conventions from scanned code — assigning confidence scores and cross-referencing with documentation.

> All stories in this epic were implemented via Ralph Loop (see `.ralph/tasks/prd-convention-detection-engine.md`).

### Story 3.1: ConventionDetector Trait & Detection Pipeline [COMPLETED]

As a **developer**,
I want a trait-based detection pipeline that runs all detectors on parsed IR,
So that adding new detectors requires no changes to core scanning logic.

**Acceptance Criteria:**

**Given** the `seshat-detectors` crate
**When** I inspect the public API
**Then** `ConventionDetector` trait defined with: `fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding>`
**And** `run_all_detectors(files: &[ProjectFile]) -> Vec<DetectorResults>` orchestrates all registered detectors
**And** detectors run sequentially per file, files processed in parallel via rayon
**And** `confidence.rs` implements frequency-based scoring: `adoption_count / total_count`
**And** configurable thresholds: >0.85 Strong, 0.50-0.85 Moderate, 0.20-0.50 Weak, <0.20 Info
**And** failing detector logs warning and is skipped for that file
**And** language-aware relevance weighting adjusts priority per language

### Story 3.2: Dependency Usage Detector [COMPLETED]

As a **developer**,
I want Seshat to detect canonical libraries per domain,
So that AI agents use the right libraries.

**Acceptance Criteria:**

**Given** a scanned project with dependency manifests and IR
**When** the dependency usage detector runs
**Then** libraries grouped by domain (HTTP, logging, testing, validation, etc.)
**And** most-used library per domain identified as canonical
**And** conflicting libraries for same domain flagged
**And** dead dependencies detected
**And** findings for all 4 languages produce correct results
**And** tests verify on fixture projects

### Story 3.3: Import Organization Detector [COMPLETED]

As a **developer**,
I want Seshat to detect import grouping and ordering patterns,
So that AI agents follow the project's import style.

**Acceptance Criteria:**

**Given** source files with import statements
**When** the import detector runs
**Then** grouping patterns detected: stdlib → external → internal
**And** barrel vs. direct import preference detected (TS/JS)
**And** type-only import separation detected (TS)
**And** language-specific: Rust `use`, Python import, JS/TS import/require

### Story 3.4: Error Handling Detector [COMPLETED]

As a **developer**,
I want Seshat to detect error handling patterns,
So that AI agents use consistent error handling.

**Acceptance Criteria:**

**Given** source files with error handling code
**When** the error handling detector runs
**Then** error type patterns detected (thiserror, custom classes, Exception hierarchy)
**And** propagation style detected (`?`, try-catch, try-except)
**And** error wrapping/chaining patterns detected
**And** findings include code examples of dominant pattern

### Story 3.5: Naming Conventions Detector [COMPLETED]

As a **developer**,
I want Seshat to detect naming conventions,
So that AI agents follow consistent naming.

**Acceptance Criteria:**

**Given** a scanned project
**When** the naming detector runs
**Then** file, function, type, constant, variable naming conventions detected per language
**And** language-aware: Rust conventions weighted lower (enforced by tooling), JS/Python/TS weighted higher

### Story 3.6: Export Patterns Detector [COMPLETED]

As a **developer**,
I want Seshat to detect export patterns,
So that AI agents create consistent module boundaries.

**Acceptance Criteria:**

**Given** source files with exports
**When** the export detector runs
**Then** default vs named export preference detected (TS/JS)
**And** barrel export pattern detected with adoption rate
**And** Rust pub/mod patterns detected
**And** Python `__all__` patterns detected

### Story 3.7: Logging & Observability Detector [COMPLETED]

As a **developer**,
I want Seshat to detect logging patterns,
So that AI agents use the right logging library and format.

**Acceptance Criteria:**

**Given** source files with logging code
**When** the logging detector runs
**Then** canonical logging library identified
**And** structured vs unstructured preference detected
**And** conflicting logging libraries flagged

### Story 3.8: Test Patterns Detector [COMPLETED]

As a **developer**,
I want Seshat to detect testing conventions,
So that AI agents write tests matching project style.

**Acceptance Criteria:**

**Given** a scanned project with test files
**When** the test detector runs
**Then** testing framework identified
**And** test file placement convention detected (co-located vs `tests/`)
**And** test naming convention detected
**And** setup/teardown patterns detected

### Story 3.9: File Structure Detector [COMPLETED]

As a **developer**,
I want Seshat to detect file organization patterns,
So that AI agents place new files correctly.

**Acceptance Criteria:**

**Given** a scanned project
**When** the file structure detector runs
**Then** directory organization pattern detected (by feature/type/layer)
**And** common directory conventions identified
**And** configuration file placement patterns detected

### Story 3.10: Cross-Reference Code vs Documentation [COMPLETED]

As a **developer**,
I want Seshat to compare code conventions with documentation,
So that contradictions are surfaced.

**Acceptance Criteria:**

**Given** conventions from code AND knowledge nodes from documentation
**When** cross-reference logic runs
**Then** matching conventions reinforced (confidence boost)
**And** contradictions identified and `Contradicts` edges created
**And** contradictions surfaced in future `validate_approach` responses
**And** keyword/topic matching used (not semantic/NLP)

---

## Epic 3.5: Competitive Analysis Retrofit [COMPLETED]

> **Added 2026-03-30** based on competitive analysis of 8 analogous projects. See `docs/research/competitive-analysis-2026-03-30.md`.
>
> Epics 1-3 are already implemented. This epic retrofits the existing code with improvements that must be in place before Epics 4+ can deliver full value. Execute this epic before proceeding to Epic 4.

> All stories in this epic were implemented via BMad Dev Story workflow (see `_bmad-output/implementation-artifacts/3.5-*.md`).

### Story 3.5.1: Unify Dependency Domain Taxonomy [COMPLETED]

As a **developer**,
I want a single, consistent dependency domain taxonomy across scanner and detectors,
So that domain classification is not duplicated or contradictory.

**Acceptance Criteria:**

**Given** two parallel enums: `DependencyDomain` (8 categories in `seshat-detectors`) and `DependencyCategory` (11 categories in `seshat-scanner`)
**When** this story is complete
**Then** single `DependencyDomain` enum defined in `seshat-core` with unified categories (merging: Http, WebFramework → Web; adding: Crypto, Utilities from scanner)
**And** `seshat-scanner/manifest.rs` uses the unified enum
**And** `seshat-detectors/dependency_usage.rs` uses the unified enum
**And** no duplication — one source of truth for domain classification
**And** existing tests updated to use unified enum
**And** `cargo test --workspace` passes

### Story 3.5.2: Package Registry Metadata Integration [COMPLETED]

As a **developer**,
I want dependency domain classification to use package registry metadata instead of hardcoded name lists,
So that new packages are correctly categorized without code changes. (FR68, ADR-25)

**Acceptance Criteria:**

**Given** a project with dependencies in manifest files
**When** `seshat scan` runs
**Then** for each dependency: lookup in local SQLite cache (`package_metadata` table) first
**And** if cache miss: fetch from registry API (crates.io, npm, PyPI) and cache with 30-day TTL
**And** map registry categories/keywords/classifiers to unified `DependencyDomain`
**And** if no internet and no cache: fall back to hardcoded mapping with lower confidence
**And** new `seshat-scanner/src/registry.rs` module with `PackageRegistryClient` trait
**And** implementations for crates.io (`/api/v1/crates/{name}`), npm (`/{name}`), PyPI (`/pypi/{name}/json`)
**And** HTTP client: `ureq` (blocking, minimal deps)
**And** `package_metadata` table migration added
**And** `cargo test --workspace` passes (with mock HTTP responses in tests)

### Story 3.5.3: Git File Dates Collection [COMPLETED]

As a **developer**,
I want Seshat to collect last git commit date for each file during scan,
So that convention trend detection can determine Rising/Stable/Declining. (FR63, ADR-24)

**Acceptance Criteria:**

**Given** a project in a git repository
**When** `seshat scan` runs
**Then** `gix` performs a single commit walk to build `HashMap<PathBuf, i64>` of last modification timestamps
**And** `files_ir` table has new nullable column `last_commit_date INTEGER`
**And** `FileIRRepository::upsert` stores `last_commit_date` for each file
**And** files not in git (new, untracked) have `last_commit_date = NULL`
**And** incremental re-scan: only update dates for changed files
**And** `cargo test --workspace` passes

### Story 3.5.4: Convention Trend Computation [COMPLETED]

As a **developer**,
I want each detected convention to have a trend indicator (Rising/Stable/Declining/Unknown),
So that AI agents know whether to adopt or avoid a pattern. (FR63, ADR-24)

**Acceptance Criteria:**

**Given** conventions detected with adoption data AND files_ir with `last_commit_date`
**When** warm tier aggregation runs (or initial scan completes)
**Then** for each convention: compute P90 percentile of `last_commit_date` for files where `follows_convention = true`
**And** P90 < 90 days → Rising, 90-365 days → Stable, > 365 days → Declining, no git data → Unknown
**And** thresholds configurable in `DetectionConfig`: `trend_rising_days`, `trend_stable_days`
**And** trend stored in `KnowledgeNode.ext_data` as `{"trend": "rising"|"stable"|"declining"|"unknown"}`
**And** `Trend` enum added to `seshat-core/src/knowledge.rs`
**And** convention MCP responses include trend field
**And** unit tests verify correct trend at threshold boundaries
**And** `cargo test --workspace` passes

### Story 3.5.5: Wrapper/Facade Convention Detection Enhancement [COMPLETED]

As a **developer**,
I want the dependency usage detector to detect wrapper/facade patterns structurally,
So that direct usage of wrapped external dependencies is flagged. (FR67, ADR-28)

**Acceptance Criteria:**

**Given** a project where internal module M wraps external dependency D
**When** the dependency usage detector runs
**Then** for each external dependency: identify files that import it directly
**And** identify internal modules that import the external dep AND are re-imported by other project files
**And** if majority (>50%) of consumers use wrapper M instead of D directly: establish wrapper convention
**And** files importing D directly when wrapper M exists: `follows_convention = false`
**And** convention description auto-generated: "Use `{wrapper_module}` for `{external_dep}` operations"
**And** no hardcoded directory names (`shared/`, `utils/`, etc.) — purely import graph analysis
**And** works for all 4 supported languages
**And** unit tests with fixture projects demonstrating wrapper patterns
**And** `cargo test --workspace` passes

### Story 3.5.6: Function Parameter Extraction & Naming Analysis [COMPLETED]

As a **developer**,
I want the naming detector to analyze function parameter naming conventions,
So that AI agents follow consistent parameter naming across the project. (ADR-29)

**Acceptance Criteria:**

**Given** source files with functions
**When** scanned and naming detector runs
**Then** `Function` struct in `seshat-core/src/ir.rs` has `parameters: Vec<String>` field
**And** all 4 Tree-sitter parsers extract function parameter names (excluding Python `self`/`cls`)
**And** naming detector analyzes parameter name case patterns per language
**And** language-aware weighting: Rust lower (clippy conventions), JS/TS/Python higher
**And** existing tests updated for new field
**And** `cargo test --workspace` passes

### Story 3.5.7: Heuristic Fallbacks for Unknown Libraries [COMPLETED]

As a **developer**,
I want detectors to identify unknown libraries via heuristics,
So that new or uncommon packages are still classified rather than silently ignored. (ADR-29)

**Acceptance Criteria:**

**Given** a project with dependencies not in known-library lists
**When** detectors run
**Then** error handling detector: Rust derive(Error)/impl Error heuristic + add eyre/snafu/miette/error-stack; Python inheritance-based exception detection
**And** logging detector: name-based heuristic (contains `log`/`trace`) + API shape heuristic (`.info()`/`.debug()` calls)
**And** test patterns detector: config file detection (jest.config, vitest.config, [tool.pytest]) + name-based heuristic (contains `test`/`mock`)
**And** dependency usage detector: name-based domain classification for unrecognized packages at lower confidence
**And** export patterns detector: reads `JavaScriptIR::module_system` (was dead code), flags mixed ESM/CJS
**And** heuristic findings use KnowledgeWeight::Weak or Info — never Strong or Rule
**And** known-library findings always override heuristic findings for same package
**And** unit tests for each heuristic
**And** `cargo test --workspace` passes

---

## Epic 4: CLI Scan Report & First Impression [COMPLETED]

Developer can run `seshat scan <path>` and see an informative analysis report — the "wow moment".

> All stories in this epic were implemented via Ralph Loop (see `.ralph/tasks/prd-cli-scan-report.md`).

### Story 4.1: Basic `seshat scan` Command & Two-Phase Progress [COMPLETED]

As a **developer**,
I want to run `seshat scan <path>` and see scanning progress,
So that I know Seshat is working and how long it will take.

**Acceptance Criteria:**

**Given** a project directory
**When** I run `seshat scan ./my-project`
**Then** version header displayed
**And** Phase 1: `Discovering files... {count} found`
**And** Phase 2: progress bar with known total `Scanning ████░░░ {done}/{total} [{elapsed}]`
**And** scan pipeline executes end-to-end (discovery → parse → detect → store)
**And** database created in XDG data directory

### Story 4.2: Scan Report — Project Overview Section [COMPLETED]

As a **developer**,
I want the scan report to show project overview,
So that I immediately see what Seshat learned.

**Acceptance Criteria:**

**Given** a completed scan
**When** the report displays
**Then** language breakdown with horizontal bar charts
**And** module count and dependency count with ecosystem breakdown
**And** submodules section if applicable

### Story 4.3: Scan Report — Conventions & Next Steps [COMPLETED]

As a **developer**,
I want the scan report to show conventions and next steps,
So that I see value immediately.

**Acceptance Criteria:**

**Given** a completed scan with detected conventions
**When** the report displays
**Then** confidence tier summary: `●` high, `◐` medium, `○` low
**And** top findings with tier bullet, description, percentage
**And** "Next Steps" with copy-paste commands
**And** summary line and database path

### Story 4.4: Output Formatting, Verbosity & Error Patterns [COMPLETED]

As a **developer**,
I want consistent CLI formatting with verbosity control,
So that output is readable with detail available on demand.

**Acceptance Criteria:**

**Given** any Seshat CLI command
**Then** section headers use box-drawing format
**And** code/config in bordered boxes
**And** colors: errors red, warnings yellow. `NO_COLOR` respected.
**And** `--quiet`: errors + summary only. `--verbose`: adds skipped files, detector details, timing.
**And** errors: `error: {message}` + `hint:` lines
**And** `seshat --version`: `seshat {version} ({hash})`

---

## Epic 5: MCP Server, Serve Command & Core Tools [COMPLETED]

Developer starts Seshat as MCP server and AI agent can query project context and conventions.

> All stories (5.1-5.8) implemented. Stories 5.1-5.7 via Ralph Loop (see `.ralph/tasks/prd-mcp-server-core-tools.md`). Story 5.8 (smart DB discovery + forward-compatible tool schemas) implemented as tech spec. SSE/HTTP transports deferred to future epic.

### Story 5.1: MCP Server & `seshat serve` Command

As a **developer**,
I want to run `seshat serve` to start the MCP server,
So that AI agents can connect and query my project.

**Acceptance Criteria:**

**Given** a scanned project
**When** `seshat serve` runs
**Then** startup shows: version, loaded repos, watcher status, MCP transports
**And** MCP server starts via `rmcp` with stdio + SSE + HTTP
**And** `Ready. Press Ctrl+C to stop.` displayed
**And** Ctrl+C: graceful shutdown per ADR-21
**And** no scanned projects: error with suggestion

### Story 5.2: Response Envelope & Error Handling

As an **AI agent developer**,
I want consistent JSON response envelope,
So that I can parse any tool with one schema.

**Acceptance Criteria:**

**Given** any MCP tool call
**Then** success: `{status, tool, repo, branch, scope, duration_ms, data, metadata}`
**And** error: `{status: "error", tool, repo, error: {code, message, suggestion}}`
**And** `metadata` includes `next_steps: Vec<String>` — context-aware hints for next tool call (FR69)
**And** input validation before graph logic
**And** every call logged via tracing

### Story 5.3: `query_project_context` Tool

As an **AI agent**,
I want to query project context,
So that I understand the project's stack and structure.

**Acceptance Criteria:**

**Given** a scanned project
**When** agent calls `query_project_context`
**Then** `data` contains: languages, modules, dependencies (with canonical per domain), submodules, conventions_count, precision
**And** `data.golden_files[]`: top files by convention compliance count, with `{path, conventions_count, last_modified}` (FR64)
**And** optional focus area filters results
**And** response <1 second

### Story 5.4: `query_convention` Tool

As an **AI agent**,
I want to query conventions for a topic,
So that I know how things are done before generating code.

**Acceptance Criteria:**

**Given** a scanned project
**When** agent calls `query_convention` with topic
**Then** `data.conventions[]`: id, nature, weight, confidence, adoption, trend (rising/stable/declining/unknown), description, source, user_confirmed, examples with snippets
**And** FTS5 search matches topic against descriptions
**And** results include both auto-detected conventions AND user-recorded decisions
**And** empty result = success with empty array

### Story 5.5: `record_decision` MCP Tool

As an **AI agent**,
I want to record conventions and decisions that automated detectors cannot discover,
So that project-specific rules are captured and enforced. (FR65, ADR-27)

**Acceptance Criteria:**

**Given** a scanned project
**When** agent calls `record_decision` with description, nature (Decision/Convention/Rule), weight (Rule/Strong), category, optional examples and reason
**Then** new knowledge node created with `ext_data.source = "user"`, `ext_data.user_confirmed = true`
**And** node is immediately active in `validate_approach` checks
**And** node is never overwritten or deleted by automated re-scanning
**And** response confirms creation with node ID
**And** `metadata.next_steps` suggests: "Use `validate_approach` to verify this decision is now enforced"

### Story 5.6: `update_decision` and `remove_decision` MCP Tools

As an **AI agent**,
I want to update or remove previously recorded decisions,
So that the knowledge graph stays current with team agreements. (FR66)

**Acceptance Criteria:**

**Given** an existing user-recorded decision
**When** agent calls `update_decision` with ID and changed fields
**Then** decision updated, re-indexed in FTS5
**When** agent calls `remove_decision` with ID and reason
**Then** decision soft-deleted with reason preserved
**And** only user-recorded decisions (source = "user") can be updated/removed via these tools
**And** attempts to modify auto-detected conventions return informative error

### Story 5.7: Agent Protocol Documentation [COMPLETED]

As an **AI agent developer**,
I want clear instructions for when and how to use `record_decision`,
So that the understand → work → update loop is followed correctly.

**Acceptance Criteria:**

**Given** the Seshat MCP server documentation
**Then** protocol documented: (1) query conventions before work, (2) do work, (3) if you discover a new convention not in the graph, suggest recording it
**And** examples provided for common scenarios: wrapper conventions, architectural decisions, team agreements
**And** documentation included in MCP server `list_tools` descriptions

### Story 5.8: Smart DB Discovery & Forward-Compatible Tool Schemas

As a **developer**,
I want `seshat serve` to automatically detect the correct project from my working directory,
So that I don't accidentally serve the wrong project's conventions.

As an **AI agent developer**,
I want `repo` and `scope` parameters visible in MCP tool schemas,
So that agents are prepared for multi-repo mode without schema changes.

**Acceptance Criteria:**

**Given** multiple scanned projects in XDG data directory
**When** `seshat serve` runs without arguments from a project directory
**Then** project auto-detected from cwd name → `{name}.db` in XDG
**And** if cwd is subdirectory: walk up to `.git` root, use repo name
**And** if single DB in repos dir: use it unambiguously
**And** if multiple DBs and no match: error listing available projects with hint

**Given** `seshat serve my-project` or `seshat serve ~/Projects/my-project`
**Then** `repo` positional argument resolves as project name or directory path

**Given** any MCP tool schema
**Then** all 5 request structs include optional `repo: Option<String>` and `scope: Option<String>`
**And** both fields visible in `list_tools` JSON Schema output
**And** both fields ignored in handler logic (Epic 5 single-repo mode)

**And** shared `crate::db` module extracts `xdg_repos_dir()`, `project_name()`, `find_git_root()` from scan.rs
**And** `scan.rs` uses shared utilities instead of private copies

**Tech spec:** `_bmad-output/implementation-artifacts/tech-spec-serve-db-discovery.md`

---

## Epic 6: Submodule Support & Scoped Queries [COMPLETED]

Submodules scanned automatically into separate .db files. AI agent queries and writes routed to correct knowledge graph via scope detection (file_path auto-detect or explicit scope parameter). `seshat status` shows project tree.

> **Scope revised from original plan:** Original Epic 6 was "Multi-Repository & Submodule Support" with RepoRegistry for simultaneous multi-project serving. Revised to focus on submodule support within a single project. Multi-project daemon mode deferred to future epic. Stories 6.1-6.3 below reflect the original plan; actual implementation followed 12 user stories in the Ralph PRD (see `.ralph/tasks/prd-submodule-support-scoped-queries.md`).

> All 12 user stories (US-001 through US-012) implemented via Ralph Loop. Submodule DB isolation, parallel scanning, change detection (commit_hash compare), scope resolution module, file_path auto-scope in all 5 tools, serve with submodule connections, `seshat status` command, and repo parameter activation — all operational.

### Story 6.1: RepoRegistry & Multi-Repo Management [REVISED → Submodule DB Structure]

*Original story revised. Multi-repo RepoRegistry deferred. Implemented as submodule DB isolation instead.*

As a **developer**,
I want each submodule stored in a separate database with full scope isolation,
So that submodule conventions don't mix with root project.

**Acceptance Criteria (as implemented):**

**Given** a project with git submodules
**When** `seshat scan` runs
**Then** root DB at `$XDG_DATA/seshat/repos/{project}.db`, submodule DBs at `repos/{project}/{mount_path}.db`
**And** `submodules` table in root DB links parent ↔ child (relative_path, commit_hash, db_path)
**And** `repo_metadata` table stores summary stats per DB (file_count, convention_count, last_scan_time)
**And** submodules scanned in parallel for performance
**And** changed submodule commit triggers automatic rescan

### Story 6.2: Submodule Detection & Child Knowledge Graphs [COMPLETED]

As a **developer**,
I want Seshat to auto-detect submodules and create separate knowledge graphs,
So that submodule conventions don't mix with root project.

**Acceptance Criteria:**

**Given** a project with git submodules
**When** `seshat scan` runs
**Then** `.gitmodules` parsed for submodule paths
**And** each submodule gets own .db file with full Seshat schema
**And** submodule metadata stored in root project's `submodules` table
**And** scan report shows submodules section with per-submodule stats

### Story 6.3: Auto-Scope Detection & Query Routing [COMPLETED]

As an **AI agent**,
I want queries automatically scoped by file path,
So that I get relevant conventions without manual scope.

**Acceptance Criteria:**

**Given** project with submodules
**When** query with `file_path` in root project → scope = root
**When** query with `file_path` in submodule directory → scope = that submodule
**When** query without file_path → scope = root (default)
**And** optional explicit `scope` parameter supported
**And** all 5 MCP tools support `file_path` and `scope` parameters

---

## Epic 6.5: MCP Call Logging for Dogfooding (Added 2026-04-03) [COMPLETED]

Purpose-built JSONL call log for understanding MCP tool usage patterns during dogfooding. Dedicated `CallLogger` component (not tracing) captures full input and response summary metrics. Opt-in via `--call-log` CLI flag or config.

> All 5 user stories (US-001 through US-005) implemented via Ralph Loop (see `.ralph/tasks/prd-mcp-call-logging.md`). Enabled locally in `opencode.json` logging to `.seshat/call-log.jsonl`.

**FR Coverage:** FR71 (MCP call logging), FR72 (call log opt-in)
**ARCH:** ADR-30

### Story 6.5.1: MCP Call Logging Implementation [COMPLETED]

As a **developer dogfooding Seshat**,
I want all MCP tool calls logged to a JSONL file with full input and response summary metrics,
so that I can analyze tool usage frequency, call sequences, error rates, and validate the tool API surface.

**Acceptance Criteria:**

**Given** `seshat serve --call-log` is active
**When** any MCP tool is called
**Then** one JSONL line appended to log file with: `ts`, `session` (per-serve hex ID), `seq` (monotonic counter), `tool`, `input` (full params), `duration_ms`, `status` (ok/error), `result` (tool-specific summary scalars), `error_code` (on failure)
**And** `--call-log` without path uses `$XDG_DATA_HOME/seshat/call-log.jsonl`
**And** `--call-log /path` uses specified path
**And** `[server] call_log` in config also activates; CLI overrides
**And** file opened in append mode (never truncates, multiple sessions accumulate)
**And** log write failure does not crash server (degrades gracefully)
**And** when `--call-log` not passed and config empty, zero cost — no timing, no serialization
**And** `CallLogger` module at `crates/seshat-mcp/src/call_logger.rs`
**And** tool-specific result summaries: `query_project_context` → `{language_count, convention_count, golden_file_count}`, `query_convention` → `{convention_count, decision_count}`, `record/update/remove_decision` → `{node_id}`
**And** `execute_tool` helper in `server.rs` deduplicates logging boilerplate across all 5 tools

**PRD:** `.ralph/tasks/prd-mcp-call-logging.md`

---

## Epic 7: Advanced MCP Tools — Validate, Patterns, Dependencies, Diff Impact

AI agent can validate approaches, find code patterns, and analyze dependencies — the killer features.

> All 4 stories (7.1-7.4) implemented. `query_code_pattern`, `validate_approach` with graduated response, proactive duplicate detection, and `query_dependencies` — all operational. Code review completed 2026-04-07 (deferred items in `_bmad-output/implementation-artifacts/code-review-epic7-deferred-2026-04-07.md`). Merged to main.

### Story 7.1: `query_code_pattern` Tool

As an **AI agent**,
I want to search for code patterns by name or description,
So that I find existing implementations before writing new code.

**Acceptance Criteria:**

**Given** a scanned project
**When** agent calls `query_code_pattern`
**Then** `data.patterns[]`: name, kind, file_path, line, end_line, is_public, snippet (with truncated flag), score
**And** `data.related_conventions[]`: conventions matching the query via FTS5
**And** FTS5 for keyword matching; vector search (if configured) for semantic
**And** `metadata`: query, search_type, pattern_count, convention_count, next_steps
> **Spec update (2026-04-07, code review):** Original spec had `existing_implementations[]` with `used_by` — removed because `patterns[]` already serves this purpose. `used_by` is implemented in `validate_approach.duplicates[]` where dependency context is relevant. `description` replaced with `name` + `kind` which provide more structured information.

### Story 7.2: `validate_approach` Tool — Graduated Response

As an **AI agent**,
I want to validate my approach before coding,
So that I avoid violations and duplication.

**Acceptance Criteria:**

**Given** a scanned project
**When** agent calls `validate_approach` with description
**Then** `verdict`: approved, rules_violated, warnings_found, info_only
**And** `ready`: boolean — `false` if rules violated OR confidence of matched conventions too low (FR70)
**And** `what_would_help`: array of actionable suggestions when `ready = false` (e.g., "Query convention 'error_handling' first", "Run scan to update stale data")
**And** `summary`: deterministic template-based counts
**And** fixed severity order: rules → contradictions → duplicates → conventions → decisions → observations
**And** duplicates include existing code snippets
**And** conventions include `examples[]` snippets and `trend` indicators
**And** scope supported via MCP transport layer (resolves to correct database connection; graph-layer functions receive pre-resolved conn/branch_id)
> **Spec update (2026-04-07, code review):** `correct_example` replaced with `examples[]` — current evidence model doesn't distinguish correct vs incorrect examples; `examples` + `trend` provide sufficient context. Scope clarified: MCP layer handles scope resolution and routes to the appropriate database connection, so graph functions don't need an explicit scope parameter.

### Story 7.3: Proactive Duplicate Detection

As an **AI agent**,
I want Seshat to warn about existing code matching my approach,
So that I don't recreate utilities.

**Acceptance Criteria:**

**Given** agent calls `validate_approach`
**When** similar functionality exists
**Then** `duplicates` section includes existing implementation with snippet
**And** detection via function name matching + FTS5 on descriptions
**And** only high-confidence matches included
**And** each duplicate shows `used_by` count (populated when `file_context` is provided; 0 otherwise to avoid expensive N×`query_dependencies` calls per duplicate)
> **Spec update (2026-04-07, code review):** `used_by` is conditionally populated — computing it requires one `query_dependencies` call per duplicate, each loading full IR. Without `file_context`, this cost is unjustified.

### Story 7.4: `query_dependencies` Tool

As an **AI agent**,
I want to analyze dependencies of a module or function,
So that I understand blast radius of changes.

**Acceptance Criteria:**

**Given** a scanned project
**When** agent calls `query_dependencies` with path
**Then** `dependents[]`: file_path, line, import_names[]
**And** `dependencies[]`: file_path, import_names[], resolved
**And** `blast_radius`: low (<3), medium (3-10), high (>10)
**And** `backward_compatibility_note` when dependents exist
> **Spec update (2026-04-07, code review):** Field names aligned with implementation: `file` → `file_path` (more precise), `import name` → `import_names[]` (one import statement can import multiple names). Added `resolved` flag on dependencies to indicate whether an import was resolved to a known file in IR.

### Story 7.5: `seshat status` Command [COMPLETED — moved to Epic 6]

> Implemented as Epic 6 US-011. Shows indexed projects tree with submodules, branch, file/convention counts, DB sizes. Watcher and server status deferred to Epic 9 (watcher) and future epic (daemon mode).

As a **developer**,
I want to check Seshat status,
So that I can monitor indexed projects and server state.

**Acceptance Criteria (as implemented in Epic 6):**

**Given** scanned projects
**When** `seshat status`
**Then** "Indexed Projects" as tree: name, branch, files, conventions, DB size (submodules indented)
**And** `--verbose` shows full DB paths
**And** when not scanned: helpful message

### Story 7.6: Optional Vector Search Provider

As a **developer**,
I want to optionally enable vector search for semantic code pattern matching,
So that `query_code_pattern` can find implementations by description, not just keywords.

**Acceptance Criteria:**

**Given** `[embedding]` section configured in `seshat.toml`
**When** vector search is enabled
**Then** `EmbeddingProvider` trait implemented (built-in via fastembed-rs, zero-config)
**And** `query_code_pattern` uses embeddings for semantic matching alongside FTS5
**And** when not configured, FTS5-only search works as default (zero-config)
**And** trait-based abstraction allows adding new providers without core changes

> **Spec update (2026-04-11, Epic 8):** Original spec listed Ollama and OpenAI as providers. Both removed — external HTTP providers require running daemons or API keys, contradicting local-first philosophy. Replaced with built-in `fastembed-rs` provider (all-MiniLM-L6-v2). Archive tag `archive/embedding-http-providers` preserves old implementation.

---

### Story 7.7: `map_diff_impact` MCP Tool (Added 2026-04-16)

As an **AI agent**,
I want to call `map_diff_impact()` before committing or during code review,
So that I understand which conventions and dependents are at risk from current uncommitted changes.

**Acceptance Criteria:**

**Given** a project with uncommitted changes
**When** `map_diff_impact()` called (no required arguments)
**Then** run `git diff --name-only HEAD` → list changed files
**And** for each changed file: load IR → extract exported symbols
**And** for each symbol: query dependents count + blast_radius (low/medium/high)
**And** return `changed_files`, `affected_symbols`, `convention_risks`, `blast_radius_summary`
**And** `convention_risks`: conventions whose source files are in the changed set
**And** `blast_radius_summary`: total dependent count + overall risk level

**Optional parameters:**
- `staged_only: bool` — diff only staged changes (default: false)
- `base: string` — diff against specific ref (default: HEAD)

**Response includes** `metadata.next_steps` as always.

**Implementation files:**
- New `crates/seshat-graph/src/diff_impact.rs`
- `crates/seshat-mcp/src/server.rs` — register tool
- Uses `gix` (already a dependency) for git diff operations

---

## Epic 8: Built-in Embeddings & Semantic Search Quality [COMPLETED]

**Goal:** Replace HTTP embedding providers (Ollama, OpenAI) with a zero-config built-in provider. Improve semantic search quality via richer embedding context.

**Status:** All 2 stories (8.1-8.2) implemented via BMad workflow. `fastembed-rs` built-in provider (all-MiniLM-L6-v2, 384-dim), richer embedding text (function signature + body snippet + file-level imports), HTTP providers removed. Code review findings D20-D22 deferred. Merged to main.

### Story 8.1: Built-in Embedding Provider

As a **developer**,
I want vector search to work out-of-the-box without external services,
So that `query_code_pattern` semantic search is available with zero configuration.

**Acceptance Criteria:**

**Given** `[embedding]` section in `seshat.toml` (or default config)
**Then** embeddings generated locally using `fastembed-rs` (all-MiniLM-L6-v2, 384 dim)
**And** no Ollama daemon, no OpenAI API key, no internet access required
**And** feature can be disabled at compile time via `--no-default-features`
**And** HTTP providers (Ollama, OpenAI) removed from codebase

### Story 8.2: Rich Embedding Text Context

As a **developer**,
I want embedding text to include full function signature, body preview, and imports,
So that semantic search finds implementations by functionality description.

**Acceptance Criteria:**

**Given** a function `handle_request(req, ctx) -> Response` in `src/handler.rs`
**Then** embedding text includes: visibility, async modifier, name, parameters, file path
**And** first 5 lines of body (for logic context)
**And** last 3 lines of body (for return value context)
**And** all file-level import module names (for domain context)
**And** format: `{vis}{async}fn {name}({params}) in {path}\n{body}\nuses: {imports}`

---

## Epic 9: CLI Utilities — Init, Agent Instructions, Uninstall, Auto-Scan **[MOSTLY COMPLETE]**

Developer can install, update, and uninstall Seshat integration for AI coding agents. `seshat status` was moved to Epic 6 (US-011).

**Status:**
- **Story 9.1** (`seshat init`): ✅ COMPLETE. 158 unit + 3 integration tests.
- **Story 9.2** (agent instructions): ✅ COMPLETE. 13 integration tests + 24 unit tests. Writes AGENTS.md/CLAUDE.md with idempotent markers, installs SKILL.md, registers hooks.
- **Story 9.4** (`seshat uninstall`): ✅ COMPLETE. 16 integration tests. Full reverse of init: removes MCP entries, instructions, skills, hooks. Supports all 4 clients + dry-run.
- **Story 9.5** (auto-scan): ✅ COMPLETE. 12 unit tests in MCP crate. `ScanState` state machine with Condvar blocking. Project size check (50k file limit). Watcher starts after scan.
- **Story 9.3** (`seshat update`): 🔴 DEFERRED. Requires GitHub Actions release pipeline planning. Not started.

**FRs covered:** FR46, FR71, FR72
**UX-DR covered:** UX-DR45 through UX-DR51

### Story 9.1: `seshat init` with Auto-Detection [COMPLETED]

As a **developer**,
I want `seshat init` to detect my AI clients and generate (or auto-patch) MCP configurations,
So that I can connect Seshat to my AI tools in seconds without manually editing JSON files.

**Acceptance Criteria (as implemented):**

**Given** developer with AI coding clients installed
**When** `seshat init` without arguments from a project directory
**Then** smart scope: project-level config targeted if it already exists, global fallback otherwise
**And** detected clients listed (via `which` crate PATH lookup, Claude Desktop via app bundle check)
**And** for each JSON config: snippet shown in `── copy ──` block + `[y/N]` auto-patch with timestamped backup
**And** for each JSONC config: snippet shown only, explains why auto-patch not supported
**And** `seshat init <client>`: explicit client (claude-code, claude-desktop, opencode, cursor)
**And** `seshat init --project`: force project-level configs
**And** `seshat init --global`: force global configs
**And** `seshat init --dry-run`: show actions without writing
**And** no clients found: helpful message with supported client list
**And** patch error propagated as non-zero exit
**And** backup: `{filename}.seshat-backup.{timestamp_ms}` next to original

**Implementation:** `crates/seshat-cli/src/init.rs` (158 unit tests, 3 integration tests)

---

### Story 9.2: Agent Instructions in `seshat init` [COMPLETED]

**Status:** ✅ All 10 ACs implemented and verified. Dry-run UX improved to show specific paths. Backup for settings.json added. 24 unit tests in `instructions.rs` + 158 in `init.rs` + 11 integration tests in `init_instructions.rs`. Code review completed 2026-04-17.

As a **developer**,
I want `seshat init` to write Seshat usage instructions into my AI agent's config,
So that my AI agent knows when and how to use Seshat tools during coding sessions.

**Acceptance Criteria:**

**Given** `seshat init` (any scope/target)
**When** MCP config is written for an agent
**Then** write `rules/seshat.md` content into the agent's instruction file with idempotency
  markers `<!-- seshat:start -->` / `<!-- seshat:end -->` (append if absent, replace if present)
**And** target files: AGENTS.md (OpenCode, Codex), CLAUDE.md (Claude Code/Desktop), .cursorrules (Cursor)
**And** write `skills/seshat/SKILL.md` to `~/.claude/skills/seshat/` and `~/.config/opencode/skills/seshat/`
**And** install soft SessionStart hook and PreToolUse hook for Claude Code
**And** `--dry-run` shows all planned writes (now with specific paths per file)
**And** `--skip-instructions` skips instruction/skill/hook writing (MCP config only)
**And** all content embedded in binary via `include_str!()`
**And** settings.json backup created before modification

**Source artifacts:** `rules/seshat.md`, `skills/seshat/SKILL.md`,
`rules/hooks/seshat-session-start`, `rules/hooks/seshat-pre-tool` (all created 2026-04-16)

**Implementation:** `crates/seshat-cli/src/instructions.rs` (24 unit tests), `crates/seshat-cli/src/init.rs` (158 unit tests), `crates/seshat-cli/tests/init_instructions.rs` (11 integration tests)

---

### Story 9.5: Auto-Scan on First MCP Call [COMPLETED]

As a **developer**,
I want Seshat to automatically scan my project on the first MCP tool call,
So that I get zero-config experience without running `seshat scan` manually.

**Status:** ✅ Fully implemented. `ScanState` state machine (`crates/seshat-mcp/src/server.rs` lines 75-172) with `Condvar` blocking semantics. `resolve_serve_db_or_project_root()` in `db.rs` returns `AutoScan` target when no DB exists. Background `tokio::spawn` for scan, watcher waits for scan completion. Project size guard (50k file limit). 12 unit tests in MCP crate.

**Acceptance Criteria:**

**Given** `seshat serve` starts in a directory with no existing DB
**Then** server starts successfully (no error, accepts MCP connections)

**Given** AI agent calls any Seshat MCP tool
**When** project not yet scanned
**Then** run `scan_project` synchronously (blocking) before responding
**And** return normal tool response once scan completes
**And** include `"auto_scanned": true, "first_run": true` in metadata
**And** watcher starts automatically after scan
**And** if project > `auto_scan_limit` (default 50,000 files):
  return error `"Project too large for auto-scan. Run: seshat scan"`

**Given** AI agent is in a git worktree directory, project not scanned
**Then** scan parent repo (not worktree dir), return scanning status with parent repo name

**Given** scan fails
**Then** return error with actionable message and `seshat scan --verbose` suggestion

---

### Story 9.3: `seshat update` — Version Check [DEFERRED]

As a **developer**,
I want Seshat to notify me when a newer version is available,
So that I don't run stale tooling with outdated MCP schemas.

**Acceptance Criteria:**

**Given** `seshat serve` starts
**When** cached version check older than 24h
**Then** background task fetches crates.io API (non-blocking)
**And** if newer: print notice at startup + `_notice` field in first MCP response
**And** if network unavailable: silently skip
**And** `seshat update` CLI: explicit check, print result, exit 0

---

### Story 9.4: `seshat uninstall` — Clean Removal [COMPLETED]

As a **developer**,
I want `seshat uninstall` to cleanly remove all Seshat configuration,
So that I can remove Seshat without manually editing config files.

**Status:** ✅ Fully implemented. `crates/seshat-cli/src/uninstall.rs` (1,604 lines), 16 integration tests in `tests/uninstall.rs`. Handles all 4 clients, 5 uninstall target types (MCP entries, instructions, skills, hook scripts, hook registrations). Includes `claude mcp remove` CLI integration, dry-run, and end-to-end "reverse init" flow test.

**Acceptance Criteria:**

**Given** `seshat uninstall [client] [--global | --project] [--dry-run]`
**Then** remove `seshat` MCP entry from all detected agent configs
**And** remove `<!-- seshat:start -->...<!-- seshat:end -->` block from instruction files
**And** remove `~/.claude/skills/seshat/` and OpenCode equivalent
**And** remove seshat hooks from `~/.claude/settings.json` and hook scripts
**And** does NOT remove binary or `.seshat/*.db` files
**And** same `[y/N]` confirmation UX as init

---

## Epic 10: File Watcher & Incremental Updates **[COMPLETED]**

Seshat watches the project directory for changes and updates the knowledge graph incrementally — hot tier for code structure, warm tier for convention aggregates. No manual re-scan needed.

**Status:** All stories implemented. `seshat-watcher` crate: 1,176 lines, 15 tests.
Hot tier (notify-debouncer-full, 500ms), warm tier (30s interval), bulk rescan (>N events in 2s).
Watcher runs as background tokio task in `seshat serve`. Enabled by default.

**FRs covered:** FR7, FR8, FR9
**NFR covered:** NFR6, NFR7

### Story 10.1: File Watcher & Hot Tier [COMPLETED]

As a **developer**,
I want Seshat to detect file changes and update IR immediately,
So that AI agent always has current information.

**Implementation:** `crates/seshat-watcher/src/hot_tier.rs` (571 lines, 9 tests).

### Story 10.2: Warm Tier & Convention Recalculation [COMPLETED]

As a **developer**,
I want convention aggregates recalculated periodically,
So that confidence scores stay current.

**Implementation:** `crates/seshat-watcher/src/warm_tier.rs` (209 lines, 3 tests).

### Story 10.3: Bulk Change Detection [COMPLETED]

As a **developer**,
I want Seshat to handle git checkout gracefully,
So that branch switching doesn't overwhelm the watcher.

**Implementation:** `crates/seshat-watcher/src/lib.rs` (396 lines, 3 tests). Bulk threshold
configurable via `[watcher] bulk_change_threshold` (default: 20 events in 2s).

---

## Epic 11: Branch-Aware Knowledge Graph

Seshat maintains per-branch snapshots of the knowledge graph. Switching branches instantly switches context. Background sync catches up. Garbage collection cleans deleted branches.

**FRs covered:** FR17, FR18, FR19, FR20
**NFR covered:** NFR8

### Story 11.1: Branch Detection, Snapshot Creation & Git Worktree Support

As a **developer**,
I want Seshat to detect the real git branch and support git worktrees,
So that each branch/worktree has its own context and worktrees reuse the main repo's DB.

**Acceptance Criteria (original):**

**Given** Seshat watching a project
**When** `.git/HEAD` changes
**Then** `gix` reads new branch name
**And** existing snapshot: switch branch_id (instant, <2s)
**And** no snapshot: create by copying nodes + edges + files_ir with new branch_id
**And** background sync: compare hashes, re-parse changed files
**And** during sync: queries return from snapshot (possibly stale)

**Acceptance Criteria (added 2026-04-16 — worktree support):**

**Given** `seshat serve` starts in a git worktree directory
**When** `.git` is a file (not directory) containing `gitdir: <path>`
**Then** parse `gitdir:` → resolve canonical `.git` dir → find main repo root
**And** locate and use main repo's `.seshat/seshat.db`
**And** read actual branch name from worktree's `HEAD` file
**And** pass real `BranchId` (not `"main"`) to orchestrator and watcher

**Given** `seshat serve` starts in any git repo
**Then** read actual branch via `gix::discover` → `HEAD` reference
**And** all `BranchId::from("main")` hardcodes in `orchestrator.rs` replaced with detected branch

**Integration tests required:**
- `worktree_auto_init` — worktree detects main DB, correct branch_id
- `worktree_isolated_conventions` — changes in worktree branch don't affect main branch
- `multiple_worktrees_same_db` — 2 worktrees + main share DB without corruption

**Implementation files:**
- `crates/seshat-cli/src/db.rs` — `find_git_root_or_worktree()` (handles `.git` file)
- `crates/seshat-cli/src/serve.rs` — detect real branch via `gix`
- `crates/seshat-scanner/src/orchestrator.rs` — replace 8× `BranchId::from("main")`

### Story 11.2: Branch Snapshot Garbage Collection

As a **developer**,
I want Seshat to clean up deleted branch snapshots,
So that database size doesn't grow unbounded.

**Acceptance Criteria:**

**Given** branch snapshots in database
**When** `gix` reports branch no longer exists locally
**Then** snapshot deleted: `DELETE FROM nodes/edges/files_ir WHERE branch_id = ?`
**And** GC runs on startup and periodically (every hour)
**And** main/master never garbage collected

---

## Epic 12: Interactive Convention Review (TUI)

Developer can interactively review detected conventions via TUI wizard — confirm, reject, partially confirm. Search/filter by keyword. Precision self-diagnostic shows calibration quality.

**FRs covered:** FR16, FR43, FR44, FR45
**UX-DR covered:** UX-DR15 through UX-DR33

### Story 12.1: TUI Review Wizard — Core Navigation & Actions

As a **developer**,
I want to interactively review conventions in a TUI,
So that I can calibrate Seshat's knowledge graph.

**Acceptance Criteria:**

**Given** a scanned project with conventions
**When** `seshat review`
**Then** ratatui TUI: bordered frame, title, progress counter
**And** convention card: name, nature, confidence, weight, code example, adoption stats
**And** keys: `y` confirm (→Strong), `n` reject (→Observation), `p` partial, `s` skip, `↑↓` navigate, `q` finish

### Story 12.2: TUI Search/Filter & Precision Diagnostic

As a **developer**,
I want to search conventions and see precision after review,
So that I can find specific conventions and know calibration quality.

**Acceptance Criteria:**

**Given** TUI review open
**When** `/` pressed → search input, real-time keyword filter
**And** `Enter` selects, `Esc` clears filter
**When** `q` pressed → summary: `✓ Confirmed {n}`, `✗ Rejected {n}`, `~ Partial {n}`, `⊘ Skipped {n}`
**And** precision: `confirmed / (confirmed + rejected + partial)`
**And** >= 70%: `✓ Seshat is calibrated and ready to use`
**And** < 70%: `⚠ Low precision` warning
**And** knowledge graph updated with all decisions
