# PRD: Epic 7 — Advanced MCP Tools: Validate, Patterns, Dependencies

## Introduction

**Type:** Feature

Add three new MCP tools (`query_code_pattern`, `validate_approach`, `query_dependencies`) and an optional vector search provider — the killer features that differentiate Seshat from every competitor. These tools enable AI agents to find existing code before writing new code, validate approaches before implementing, detect duplicates proactively, and understand blast radius of changes. Together they shift convention enforcement left — into the agent's planning phase, before a single line of code is written.

**Depends on:** Epics 1-6.5 (all completed). MCP server operational with 5 tools, submodule scoping, call logging. Knowledge graph populated with conventions, IR (functions, types, imports, exports), FTS5 index on conventions, golden files.

**FRs covered:** FR34, FR35, FR36, FR37, FR50, FR60, FR70
**Architecture:** ADR-26 (embedding search deferred to M2+, now implemented as optional provider)

## Goals

- AI agent can search for code patterns by name or description, finding existing implementations before writing new code
- AI agent can validate a proposed approach and receive a graduated response: rules violated → conventions → decisions → observations → duplicates
- AI agent receives a `ready` boolean and `what_would_help` array for evidence gating (FR70)
- AI agent can query dependencies for any file/module and see blast radius estimate
- Proactive duplicate detection warns about existing code matching the proposed approach
- Optional vector search via embedding provider (Ollama, OpenAI) enhances semantic matching when configured; FTS5-only works as zero-config default
- All new tools follow existing patterns: `execute_tool` pipeline, `ResponseEnvelope<T>`, call logging, scope-aware routing

## Story Dependency Graph

```
US-001 (query_code_pattern: graph layer + MCP tool)
  └─► US-003 (validate_approach: uses pattern search for duplicate detection)

US-002 (query_dependencies: graph layer + MCP tool) — independent

US-003 (validate_approach + duplicate detection: uses US-001 + conventions + decisions)

US-004 (vector search provider: enhances US-001 query_code_pattern)
```

## User Stories

### US-001: `query_code_pattern` — Code Pattern Search Tool

**Description:** As an AI agent, I want to search for code patterns by name or description so that I find existing implementations before writing new code.

**Acceptance Criteria:**
- [ ] New graph module `crates/seshat-graph/src/code_pattern.rs` with `query_code_pattern()` function
- [ ] Function signature: `pub fn query_code_pattern(conn: &Arc<Mutex<Connection>>, branch_id: &str, query: &str) -> Result<CodePatternData, GraphError>`
- [ ] **IR search**: deserialize all `files_ir` for the branch, search function names, type names, and export names against query tokens
- [ ] Scoring: exact match (1.0) > prefix match (0.7) > contains (0.4). Results sorted by score descending.
- [ ] Each result includes: `name`, `kind` (function/type/export), `file_path`, `line`, `end_line`, `is_public`, `snippet` (function signature or type definition), `score`
- [ ] Snippet truncation: max 10 lines, `truncated: true` flag when exceeding
- [ ] **Convention search**: also search existing `conventions_fts` via `search_conventions()` for related conventions
- [ ] Response structure: `data.patterns[]` (from IR) + `data.related_conventions[]` (from FTS5, reusing `ConventionResult` type)
- [ ] `metadata.query`, `metadata.pattern_count`, `metadata.convention_count`, `metadata.search_type` ("keyword" for V1)
- [ ] `metadata.next_steps`: e.g. "Call validate_approach to check if your approach conflicts with existing conventions"
- [ ] New MCP tool handler in `crates/seshat-mcp/src/tools/query_code_pattern.rs`
- [ ] Request struct: `query: String` (required), `kind: Option<String>` (filter: "function", "type", "export", or all), `scope: Option<String>`, `file_path: Option<String>`, `repo: Option<String>`
- [ ] Tool registered in `server.rs` with `#[tool(description = "...")]` and wired through `execute_tool`
- [ ] Call logger summary: `{pattern_count, convention_count}`
- [ ] Empty query returns `ErrorEnvelope` with `EMPTY_TOPIC` error code
- [ ] No results: return empty arrays (not an error)
- [ ] Unit tests: search finds function by exact name, by prefix, by substring; type search; convention results included; empty query error; no results case
- [ ] Integration test: register tool, call via MCP, verify response envelope
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes

### US-002: `query_dependencies` — Dependency Analysis Tool

**Description:** As an AI agent, I want to analyze dependencies of a file or module so that I understand the blast radius of my changes.

**Acceptance Criteria:**
- [ ] New graph module `crates/seshat-graph/src/dependencies.rs` with `query_dependencies()` function
- [ ] Function signature: `pub fn query_dependencies(conn: &Arc<Mutex<Connection>>, branch_id: &str, target_path: &str) -> Result<DependencyData, GraphError>`
- [ ] **Build dependency index**: deserialize all `files_ir` for the branch, build two maps:
  - Forward: `file_path → Vec<Import>` (what this file imports)
  - Reverse: `exported_module_or_name → file_path` (who provides this export)
- [ ] **Module resolution**: best-effort matching of import paths to file paths:
  - Relative imports (`./foo`, `../bar`): resolve against importing file's directory
  - Absolute project imports (`crate::`, `@/`, `from mypackage`): match against known file paths by stripping common prefixes
  - External package imports: excluded from file-level dependency graph (they're in `dependencies_used` already)
  - Unresolved imports: included in response with `resolved: false`
- [ ] **Direct dependencies only** (V1): for the target file, return:
  - `dependencies[]`: files that target imports from — `{file_path, import_names: Vec<String>, resolved: bool}`
  - `dependents[]`: files that import from target — `{file_path, import_names: Vec<String>, line}`
- [ ] `blast_radius` classification: `"low"` (<3 dependents), `"medium"` (3-10), `"high"` (>10)
- [ ] `blast_radius_count`: exact number of direct dependents
- [ ] `backward_compatibility_note`: present when dependents exist — "This file has N direct dependents. Changes to its public API may break them."
- [ ] `external_dependencies[]`: packages from `dependencies_used` for the target file — `{package, domain, import_path}`
- [ ] New MCP tool handler in `crates/seshat-mcp/src/tools/query_dependencies.rs`
- [ ] Request struct: `path: String` (required, file path relative to project root), `scope: Option<String>`, `file_path: Option<String>` (for auto-scope), `repo: Option<String>`
- [ ] Tool registered in `server.rs` with `execute_tool` pipeline
- [ ] Call logger summary: `{dependent_count, dependency_count, blast_radius}`
- [ ] `metadata.next_steps`: e.g. "Review dependents before changing public API", "Call validate_approach to check for convention violations"
- [ ] Error: target path not found in IR → `ErrorEnvelope` with `NODE_NOT_FOUND` code
- [ ] Unit tests: file with known imports → correct dependencies; file imported by others → correct dependents; blast radius calculation; unresolved imports flagged; file not in IR → error
- [ ] Integration test: register tool, call via MCP, verify response
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes

### US-003: `validate_approach` — Pre-Implementation Validation Tool

**Description:** As an AI agent, I want to validate my proposed approach before writing code so that I avoid convention violations, duplicates, and bad patterns.

**Acceptance Criteria:**
- [ ] New graph module `crates/seshat-graph/src/validate_approach.rs` with `validate_approach()` function
- [ ] Function signature: `pub fn validate_approach(conn: &Arc<Mutex<Connection>>, branch_id: &str, params: ValidateApproachParams) -> Result<ValidateApproachData, GraphError>`
- [ ] `ValidateApproachParams`: `description: String`, `file_context: Option<String>` (file being modified), `approach_type: Option<String>` (e.g. "new_function", "refactor", "dependency_add")
- [ ] **Graduated response** with fixed severity order:
  1. `rules[]` — conventions with `weight = Rule` that match the approach description. Violations here → `verdict = "rules_violated"`. Each includes: description, evidence snippet, severity "must_fix"
  2. `contradictions[]` — code vs doc contradictions from `edges` where `edge_type = Contradicts`. Each includes: code_convention, doc_convention, recommendation
  3. `duplicates[]` — existing code matching the approach description (uses `query_code_pattern` IR search internally). Each includes: name, file_path, line, snippet, `used_by` count (number of importers from US-002 dependency index). Only high-confidence matches (score > 0.6)
  4. `conventions[]` — matching conventions (via FTS5 on description). Each includes: description, weight, confidence, adoption rate, trend, correct_example snippet
  5. `decisions[]` — user-recorded decisions matching the topic. Each includes: description, nature, weight, reason
  6. `observations[]` — low-confidence observations matching. Each includes: description, confidence
- [ ] **Verdict logic**: `rules_violated` if any rules match → `warnings_found` if contradictions or high-weight conventions match → `info_only` otherwise → `approved` if nothing matches
- [ ] **Evidence gating (FR70)**:
  - `ready: bool` — `false` if verdict is `rules_violated` OR if matched conventions have `confidence < 0.5` (stale/unreliable data)
  - `what_would_help: Vec<String>` — actionable suggestions when `ready = false`, e.g. "Run seshat scan to update stale conventions", "Query convention 'error_handling' for more context", "Review rule: 'Always use thiserror for error types'"
- [ ] `summary`: deterministic template — e.g. "Found 2 rules, 0 contradictions, 1 duplicate, 5 conventions, 1 decision, 3 observations"
- [ ] New MCP tool handler in `crates/seshat-mcp/src/tools/validate_approach.rs`
- [ ] Request struct: `description: String` (required), `file_context: Option<String>`, `approach_type: Option<String>`, `scope: Option<String>`, `file_path: Option<String>`, `repo: Option<String>`
- [ ] Tool registered in `server.rs` with `execute_tool` pipeline
- [ ] Call logger summary: `{verdict, rule_count, duplicate_count, convention_count, ready}`
- [ ] `metadata.next_steps`: contextual — if rules violated: "Fix rule violations before proceeding"; if duplicates: "Consider reusing existing implementation at {file}:{line}"; if approved: "Proceed with implementation"
- [ ] Empty description → `ErrorEnvelope` with `EMPTY_TOPIC` error code
- [ ] Unit tests: approach matching a rule → verdict rules_violated, ready=false; approach with duplicates → duplicates section populated; clean approach → approved, ready=true; evidence gating with stale conventions; what_would_help populated correctly
- [ ] Integration test: register tool, call via MCP, verify full response structure
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes

### US-004: Optional Vector Search Provider

**Description:** As a developer, I want to optionally enable vector search for semantic code pattern matching so that `query_code_pattern` can find implementations by description, not just keywords.

**Acceptance Criteria:**
- [ ] New crate `crates/seshat-embedding/` with `Cargo.toml`, `lib.rs`
- [ ] `EmbeddingProvider` trait: `async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError>` and `fn dimension(&self) -> usize`
- [ ] `OllamaProvider` implementation: POST to `http://localhost:11434/api/embeddings` with configurable model name (default: `all-minilm`)
- [ ] `OpenAIProvider` implementation: POST to `https://api.openai.com/v1/embeddings` with API key from env var `OPENAI_API_KEY`, configurable model (default: `text-embedding-3-small`)
- [ ] Provider selection via `[embedding]` config section in `seshat.toml`:
  ```toml
  [embedding]
  provider = "ollama"  # or "openai"
  model = "all-minilm"
  # dimension = 384  # auto-detected from provider
  ```
- [ ] When `[embedding]` not configured: vector search disabled, FTS5-only. Zero overhead.
- [ ] New migration `V6__code_embeddings.sql`: `code_embeddings` table:
  ```sql
  CREATE TABLE code_embeddings (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      branch_id TEXT NOT NULL,
      file_path TEXT NOT NULL,
      item_name TEXT NOT NULL,
      item_kind TEXT NOT NULL,  -- 'function', 'type', 'export'
      embedding BLOB NOT NULL,  -- f32 vector as raw bytes
      UNIQUE(branch_id, file_path, item_name, item_kind)
  );
  ```
- [ ] Embedding generation during `seshat scan`: for each function/type/export, embed `"{kind} {name} in {file_path}"`. Batch calls to provider (configurable `batch_size`, default 32).
- [ ] Vector similarity search: cosine similarity computed in Rust (no SQLite extension needed). Load embeddings for branch, compute cosine with query embedding, return top-K.
- [ ] `query_code_pattern` enhanced: when embedding provider configured, `search_type` becomes `"semantic"`. Search both FTS5 + vector, merge results by score.
- [ ] `metadata.search_type`: `"keyword"` (FTS5 only) or `"semantic"` (FTS5 + vector)
- [ ] Provider errors (timeout, API error): degrade gracefully to FTS5-only search, log warning. Never crash.
- [ ] Config validation: unknown provider name → clear error at startup
- [ ] `seshat.example.toml` updated with commented `[embedding]` section
- [ ] Unit tests: mock provider returns expected embeddings; cosine similarity calculation; vector search ranking; graceful degradation on provider error; config parsing
- [ ] Integration test: `query_code_pattern` with mock embedding provider returns semantic results
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes

### US-005: Tool Registration, Call Logger & Cross-Tool Integration

**Description:** As a developer, I need all three new tools properly registered, logged, and cross-referenced so that the complete Epic 7 toolset works end-to-end.

**Acceptance Criteria:**
- [ ] All 3 new tools registered in `server.rs` via `#[tool(description = "...")]` with comprehensive descriptions for AI agent discovery
- [ ] Tool descriptions include: purpose, key parameters, example use cases, what to call next
- [ ] `impl_tool_request!` macro invoked for all 3 new request types
- [ ] Call logger produces correct summary for each new tool (verify via integration test with `--call-log`)
- [ ] `validate_approach` internally reuses `query_code_pattern` graph function for duplicate detection (not duplicated logic)
- [ ] `validate_approach` can optionally use `query_dependencies` to enrich `used_by` counts for duplicates (if target file is provided via `file_context`)
- [ ] Existing `metadata.next_steps` for current 5 tools updated to reference new tools where relevant:
  - `query_project_context` → "Call validate_approach before implementing changes"
  - `query_convention` → "Call validate_approach to check your approach against these conventions"
  - `record_decision` → "Recorded decisions are checked by validate_approach"
- [ ] `ErrorCode` enum: verify existing codes cover all new tool error cases. Add new codes only if needed (e.g. `FileNotFound` for `query_dependencies` target path).
- [ ] All existing tests still pass (no regressions)
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes

## Functional Requirements

- FR-1: `query_code_pattern` searches deserialized IR (functions, types, exports) by name matching and returns ranked results with file paths, line numbers, and code snippets
- FR-2: `query_code_pattern` also searches convention FTS5 index and returns related conventions alongside code patterns
- FR-3: `query_code_pattern` supports optional `kind` filter to narrow results to functions, types, or exports
- FR-4: `validate_approach` returns graduated response in fixed severity order: rules → contradictions → duplicates → conventions → decisions → observations
- FR-5: `validate_approach` includes `ready` boolean and `what_would_help` array for evidence gating (FR70)
- FR-6: `validate_approach` detects duplicates by reusing `query_code_pattern` IR search internally
- FR-7: `validate_approach` verdict is deterministic: `rules_violated` > `warnings_found` > `info_only` > `approved`
- FR-8: `query_dependencies` shows direct dependents and dependencies for a target file with best-effort module resolution
- FR-9: `query_dependencies` classifies blast radius as low (<3), medium (3-10), or high (>10) based on direct dependent count
- FR-10: `query_dependencies` marks unresolved imports with `resolved: false`
- FR-11: When `[embedding]` is configured, `query_code_pattern` uses vector similarity search alongside FTS5 for semantic matching
- FR-12: When `[embedding]` is not configured, all tools work with FTS5-only (zero-config, no degradation)
- FR-13: Vector search provider errors degrade gracefully to FTS5-only without crashing
- FR-14: All new tools follow `execute_tool` pipeline: scope routing, call logging, response envelope, error handling

## Non-Goals

- No transitive dependency analysis (V1 is direct dependencies only — transitive deferred)
- No call graph extraction (deferred to M2+, per ADR-26)
- No pre-computed dependency graph stored in SQLite (computed on-the-fly from IR)
- No `code_fts` table for IR search (in-memory search is fast enough for V1)
- No UI for vector search configuration (config file only)
- No embedding provider beyond Ollama and OpenAI (trait allows future providers)
- No cross-scope dependency analysis (dependencies within one scope only)
- No automatic re-embedding on file changes (re-embed on next `seshat scan`)

## Technical Considerations

- **IR deserialization performance**: `FileIRRepository::get_by_branch()` already returns `Vec<(String, ProjectFile)>`. Bincode deserialization is fast. For 3k files: ~50ms. Well within MCP P95 target of 1s.
- **Module resolution is best-effort**: Relative imports resolved against file directory. Absolute imports matched against known file paths by suffix. TypeScript path aliases and barrel exports are NOT resolved in V1. Unresolved imports flagged in response.
- **Vector storage**: Embeddings stored as raw `f32` bytes in BLOB column. Cosine similarity computed in Rust via `f32` dot product — no SQLite extension needed. For 10k items × 384 dimensions: ~15MB in DB, ~200ms for full cosine scan.
- **Embedding batch size**: Default 32. Ollama processes serially anyway. OpenAI supports batching natively.
- **New crate**: `seshat-embedding` added to workspace. Dependencies: `reqwest` (async HTTP for API calls), `serde_json`, `tokio`. Only compiled when embedding feature is used.
- **Thread safety**: Graph query functions take `&Arc<Mutex<Connection>>` — same pattern as existing tools. No new concurrency concerns.
- **`validate_approach` composability**: Internally calls `query_code_pattern` graph function (not MCP handler) for duplicate detection. This avoids double serialization and keeps it fast.

## Success Metrics

- All 3 new MCP tools produce correct responses and are discoverable via `list_tools`
- `validate_approach` correctly identifies rule violations and returns `ready: false` with actionable `what_would_help`
- `query_code_pattern` finds existing functions/types by name within MCP P95 target (<1s)
- `query_dependencies` correctly identifies direct dependents and classifies blast radius
- When embedding provider is configured, `query_code_pattern` returns semantic matches
- When embedding provider is NOT configured, all tools work identically to keyword-only mode
- Existing test suite passes with zero regressions
- Call logger produces correct summaries for all new tools

## Open Questions

- None for V1 scope. Technical decisions resolved in Party Mode discussion (2026-04-04):
  - Pattern search: in-memory IR + existing convention FTS5 (no new FTS5 table)
  - Dependencies: direct only, best-effort module resolution
  - Vector search: optional provider, FTS5 as zero-config default
