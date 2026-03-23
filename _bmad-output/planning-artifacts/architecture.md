---
stepsCompleted: [1, 2, 3, 4]
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

---

## Technology Stack

### Primary Technology Domain

Rust CLI tool + MCP server. No web framework, no frontend. Backend-only with dual interface (CLI for humans, MCP for AI agents).

### Starter Template

There is no pre-built starter template for this project type. Project initialized as a Rust workspace from scratch with manual crate structure setup. Workspace setup is the first implementation task (Milestone M0).

```bash
cargo init --name seshat
# Then manually create workspace structure:
# Cargo.toml (workspace), seshat-core/, seshat-scanner/, etc.
```

### Technology Decisions

**Language & Runtime:**
- Rust (latest stable edition)
- Async runtime: `tokio` (multi-threaded, required by MCP library and file watcher)
- Parallelism: `rayon` for CPU-bound scanning and detection (work-stealing thread pool)

**Storage & Migrations:**
- `rusqlite` for SQLite with WAL mode, FTS5
- `refinery` for database migrations — SQL files embedded in binary via `embed_migrations!`, auto-applied on startup. Sequential versioned files: `V1__initial_schema.sql`, `V2__add_branch_snapshots.sql`, etc.
- Optional: trait-based `EmbeddingProvider` for vector search integration

**Parsing:**
- `tree-sitter` with language grammars compiled into binary:
  - `tree-sitter-rust`
  - `tree-sitter-typescript`
  - `tree-sitter-javascript`
  - `tree-sitter-python`
- Note: Tree-sitter runtime is a C dependency — unavoidable for multi-language AST parsing

**MCP Server:**
- `rmcp` — official Rust MCP SDK, latest specification support, millions of downloads
- Supports stdio + SSE + HTTP transports out of the box

**CLI:**
- `clap` (derive API) for argument parsing and `--help` generation
- `ratatui` + `crossterm` for TUI review wizard
- `indicatif` for progress bars during scanning
- `owo-colors` for colored output (respects `NO_COLOR`)

**Observability:**
- `tracing` + `tracing-subscriber` for structured logging
- Log levels: error/warn/info/debug/trace
- Configurable via `--log-level` flag or `SESHAT_LOG` environment variable

**File System & Git:**
- `notify` for cross-platform file watching
- `walkdir` for directory traversal
- `gix` (gitoxide) for git operations — pure Rust, no C dependencies:
  - Branch detection
  - Submodule discovery from `.gitmodules`
  - `.gitignore` parsing (native support, replaces separate `ignore` crate)

**Serialization:**
- `serde` + `serde_json` for JSON (MCP responses, config files)
- `toml` for optional config file (`seshat.toml`)

**Testing:**
- Built-in `#[test]` + `#[tokio::test]`
- `insta` for snapshot testing of full MCP responses
- `expect-test` for inline snapshot tests at unit level
- `tempfile` for test fixtures with temporary directories
- `assert_cmd` for CLI integration tests

**Build & Distribution:**
- GitHub Actions CI for cross-compilation (5 targets)
- `cross` for cross-platform builds
- Homebrew formula

### C Dependencies Assessment

| Dependency | Source | Avoidable? | Risk |
|-----------|--------|-----------|------|
| Tree-sitter runtime | C | No — no pure Rust multi-language parser exists | Low — mature, widely used |
| SQLite (via rusqlite) | C | No — no viable pure Rust embedded SQL DB | Low — most battle-tested DB in the world |
| ~~libgit2 (via git2)~~ | ~~C~~ | **Replaced** by `gix` (pure Rust) | N/A |

Two unavoidable C dependencies. Both mature and well-supported for cross-compilation. All other dependencies are pure Rust.

### Stack Philosophy

**Boring technology everywhere except where we innovate.** The entire infrastructure stack (`rusqlite`, `tree-sitter`, `clap`, `tokio`, `rayon`, `tracing`, `serde`) is proven, mature, and widely adopted. Innovation is concentrated in the knowledge graph schema, convention detection algorithms, and `validate_approach` graduated response logic — not in infrastructure choices.

---

## Core Architectural Decisions

### Category 1: Data Architecture

**ADR-1: Single `nodes` table with JSON extension column**
- One table for all knowledge node types (Fact, Convention, Decision, Preference, Observation)
- Columns: `id`, `branch_id`, `nature`, `weight`, `confidence`, `adoption_count`, `total_count`, `description`, `ext_data` (JSON for type-specific fields — e.g., `reasoning` for Decision, `adoption_rate` for Convention)
- Rationale: Simple queries, no joins between types, new Nature types = new enum value, not new migration

**ADR-2: Adjacency list for graph edges**
```sql
CREATE TABLE edges (
    id INTEGER PRIMARY KEY,
    source_id INTEGER REFERENCES nodes(id),
    target_id INTEGER REFERENCES nodes(id),
    edge_type TEXT NOT NULL,  -- RelatedTo, Updates, Contradicts, PartOf, DependsOn, Implements
    branch_id TEXT NOT NULL,
    weight REAL DEFAULT 1.0,
    metadata TEXT              -- JSON for edge-specific data
);
```
- Sparse graph, adjacency list is optimal. No adjacency matrix needed.

**ADR-3: Branch snapshots via `branch_id` column (full copy)**
- Every node and edge has a `branch_id` column
- Creating branch snapshot: `INSERT INTO nodes SELECT ... WHERE branch_id = 'main'` with new branch_id (~50-100ms for 5000 nodes)
- Switching branch: change WHERE clause in all queries — instant
- Cross-branch queries possible (e.g., diff conventions between branches)
- GC: `DELETE FROM nodes WHERE branch_id = ?` + same for edges
- Storage: ~30-40MB per branch for 100k LOC project. 10 branches = 300-400MB. Acceptable.

**ADR-4: IR stored in DB with LRU cache**
```sql
CREATE TABLE files_ir (
    id INTEGER PRIMARY KEY,
    branch_id TEXT NOT NULL,
    file_path TEXT NOT NULL,
    language TEXT NOT NULL,
    content_hash TEXT NOT NULL,  -- SHA256 of file content, for change detection
    ir_data BLOB NOT NULL,       -- serialized ProjectFile (bincode)
    updated_at INTEGER NOT NULL,
    UNIQUE(branch_id, file_path)
);
```
- IR persisted in DB — no full re-parse on restart
- `content_hash` enables incremental scan: hash match = skip, hash mismatch = re-parse
- LRU cache (in-memory) for frequently accessed IR — DB is backing store, cache is hot path
- Serialization format: `bincode` (fast, compact, Rust-native)

**ADR-5: Database migrations via `refinery`**
- SQL migration files in `migrations/` directory: `V1__initial_schema.sql`, `V2__...`, etc.
- Embedded in binary via `embed_migrations!`
- Auto-applied on startup — any previous version upgradeable to current

### Category 2: Convention Detection Pipeline

**ADR-6: Parallel scanning by file, sequential detectors per file**
```rust
files.par_iter()                      // rayon: parallel over files
    .map(|f| parse_to_ir(f))          // Tree-sitter → ProjectFile
    .map(|ir| run_all_detectors(&ir)) // 8 detectors sequentially per file
    .collect::<Vec<DetectorResults>>()
```
- 2000 files × 8 cores = ~250 files/core. Sufficient parallelism.
- Detectors on one file = microseconds. Parallelizing within a file adds overhead without benefit.

**ADR-7: Simple frequency-based confidence scoring (MVP)**
```
confidence = adoption_count / total_count
```
- Weight mapping (configurable thresholds in `seshat.toml`):
  - `> 0.85` → Strong
  - `0.50 - 0.85` → Moderate
  - `0.20 - 0.50` → Weak
  - `< 0.20` → Info (excluded from validate_approach or shown as informational)
- Formula lives in one place — easy to replace with weighted scoring later
- Architecture ready for future fields: `recency_weight`, `user_confirmed` boost

**ADR-8: Cross-language IR with common base + language enum**
```rust
pub struct ProjectFile {
    // Common for all languages
    pub path: PathBuf,
    pub language: Language,
    pub content_hash: String,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub functions: Vec<Function>,
    pub types: Vec<TypeDef>,
    pub dependencies_used: Vec<DependencyUsage>,
    // Language-specific
    pub language_ir: LanguageIR,
}

pub enum LanguageIR {
    Rust(RustIR),         // pub visibility, mod structure, derives, traits
    TypeScript(TypeScriptIR), // default exports, barrel exports, decorators, type-only imports
    JavaScript(JavaScriptIR), // CommonJS vs ESM, module.exports
    Python(PythonIR),     // __all__, __init__.py, type hints, decorators
}
```
- Common fields for universal detectors, enum for language-specific detectors
- Exhaustive match guarantees compile-time coverage — Rust warns on missing variants
- Adding new language = new enum variant + parser + detector implementations

### Category 3: MCP Response Architecture

**ADR-9: Unified JSON response envelope**

All MCP tools return the same envelope:
```json
{
  "status": "success | error",
  "tool": "query_convention",
  "repo": "/path/to/project",
  "branch": "main",
  "scope": "root | submodule_name",
  "duration_ms": 47,
  "data": { },
  "metadata": { "node_count": 3, "confidence_range": [0.72, 0.95] }
}
```

Error case:
```json
{
  "status": "error",
  "tool": "query_convention",
  "repo": "/path/to/project",
  "error": {
    "code": "REPO_NOT_SCANNED",
    "message": "Repository has not been scanned. Run `seshat scan` first.",
    "suggestion": "seshat scan /path/to/project"
  }
}
```

**ADR-10: Code snippets included in responses**
- Convention examples, pattern matches, and duplicate warnings include actual code snippets with file:line references
- Max snippet length: 20 lines (configurable). Truncated with `"truncated": true` flag.
- Agent gets both the rule and the example — no additional file reads needed

**ADR-11: `validate_approach` graduated response with deterministic summary**

Response structure (fixed ordering by severity):
```json
{
  "data": {
    "verdict": "approved | rules_violated | warnings_found | info_only",
    "summary": "Found: 2 convention warning(s), 1 duplicate(s) — use existing implementation(s).",
    "rules": [],
    "contradictions": [],
    "duplicates": [{ "message": "...", "existing": {"file": "...", "line": 23, "snippet": "..."}, "used_by": 14 }],
    "conventions": [{ "convention_id": "...", "severity": "should_fix", "message": "...", "confidence": 0.93, "correct_example": {"file": "...", "snippet": "..."} }],
    "decisions": [],
    "observations": []
  }
}
```

- `verdict` enum: agent branches on verdict without parsing all sections
- `summary`: deterministic template-based generation — counts + type names, no LLM needed
- Fixed ordering: rules → contradictions → duplicates → conventions → decisions → observations
- Both duplicates and conventions include code snippets showing the correct approach

### Category 4: Incremental Update Architecture

**ADR-12: Two independent tokio tasks for hot/warm tiers**

```
┌─────────────────┐     ┌──────────────────┐
│   Hot Tier Task  │     │  Warm Tier Task   │
│                  │     │                   │
│ notify events →  │     │ Timer (30s) →     │
│ re-parse file →  │     │ has_changes? →    │
│ update IR in DB →│     │ recalculate       │
│ update edges     │     │ convention        │
│                  │     │ aggregates        │
└─────────────────┘     └──────────────────┘
         │                        │
         └────── shared DB ───────┘
```

- Non-blocking, independent lifecycle
- Hot tier: <1s response to file changes
- Warm tier: 30s interval (configurable), only runs if `has_pending_changes`
- Consistency model: **eventual consistency** between tiers — MCP queries may see updated IR but stale convention confidence for up to 30 seconds. Acceptable.

**ADR-13: Pragmatic per-file convention invalidation**
- File changed → hot tier re-parses → new IR → re-run detectors on this file only → update per-file findings in DB
- Warm tier: single SQL query recalculates all convention confidence scores from per-file findings
- O(1) per file change, not O(N) full rescan

**ADR-14: Branch switch detection via `.git/HEAD` watch**
- `notify` watches `.git/HEAD` file — changes only on branch switch / checkout
- On change: `gix` reads new branch name → deterministic detection, no debounce guessing
- Flow: detect switch → check if snapshot exists → YES: switch branch_id + background sync → NO: create snapshot from current + background full diff
- During sync: agent gets responses from snapshot (possibly seconds stale). After sync: fully current.
