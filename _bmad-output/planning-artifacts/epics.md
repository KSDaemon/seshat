---
stepsCompleted: [1, 2]
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

**Total: 62 FRs** (M0: 24, M1: 17, M2: 9, M3: 12)

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
| FR7 | 9 | Incremental updates (hot/warm) |
| FR8 | 9 | File watcher real-time |
| FR9 | 9 | Bulk change detection |
| FR10 | 2 | SQLite storage |
| FR11 | 2 | Documentation file ingestion |
| FR12 | 2 | Graceful skip unparseable files |
| FR13 | 2 | 2D knowledge node typing |
| FR14 | 2 | Typed graph edges |
| FR15 | 2 | Confidence scoring |
| FR16 | 11 | Interactive convention review |
| FR17 | 10 | Per-branch snapshots |
| FR18 | 10 | Instant branch switch |
| FR19 | 10 | Background sync after switch |
| FR20 | 10 | GC deleted branches |
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
| FR31 | 5 | MCP server (stdio/SSE/HTTP) |
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
| FR42 | 7 | seshat status command |
| FR43 | 11 | seshat review TUI |
| FR44 | 11 | Review search/filter |
| FR45 | 11 | Precision self-diagnostic |
| FR46 | 8 | seshat init command |
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

**Coverage: 62/62 FRs mapped.**

## Epic List

### Epic 1: Development Infrastructure & Project Bootstrap
Seshat project is set up with Rust workspace, 9 crates, CI/CD pipeline, pre-commit hooks, and database migrations — enabling systematic development of all features.

**FRs covered:** FR53, FR54
**ARCH covered:** ARCH-1 through ARCH-6, ARCH-14 through ARCH-17, ARCH-21 through ARCH-23
**NFR covered:** NFR28, NFR29, NFR30, NFR32, NFR33, NFR34

### Epic 2: Code Scanning & Knowledge Graph
Developer can scan a project directory and Seshat builds a knowledge graph with parsed code, detected modules, dependencies, and documentation — the foundation of all intelligence.

**FRs covered:** FR1, FR2, FR3, FR4, FR5, FR10, FR11, FR12, FR55, FR13, FR14, FR15, FR56, FR51, FR52
**ARCH covered:** ARCH-7, ARCH-8, ARCH-18, ARCH-20
**NFR covered:** NFR1, NFR2, NFR3, NFR9, NFR11, NFR12, NFR13, NFR14, NFR15, NFR16, NFR27
**UX-DR covered:** UX-DR60, UX-DR61

### Epic 3: Convention Detection Engine
Seshat can automatically detect coding conventions from scanned code — import patterns, error handling, naming, dependencies, and more — assigning confidence scores and cross-referencing with documentation.

**FRs covered:** FR21, FR22, FR23, FR24, FR25, FR26, FR27, FR28, FR29, FR30
**ARCH covered:** ARCH-11
**NFR covered:** NFR30
*Stories span M0 (first 3 detectors), M1 (3 more), M2 (final 2). Each story is standalone.*

### Epic 4: CLI Scan Report & First Impression
Developer can run `seshat scan <path>` and see a beautiful, informative analysis report showing what Seshat discovered about their project — the "wow moment".

**FRs covered:** FR6, FR40
**UX-DR covered:** UX-DR1 through UX-DR14, UX-DR52 through UX-DR59, UX-DR87 through UX-DR89

### Epic 5: MCP Server, Serve Command & Core Tools
Developer can start Seshat as MCP server via `seshat serve` and AI agent can connect and query project context and conventions — the core value proposition.

**FRs covered:** FR31, FR32, FR33, FR38, FR39, FR41, FR49
**ARCH covered:** ARCH-9, ARCH-12, ARCH-13
**NFR covered:** NFR4, NFR5, NFR10, NFR17, NFR18, NFR19, NFR20, NFR21, NFR22, NFR23, NFR26
**UX-DR covered:** UX-DR34 through UX-DR39, UX-DR62 through UX-DR72, UX-DR84 through UX-DR86

### Epic 6: Multi-Repository & Submodule Support
Developer can scan multiple projects and Seshat manages them with namespace isolation. Submodules detected automatically. AI agent queries route to the correct knowledge graph.

**FRs covered:** FR47, FR48, FR57, FR58, FR59, FR61, FR62
**ARCH covered:** ARCH-10, ARCH-19
**UX-DR covered:** UX-DR8

### Epic 7: Advanced MCP Tools — Validate, Patterns, Dependencies
AI agent can validate approaches before coding, find code patterns by functionality, and analyze dependencies — the killer features that differentiate Seshat.

**FRs covered:** FR34, FR35, FR36, FR37, FR42, FR50, FR60
**UX-DR covered:** UX-DR73 through UX-DR83

### Epic 8: CLI Utilities — Status & Init
Developer can check status of indexed projects and watcher state via `seshat status`, and generate copy-paste-ready MCP configurations for detected AI clients via `seshat init`.

**FRs covered:** FR46
**UX-DR covered:** UX-DR40 through UX-DR51

### Epic 9: File Watcher & Incremental Updates
Seshat watches the project directory for changes and updates the knowledge graph incrementally — hot tier for code structure, warm tier for convention aggregates. No manual re-scan needed.

**FRs covered:** FR7, FR8, FR9
**NFR covered:** NFR6, NFR7

### Epic 10: Branch-Aware Knowledge Graph
Seshat maintains per-branch snapshots of the knowledge graph. Switching branches instantly switches context. Background sync catches up. Garbage collection cleans deleted branches.

**FRs covered:** FR17, FR18, FR19, FR20
**NFR covered:** NFR8

### Epic 11: Interactive Convention Review (TUI)
Developer can interactively review detected conventions via TUI wizard — confirm, reject, partially confirm. Search/filter by keyword. Precision self-diagnostic shows calibration quality.

**FRs covered:** FR16, FR43, FR44, FR45
**UX-DR covered:** UX-DR15 through UX-DR33
