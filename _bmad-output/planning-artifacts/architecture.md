---
stepsCompleted: [1, 2, 3, 4, 5, 6, 7]
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

---

## Implementation Patterns & Consistency Rules

### Naming Patterns

**Database (SQLite):**
- Table names: `snake_case`, plural (`nodes`, `edges`, `files_ir`)
- Column names: `snake_case` (`branch_id`, `content_hash`, `edge_type`)
- Index names: `idx_{table}_{column}` (`idx_nodes_branch_id`, `idx_edges_source_id`)

**Rust Code:**
- Modules: `snake_case` (`convention_detector.rs`, `import_analyzer.rs`)
- Structs/Enums: `PascalCase` (`ProjectFile`, `KnowledgeNature`, `LanguageIR`)
- Functions: `snake_case` (`parse_to_ir`, `run_detectors`, `query_convention`)
- Constants: `SCREAMING_SNAKE_CASE` (`DEFAULT_CONFIDENCE_THRESHOLD`, `MAX_SNIPPET_LINES`)
- Trait names: `PascalCase`, descriptive (`ConventionDetector`, `EmbeddingProvider`, `NodeRepository`)

**MCP Tool names:** `snake_case` (`query_convention`, `validate_approach`)

**JSON response fields:** `snake_case` throughout — consistent with Rust serde defaults (`#[serde(rename_all = "snake_case")]`)

**Config file (`seshat.toml`):** Section and key names `snake_case` (`[scan]`, `confidence_threshold`)

### Rust-Specific Patterns

**Type-Safe IDs (Newtype Pattern):**
```rust
pub struct NodeId(i64);
pub struct EdgeId(i64);
pub struct BranchId(String);
```
Compiler prevents accidentally passing `EdgeId` where `NodeId` is expected. Safety net for late-night coding.

**Default Trait on All Config Structs:**
```rust
pub struct ScanConfig {
    pub languages: Vec<Language>,
    pub ignore_patterns: Vec<String>,
    pub max_file_size: usize,
}

impl Default for ScanConfig {
    fn default() -> Self { /* sensible defaults */ }
}
```
This is the "zero-config promise" at code level. Every config struct works with `Default::default()`.

**Repository Trait Pattern for Storage:**
```rust
pub trait NodeRepository {
    fn get_by_id(&self, id: NodeId, branch: &BranchId) -> Result<Option<KnowledgeNode>>;
    fn find_by_nature(&self, nature: KnowledgeNature, branch: &BranchId) -> Result<Vec<KnowledgeNode>>;
    fn insert(&self, node: &KnowledgeNode) -> Result<NodeId>;
    fn update(&self, node: &KnowledgeNode) -> Result<()>;
    fn delete(&self, id: NodeId, branch: &BranchId) -> Result<()>;
}
```
Storage crate exposes traits. SQLite implementation behind trait. Enables mock-based testing of graph logic.

**Version String with Git Hash:**
```rust
// build.rs captures git hash at compile time
const VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_HASH: &str = env!("GIT_HASH");
// Output: "seshat 0.1.0 (a3b4c5d6)"
```

### Structure Patterns

**Crate Organization:**
- One `lib.rs` per crate — public API surface
- Use `module_name.rs` style (Rust 2018+), not `mod.rs`
- Unit tests: `#[cfg(test)] mod tests` at bottom of each file
- Integration tests: `tests/` directory at crate root
- Test fixtures: `tests/fixtures/` with sample projects

**Module Documentation:**
Every `lib.rs` starts with `//!` doc comment — what the crate does, how it fits in the pipeline, key types. `cargo doc` generates navigable docs automatically.

```rust
//! # Seshat Scanner
//!
//! Parses source code files into intermediate representation (IR)
//! using Tree-sitter grammars. Produces `ProjectFile` structs
//! consumed by convention detectors.
```

**Test Helper Pattern:**
```rust
// seshat-core/src/test_helpers.rs
// Behind feature flag: #[cfg(any(test, feature = "test-helpers"))]

pub fn make_convention(nature: KnowledgeNature, confidence: f32) -> KnowledgeNode { ... }
pub fn make_project_file(language: Language) -> ProjectFile { ... }
```
Single source of test factories. Other crates use via `seshat-core = { ..., features = ["test-helpers"] }` in `[dev-dependencies]`.

### Error Handling Pattern

- Each crate defines its own error type in `error.rs` using `thiserror`
- Error propagation via `?` operator throughout library code
- `.unwrap()` / `.expect()` only in tests and `main.rs` initialization
- All errors implement `std::fmt::Display` for human-readable messages

```rust
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("Failed to parse {path}: {reason}")]
    ParseError { path: PathBuf, reason: String },
    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

### Logging Pattern

- Every public function entry: `tracing::debug!`
- Errors: `tracing::error!` with context
- Performance-sensitive: `tracing::trace!`
- MCP tool calls: `tracing::info!` with structured fields
- Use `#[tracing::instrument]` on public functions

```rust
#[tracing::instrument(skip(db), fields(repo = %repo_path))]
pub fn query_convention(db: &Database, repo_path: &str, topic: &str) -> Result<Response> {
    tracing::debug!(topic, "Querying convention");
    // ...
}
```

### Serialization Pattern

- `#[serde(rename_all = "snake_case")]` on all structs
- `#[serde(skip_serializing_if = "Option::is_none")]` for optional fields
- Empty arrays: include as `[]` (distinguishes "nothing found" from "not applicable")
- Null values: omit field entirely rather than including `null`

### Concurrency Pattern

- `rayon` for CPU-bound parallel work (scanning, detection) — sync
- `tokio` for async I/O (MCP server, file watcher) — async
- Never mix: scan pipeline is sync, server pipeline is async
- Bridge: `tokio::task::spawn_blocking` for calling sync scan code from async context

### Graceful Degradation Pattern

- One bad file → skip, log warning, continue scanning
- One failed detector → skip detector for this file, log, continue
- Corrupted IR → re-parse from source, update DB
- Missing Tree-sitter grammar → skip language, log warning

### Database Transaction Pattern

- All write operations wrapped in transactions
- Scan writes in batches per file (not one giant transaction)
- Read operations: no transaction needed (SQLite WAL allows concurrent reads)

### Anti-Patterns (Explicitly Forbidden)

| Anti-Pattern | Why | Instead |
|-------------|-----|---------|
| `.unwrap()` in library code | Panics kill MCP server | `?` with proper error types |
| `println!` for output | Bypasses tracing | `tracing::info!` or CLI formatter |
| Raw SQL scattered in code | Unmaintainable | Centralize in `seshat-storage` |
| Shared mutable state without sync | Data races | `Arc<RwLock<>>` or message passing |
| Blocking I/O in async context | Starves tokio | `spawn_blocking` |
| Hard-coded paths or config | Not portable | Config system or parameters |

### Enforcement

**CI Pipeline:**
1. Conventional commit validation
2. `cargo fmt --check` — formatting
3. `cargo clippy -- -D warnings` — lints as errors
4. `cargo test` — all tests pass
5. Seshat self-scan — dog-fooding quality gate
6. `cargo doc --no-deps` — documentation builds without warnings

**Local Pre-Commit Hooks (`.pre-commit-config.yaml`):**
- `trailing-whitespace`, `end-of-file-fixer`, `check-yaml`, `check-toml`, `check-merge-conflict`
- `conventional-pre-commit` on commit-msg stage — enforces conventional commit format
- `cargo fmt --check` + `cargo clippy -D warnings` on pre-commit

**Conventional Commits & Release Automation:**
- All commits follow conventional commits format (`feat:`, `fix:`, `docs:`, `refactor:`, etc.)
- `!` suffix = breaking change = major version bump
- `feat:` = minor, `fix:` = patch
- `release-plz` automates: version bump + CHANGELOG.md generation + git tag + GitHub Release
- Triple enforcement: local pre-commit hook + CI validation + release automation

---

## Project Structure & Boundaries

### Complete Project Directory Structure

```
seshat/
├── Cargo.toml                          # Workspace manifest
├── Cargo.lock
├── README.md
├── LICENSE
├── CHANGELOG.md                        # Auto-generated by release-plz from conventional commits
├── BACKLOG.md                          # Ideas parking lot
├── seshat.toml.example                 # Documented config with all defaults (commented out)
├── .gitignore
├── .pre-commit-config.yaml             # Pre-commit hooks: fmt, clippy, conventional commits
├── .cargo/
│   └── config.toml                     # Workspace-wide cargo settings
├── .github/
│   └── workflows/
│       ├── ci.yml                      # fmt, clippy, test, self-scan, commit lint
│       ├── release.yml                 # release-plz: changelog, version bump, cross-compile, publish
│       └── lint-workflows.yml          # actionlint — validates GitHub Actions workflows (runs only on .github/ changes)
│
├── crates/
│   ├── seshat-core/                    # Base types, traits, IR
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                  # Public API: types, traits, re-exports
│   │       ├── ir.rs                   # ProjectFile, CommonIR, LanguageIR enum
│   │       ├── knowledge.rs            # KnowledgeNode, KnowledgeNature, KnowledgeWeight
│   │       ├── edge.rs                 # Edge, EdgeType
│   │       ├── ids.rs                  # NodeId, EdgeId, BranchId (newtype)
│   │       ├── config.rs              # ScanConfig, DetectionConfig, ServerConfig (all impl Default)
│   │       ├── detector_result.rs     # DetectorResult, ConventionFinding — shared between detectors/storage/graph
│   │       ├── error.rs               # CoreError
│   │       └── test_helpers.rs        # Factory functions (behind "test-helpers" feature flag)
│   │
│   ├── seshat-scanner/                 # Tree-sitter → IR
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                  # scan_project(), scan_file()
│   │       ├── error.rs
│   │       ├── discovery.rs            # File discovery, .gitignore via `ignore` crate (WalkBuilder)
│   │       ├── parser/
│   │       │   ├── mod.rs              # Parser trait + language dispatch
│   │       │   ├── rust.rs             # Rust Tree-sitter → ProjectFile
│   │       │   ├── typescript.rs
│   │       │   ├── javascript.rs
│   │       │   └── python.rs
│   │       ├── manifest.rs             # Cargo.toml / package.json / pyproject.toml parsing
│   │       └── documentation.rs        # Markdown / JSON schema / OpenAPI ingestion
│   │
│   ├── seshat-detectors/               # Convention detection on IR
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs                  # run_all_detectors(), DetectorResult
│   │   │   ├── error.rs
│   │   │   ├── trait.rs               # ConventionDetector trait definition
│   │   │   ├── confidence.rs          # Frequency calculation, threshold → weight mapping
│   │   │   ├── dependency_usage.rs    # Detector #1
│   │   │   ├── imports.rs             # Detector #2
│   │   │   ├── error_handling.rs      # Detector #3
│   │   │   ├── naming.rs             # Detector #4
│   │   │   ├── exports.rs            # Detector #5
│   │   │   ├── logging.rs            # Detector #6
│   │   │   ├── tests_pattern.rs      # Detector #7
│   │   │   └── file_structure.rs     # Detector #8
│   │   └── tests/
│   │       └── fixtures/
│   │           ├── rust_samples/      # Small .rs files with known patterns
│   │           ├── typescript_samples/
│   │           └── python_samples/
│   │
│   ├── seshat-storage/                 # SQLite, migrations, FTS5, backup
│   │   ├── Cargo.toml
│   │   ├── migrations/                # refinery SQL migrations (embedded in binary)
│   │   │   ├── V1__initial_schema.sql
│   │   │   ├── V2__add_fts5.sql
│   │   │   └── ...
│   │   └── src/
│   │       ├── lib.rs                  # Database struct, connection management
│   │       ├── error.rs
│   │       ├── migrations.rs           # refinery embed_migrations! + runner
│   │       ├── repository/
│   │       │   ├── mod.rs             # Repository traits
│   │       │   ├── nodes.rs           # NodeRepository impl
│   │       │   ├── edges.rs           # EdgeRepository impl
│   │       │   ├── files_ir.rs        # FileIRRepository impl
│   │       │   └── branches.rs        # BranchRepository impl
│   │       ├── search.rs             # FTS5 queries
│   │       ├── backup.rs             # Automatic backup logic
│   │       └── schema.rs            # Table definitions reference
│   │
│   ├── seshat-graph/                   # Knowledge graph intelligence
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                  # High-level query API
│   │       ├── error.rs
│   │       ├── queries/
│   │       │   ├── mod.rs
│   │       │   ├── project_context.rs # query_project_context logic
│   │       │   ├── conventions.rs     # query_convention logic
│   │       │   ├── patterns.rs        # query_code_pattern logic
│   │       │   ├── validation.rs      # validate_approach + graduated response
│   │       │   ├── dependencies.rs    # query_dependencies logic
│   │       │   └── duplicates.rs      # Proactive duplicate detection
│   │       ├── aggregation.rs         # Convention aggregate recalculation (warm tier)
│   │       ├── cross_reference.rs    # Cross-reference code conventions vs documentation (FR30)
│   │       └── cache.rs              # LRU cache for IR and frequent queries (configurable max size)
│   │
│   ├── seshat-watcher/                 # File watching, incremental updates
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                  # start_watcher(), WatcherHandle
│   │       ├── error.rs
│   │       ├── hot_tier.rs           # Immediate file change → re-parse → update IR
│   │       ├── warm_tier.rs          # Periodic convention recalculation
│   │       ├── branch_detector.rs    # .git/HEAD watch + gix branch name
│   │       └── events.rs            # FileEvent enum, bulk change detection
│   │
│   ├── seshat-mcp/                     # MCP server, thin tool handlers
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                  # McpServer struct, start_server()
│   │       ├── error.rs
│   │       ├── envelope.rs           # Response/Error envelope formatting (ADR-9)
│   │       ├── scope.rs             # Auto-scope detection, submodule routing
│   │       ├── tools/
│   │       │   ├── mod.rs            # Tool registration with rmcp
│   │       │   ├── project_context.rs
│   │       │   ├── convention.rs
│   │       │   ├── code_pattern.rs
│   │       │   ├── validate.rs
│   │       │   └── dependencies.rs
│   │       └── summary.rs           # Deterministic summary generation
│   │
│   ├── seshat-cli/                     # CLI commands, TUI
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── error.rs
│   │       ├── commands/
│   │       │   ├── mod.rs
│   │       │   ├── scan.rs           # seshat scan <path>
│   │       │   ├── serve.rs          # seshat serve
│   │       │   ├── status.rs         # seshat status
│   │       │   ├── review.rs         # seshat review
│   │       │   └── init.rs           # seshat init <client>
│   │       ├── output.rs            # Colored formatting, progress bars
│   │       └── tui/
│   │           ├── mod.rs
│   │           ├── review_wizard.rs  # ratatui interactive review
│   │           └── widgets.rs
│   │
│   └── seshat-bin/                     # Binary entry point
│       ├── Cargo.toml                  # [[bin]] name = "seshat"
│       ├── build.rs                    # Git hash capture for version string
│       └── src/
│           ├── main.rs                # clap args, config loading, wiring, startup sequence
│           ├── config.rs             # seshat.toml loading, env var resolution
│           └── repo_registry.rs      # Multi-repo management: discover, register, route queries
│
└── tests/
    ├── fixtures/
    │   ├── rust_project/              # Full sample Rust project for E2E
    │   ├── typescript_project/
    │   └── python_project/
    ├── integration/
    │   ├── scan_test.rs
    │   ├── mcp_test.rs
    │   └── branch_test.rs
    └── snapshots/                     # insta snapshot files
```

### Architectural Boundaries

**Crate Dependency Flow:**
```
                         ┌──────────────────────────────────────┐
                         │            seshat-core                │
                         │  types, traits, IR, IDs, config       │
                         └───────┬──────┬──────┬──────┬─────────┘
                                 │      │      │      │
                    ┌────────────┘      │      │      └────────────┐
                    ▼                   ▼      ▼                   ▼
              seshat-scanner     seshat-detectors    seshat-storage
              (Tree-sitter→IR)   (IR→Conventions)   (SQLite, repos)
                    │                   │                   │
                    └───────────┬───────┘                   │
                                ▼                           │
                          seshat-graph  ◄───────────────────┘
                          (intelligence, queries)
                                │
                    ┌───────────┼───────────┐
                    ▼           ▼           ▼
              seshat-mcp   seshat-cli   seshat-watcher
              (MCP tools)  (CLI/TUI)   (file watch, hot/warm)
                    │           │           │
                    └───────────┼───────────┘
                                ▼
                           seshat-bin
                          (binary, wiring)
```

**Boundary Rules:**
- `seshat-core`: Zero dependencies on other seshat crates. Pure types and traits.
- `seshat-storage`: Only depends on `core`. Owns ALL SQLite interaction. No other crate touches SQL.
- `seshat-scanner`: Only depends on `core`. Owns Tree-sitter. Outputs IR.
- `seshat-detectors`: Only depends on `core`. Consumes IR, produces KnowledgeNodes.
- `seshat-graph`: Depends on `core` + `storage`. All intelligence. No MCP, no scanning.
- `seshat-mcp`: Depends on `graph` only. Thin handlers. No direct storage.
- `seshat-watcher`: Depends on `scanner`, `detectors`, `storage`, `graph`. Orchestrates pipeline.
- `seshat-cli`: Depends on `graph`, `scanner`, `mcp`. User commands.
- `seshat-bin`: Depends on everything. Wires components, loads config, starts runtime.

### FR to Structure Mapping

| FR Category | Primary Crate | Key Files |
|------------|---------------|-----------|
| Scanning & Indexing (FR1-FR12, FR55) | `seshat-scanner` | `parser/*.rs`, `discovery.rs`, `manifest.rs`, `documentation.rs` |
| Knowledge Graph (FR13-FR20, FR56) | `seshat-core` + `seshat-storage` + `seshat-graph` | `knowledge.rs`, `repository/nodes.rs`, `queries/*.rs` |
| Convention Detection (FR21-FR30) | `seshat-detectors` | `dependency_usage.rs` through `file_structure.rs` |
| MCP Server & Tools (FR31-FR39) | `seshat-mcp` | `tools/*.rs`, `envelope.rs`, `scope.rs` |
| CLI Interface (FR40-FR46) | `seshat-cli` | `commands/*.rs`, `tui/*.rs` |
| Multi-Repo & Submodules (FR47-FR62) | `seshat-storage` + `seshat-mcp` | `repository/branches.rs`, `scope.rs` |
| Search (FR49-FR50) | `seshat-storage` | `search.rs` |
| Configuration (FR53-FR54) | `seshat-bin` | `config.rs` |
| Incremental Updates (FR7-FR9) | `seshat-watcher` | `hot_tier.rs`, `warm_tier.rs`, `branch_detector.rs` |

### Data Flow

```
Source Files
    │
    ▼ (Tree-sitter, parallel via rayon)
seshat-scanner: parse → ProjectFile IR
    │
    ▼ (8 detectors sequentially per file)
seshat-detectors: analyze IR → ConventionFindings
    │
    ▼ (batch write, transactions)
seshat-storage: persist IR + findings → SQLite (.seshat.db)
    │
    ▼ (aggregate, cache)
seshat-graph: recalculate confidences, build graduated responses
    │
    ├──▶ seshat-mcp: tool handler → JSON envelope → AI agent
    └──▶ seshat-cli: command handler → colored output → developer
```

### Config File Structure (`seshat.toml.example`)

```toml
# Seshat Configuration
# All values shown are defaults. Uncomment and modify as needed.

[scan]
# exclude_patterns = ["vendor/", "generated/"]
# max_file_size_kb = 512

[detection]
# confidence_strong = 0.85
# confidence_moderate = 0.50
# confidence_weak = 0.20
# max_snippet_lines = 20

[server]
# log_level = "info"

[watcher]
# warm_tier_interval_seconds = 30

[backup]
# enabled = true
# interval_hours = 24
# keep_count = 3

# [embedding]
# provider = "ollama"
# model = "nomic-embed-text"
# url = "http://localhost:11434"

[cache]
# ir_cache_entries = 500
# query_cache_entries = 100
```

---

## Additional Architectural Decisions (from Validation)

### ADR-15: File Walker — `ignore` crate

Use the `ignore` crate (from ripgrep) for directory walking with built-in .gitignore support, not `walkdir` + manual gix gitignore glue. The `ignore` crate provides `WalkBuilder` with native gitignore, global gitignore, and custom ignore patterns — more ergonomic than gix's directory walker API. `gix` is used for git operations (branch detection, submodule discovery), not for file walking.

### ADR-16: IR Cache Versioning

Add a version prefix to serialized IR data in `files_ir.ir_data`:

```rust
const IR_SCHEMA_VERSION: u8 = 1;

fn serialize_ir(ir: &ProjectFile) -> Vec<u8> {
    let mut buf = vec![IR_SCHEMA_VERSION];
    bincode::serialize_into(&mut buf, ir).unwrap();
    buf
}

fn deserialize_ir(data: &[u8]) -> Result<ProjectFile> {
    if data[0] != IR_SCHEMA_VERSION {
        return Err(Error::StaleIR); // triggers re-parse
    }
    bincode::deserialize(&data[1..])
}
```

When `ProjectFile` struct changes → bump `IR_SCHEMA_VERSION` → all cached IR auto-invalidated → re-parsed on next access. No migration needed for IR, only for schema tables.

### ADR-17: DetectorResult Type (in seshat-core)

```rust
/// Output of a single convention detector for a single file.
/// Lives in seshat-core because it flows: detectors → storage → graph.
pub struct ConventionFinding {
    pub file_path: PathBuf,
    pub detector_name: String,          // e.g., "imports", "error_handling"
    pub nature: KnowledgeNature,        // Convention, Observation, Fact
    pub description: String,            // "Imports grouped: stdlib → external → internal"
    pub evidence: Vec<CodeEvidence>,    // Where in the file this was found
    pub follows_convention: bool,       // Does this file follow the detected pattern?
}

pub struct CodeEvidence {
    pub line: usize,
    pub end_line: usize,
    pub snippet: String,
}

/// Aggregate output of all detectors for a single file.
pub struct DetectorResults {
    pub file_path: PathBuf,
    pub findings: Vec<ConventionFinding>,
}
```

### ADR-18: Multi-Repo Server Management (RepoRegistry)

```rust
/// Lives in seshat-bin. Manages multiple repo databases.
pub struct RepoRegistry {
    repos: HashMap<PathBuf, Arc<Database>>,  // path → DB handle
    default_repo: Option<PathBuf>,
}

impl RepoRegistry {
    /// Register a repo (opens/creates DB, runs migrations)
    pub fn register(&mut self, path: PathBuf) -> Result<()>;
    
    /// Get DB for a repo path (exact match or longest prefix for submodules)
    pub fn get_db(&self, path: &Path) -> Option<Arc<Database>>;
    
    /// Route MCP query to correct DB based on repo field
    pub fn route_query(&self, repo: &str) -> Result<Arc<Database>>;
    
    /// List all registered repos
    pub fn list_repos(&self) -> Vec<&Path>;
}
```

**Registration flow:**
- `seshat scan /path/to/project` → registers repo in registry + scans
- `seshat serve` → loads all previously scanned repos from config/data directory
- MCP queries include `repo` field in request → RepoRegistry routes to correct DB

**Discovery:**
- On startup, scan data directory for existing `.seshat.db` files
- Each DB stores its repo path in metadata table
- Lazy loading: DB opened on first query, not all at startup

### ADR-19: SQLite Connection Management

```rust
/// Single connection wrapped in Arc<Mutex<>> for write access.
/// Readers use separate read-only connections via WAL mode.
pub struct Database {
    write_conn: Arc<Mutex<Connection>>,  // single writer
    read_pool: Vec<Connection>,           // multiple readers (WAL allows concurrent reads)
}
```

- All writes go through `write_conn` (serialized via Mutex — SQLite allows only one writer anyway)
- Reads use pooled read-only connections — no mutex contention for queries
- All DB access from async context via `tokio::task::spawn_blocking`
- Connection creation wrapped in `Database::open()` which runs migrations automatically

### ADR-20: MCP Tool Input Validation

Every MCP tool handler validates input before calling graph logic:

```rust
fn handle_query_convention(input: QueryConventionInput) -> Result<Response> {
    // Validate
    if input.topic.is_empty() {
        return Err(McpError::InvalidInput {
            code: "EMPTY_TOPIC",
            message: "Topic parameter is required",
            suggestion: "Provide a topic like 'imports', 'error_handling', 'logging'",
        });
    }
    
    // Route to correct DB
    let db = registry.route_query(&input.repo)?;
    
    // Call graph logic
    let result = graph::query_convention(db, &input.topic, &input.scope)?;
    
    // Format response
    Ok(envelope::success("query_convention", result))
}
```

Invalid input → structured error (ADR-9 error format) with `code`, `message`, `suggestion`. Agent can self-correct.

### ADR-21: Startup & Shutdown Sequence

**Startup (in `seshat-bin/src/main.rs`):**
```
1. Parse CLI args (clap)
2. Load config (seshat.toml or defaults)
3. Initialize tracing subscriber
4. Run database migrations (refinery)
5. Load RepoRegistry (discover existing DBs)
6. Start file watcher (hot tier + warm tier tasks)
7. Start MCP server (rmcp)
8. Log "Seshat ready" with version, repo count, transport info
```

**Shutdown (Ctrl+C / SIGTERM):**
```
1. Signal received → tokio::select! cancellation
2. Stop MCP server (drain active requests, max 5s timeout)
3. Stop file watcher (flush pending hot tier updates)
4. Flush warm tier (final convention recalculation if pending)
5. Close all DB connections gracefully
6. Log "Seshat stopped" with uptime
```

**Partial failure:** If watcher fails to start (e.g., inotify limit), MCP server still starts — queries work, but incremental updates are disabled. Logged as warning.

### ADR-22: Scan and Serve Interaction

- `seshat scan <path>` — one-shot command. Scans project, writes to DB, prints report, exits. No server.
- `seshat serve` — long-running server. Opens existing DBs (from previous scans), starts watcher + MCP server.
- `seshat serve --scan <path>` — convenience: scan first, then serve. Equivalent to `scan` followed by `serve`.
- While serving, file watcher handles all incremental updates. No manual re-scan needed.
- If DB doesn't exist when `serve` starts — error: "No scanned projects found. Run `seshat scan` first."

### ADR-23: Cross-Reference Convention vs Documentation (FR30)

Lives in `seshat-graph/src/cross_reference.rs`. Runs as a post-detection aggregation step:

1. Load all conventions detected from code (Nature = Convention/Observation)
2. Load all knowledge nodes ingested from documentation (Nature = Fact/Rule from FR11)
3. Compare: if a doc node says "always use X" but code convention shows Y with high adoption → create `Contradicts` edge between them
4. Surface contradictions in `validate_approach` response under `contradictions` section

Initial implementation (M0): simple keyword/topic matching between doc nodes and code conventions. Semantic matching (M2+) via embeddings.

---

## Architecture Validation Results

### Coherence Validation

- All technology choices are compatible (rusqlite+refinery, tree-sitter+rayon, tokio+rmcp, gix)
- 23 ADRs are internally consistent with no contradictions
- Implementation patterns align with technology choices
- Project structure supports all ADRs

### Requirements Coverage

- **62/62 FRs covered** — all functional requirements have architectural support with clear crate ownership
- **FR4 descoped** to dependency graphs for M0 (call graphs → M2+)
- **All NFRs addressed** — performance (rayon, caching), reliability (WAL, transactions), observability (tracing), compatibility (refinery migrations)

### Implementation Readiness

- M0 is implementable from this document alone
- All crate boundaries clear — no ambiguous ownership
- `DetectorResult` type defined in core (ADR-17)
- Multi-repo management designed (ADR-18)
- Startup/shutdown sequence explicit (ADR-21)

### Architecture Completeness Checklist

- [x] Project context thoroughly analyzed
- [x] Scale and complexity assessed
- [x] Technical constraints identified (2 C deps, solo dev, local-first)
- [x] Cross-cutting concerns mapped (incrementality, observability, error handling, backward compat)
- [x] 23 ADRs documented with rationale
- [x] Technology stack fully specified with versions
- [x] Integration patterns defined (crate boundaries, data flow)
- [x] Performance addressed (rayon, hot/warm tiers, LRU cache)
- [x] Naming conventions established (DB, Rust, JSON, config)
- [x] Structure patterns defined (crate org, error handling, logging, testing)
- [x] Communication patterns specified (MCP envelope, graduated responses)
- [x] Process patterns documented (graceful degradation, transactions, concurrency)
- [x] Complete directory structure with ~70 files
- [x] Component boundaries with dependency graph
- [x] FR to crate mapping complete
- [x] Data flow diagram
- [x] Config file structure documented
- [x] CI/CD pipeline defined (conventional commits, release-plz, actionlint)
- [x] Pre-commit hooks configured

### Architecture Readiness Assessment

**Overall Status:** READY FOR IMPLEMENTATION

**Confidence Level:** High

**Key Strengths:**
- Clean crate boundaries with no circular dependencies
- Boring technology stack — innovation only in knowledge graph and detection algorithms
- Explicit patterns prevent AI agent implementation conflicts
- Three-layer architecture (parsing → detection → intelligence) maps cleanly to crates
- Incremental by design (hot/warm tiers, content hash, branch snapshots)

**Areas for Future Enhancement:**
- Call graph extraction (M2+)
- Semantic cross-referencing via embeddings (M2+)
- Server-side connection pooling optimization (if performance requires)
- Plugin architecture for third-party detectors (Phase 3)

### Implementation Handoff

**AI Agent Guidelines:**
- Follow all 23 ADRs exactly as documented
- Respect crate boundaries — never cross architectural boundaries
- Use implementation patterns consistently (error handling, logging, naming, testing)
- Refer to this document for all architectural questions
- When in doubt, choose the simpler approach

**First Implementation Priority (M0):**
1. Workspace setup: `Cargo.toml`, all 9 crate scaffolds with `lib.rs`
2. `seshat-core`: types, IR, IDs, DetectorResult, config with Default
3. `seshat-storage`: SQLite schema (V1 migration), repository traits + impls
4. `seshat-scanner`: Tree-sitter parser for Rust (first language)
5. `seshat-detectors`: `ConventionDetector` trait + first 3 detectors (dependency_usage, imports, error_handling)
6. `seshat-cli`: `scan` command with analysis report output
7. `seshat-bin`: main.rs wiring, config loading
