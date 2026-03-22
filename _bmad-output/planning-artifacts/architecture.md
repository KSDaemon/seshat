---
stepsCompleted: [1, 2]
inputDocuments: [prd.md, product-brief-seshat-2026-03-16.md]
workflowType: 'architecture'
project_name: 'Seshat'
user_name: 'Kostik'
date: '2026-03-22'
---

# Architecture Decision Document

_This document builds collaboratively through step-by-step discovery. Sections are appended as we work through each architectural decision together._

## Project Context Analysis

### Requirements Overview

**Functional Requirements:**
62 FRs across 8 capability areas, milestone-tagged M0-M3:
- **Scanning & Indexing** (FR1-FR12, FR55): Core data pipeline — Tree-sitter AST parsing, dependency manifest analysis, module detection, documentation ingestion, .gitignore respect, graceful degradation on parse failures
- **Knowledge Graph** (FR13-FR20, FR56): Two-dimensional typing (Nature×Weight), typed edges, confidence scoring, branch-aware snapshots with instant switch and GC, Decision reasoning storage
- **Convention Detection** (FR21-FR30): 8 trait-based detectors (dependency usage, imports, error handling, naming, exports, logging, tests, file structure) × 4 languages, cross-referencing code with documentation
- **MCP Server & Tools** (FR31-FR39): 5 tools via stdio/SSE/HTTP, structured JSON responses, proactive duplicate detection in validate_approach, informative errors
- **CLI Interface** (FR40-FR46): scan, serve, status, review (TUI), init, --version
- **Multi-Repo & Submodules** (FR47-FR48, FR57-FR62): Path-based repo ID, child knowledge graphs for submodules, auto-scope detection by file path, optional explicit scope
- **Search & Data** (FR49-FR52): FTS5 default, optional vector search, automatic DB backups
- **Configuration** (FR53-FR54): Optional config file, zero-config defaults

**Non-Functional Requirements:**
34 NFRs driving architectural decisions:
- **Performance**: Parallel scanning (all cores), scan <60s/100kLOC, MCP P95 <1s, hot tier <1s, warm tier <30s, branch switch <2s, memory <500MB scanning / <100MB serving, DB <50MB/100kLOC
- **Reliability**: Transactional writes (SQLite WAL), interrupted scan recovery, daily backups, no resource leaks in long-running server
- **Observability**: Structured logging via `tracing` at all levels, tool call logging with duration
- **Integration**: MCP protocol compliance, consistent JSON envelope, cross-platform (macOS/Linux/Windows), upstream Tree-sitter grammars, standard SQLite
- **Compatibility**: Automatic DB schema migration from any previous version
- **Maintainability**: Modular detector architecture (trait-based, pluggable), thin MCP integration layer, self-scanning CI

**Scale & Complexity:**
- Primary domain: CLI tool + MCP server (backend only, no frontend)
- Complexity level: Upper-Medium — concentrated in convention detection engine and validate_approach graduated response generation
- Estimated architectural components: 9 Rust crates in workspace
- Dual interface: Agent-facing (structured JSON, <1s latency) + Developer-facing (colored CLI, TUI)

### Technical Constraints & Dependencies

| Constraint | Impact |
|-----------|--------|
| **Rust, single binary** | All dependencies compiled in. No runtime deps. Cross-compilation required for 5 targets. |
| **SQLite embedded** | No external DB. Single file per repo. WAL mode for concurrency. FTS5 built-in. |
| **Tree-sitter grammars** | Compiled into binary for 4 languages. Grammar quality depends on upstream. |
| **MCP library (Rust)** | Transport layer delegated to library. Thin integration layer to minimize coupling. |
| **Local-first, no telemetry** | No network calls except optional embedding provider. All data stays on disk. |
| **Solo developer** | Architecture must be modular enough for incremental development. One crate at a time. |

### Cross-Cutting Concerns Identified

1. **Incrementality** — Everything must work incrementally: file watcher → hot tier AST updates → warm tier convention recalculation → branch snapshot sync. Full re-scan is only first-time.

2. **Observability** — `tracing` instrumented throughout all code paths. Every MCP tool call logged with repo, tool name, duration, result summary. Configurable verbosity.

3. **Error handling & graceful degradation** — One bad file must not crash the scan. One missing grammar must not prevent partial analysis. One corrupted convention must not poison all responses. Rust's `Result` type enforced throughout.

4. **Backward compatibility** — SQLite schema versioned. Sequential migrations from any previous version. No breaking changes to MCP tool response format without major version bump.

### Architectural Insights (from Party Mode)

**Three-Layer Architecture:**
- **Layer 1 (Parsing)**: Tree-sitter → Intermediate Representation (`ProjectFile`)
- **Layer 2 (Detection)**: IR → Convention Detectors → Knowledge Graph nodes
- **Layer 3 (Intelligence)**: Graph queries → Graduated responses → MCP tool output

**Intermediate Representation (IR):**
Normalized, language-agnostic representation of parsed code (`ProjectFile` struct). Decouples detectors from Tree-sitter, enables unit testing without parsing, allows future parser swaps.

**Preliminary Crate Structure:**

```
seshat/
├── seshat-core/        # Types, traits, IR (ProjectFile), KnowledgeNode, edges
├── seshat-scanner/     # Tree-sitter → IR, per-language parsers
├── seshat-detectors/   # Convention detectors on IR, trait-based, pluggable
├── seshat-storage/     # SQLite interface, migrations, FTS5, optional vector
├── seshat-graph/       # Knowledge graph logic, queries, graduated responses
├── seshat-mcp/         # MCP server, thin tool handlers
├── seshat-watcher/     # File watcher, incremental, hot/warm tiers
├── seshat-cli/         # CLI, TUI review, colored output
└── seshat/             # Binary, wiring, config, startup
```

**Dependency graph (no cycles):**
```
core ← base types
storage ← core
scanner ← core (produces IR)
detectors ← core (consumes IR, produces KnowledgeNodes)
graph ← core, storage (queries, intelligence)
watcher ← scanner, detectors, graph (orchestrates pipeline)
mcp ← graph (thin handlers)
cli ← graph, scanner, mcp (user commands)
seshat (binary) ← all crates
```

**Key principle:** Graph crate = intelligence (all query logic, duplicate detection, graduated responses). MCP crate = thin plumbing (parse input, call graph, format output).
