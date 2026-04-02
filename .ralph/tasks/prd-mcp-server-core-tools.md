# PRD: MCP Server, Serve Command & Core Tools (Epic 5)

## Implementation Status (2026-04-02)

| Story | Status | Notes |
|-------|--------|-------|
| US-001 | ✅ Done | rmcp + tokio deps, ServerConfig extended |
| US-002 | ✅ Done | McpServer struct, rmcp stdio, 3 tests |
| US-003 | ✅ Done | seshat serve, DB discovery, startup/shutdown — **DB discovery is broken (most-recently-modified), fix in US-013** |
| US-004 | ✅ Done | ResponseEnvelope, ErrorEnvelope, 12 tests |
| US-005 | ✅ Done | Convention persistence to nodes table |
| US-006 | ✅ Done | FTS5 migration + index management, 8 tests |
| US-007 | ✅ Done | Golden files computation, 7 tests |
| US-008 | ✅ Done | query_project_context tool, 15+ tests |
| US-009 | ✅ Done | query_convention + FTS5 search, 17 tests |
| US-010 | ✅ Done | record_decision tool, 16 tests |
| US-011 | ✅ Done | update/remove_decision tools, 34 tests |
| US-012 | ✅ Done | Agent protocol documentation in tool descriptions |
| **US-013** | **Pending** | **Smart DB discovery + forward-compatible repo/scope params** (see tech-spec-serve-db-discovery.md) |

**Known limitation:** SSE/HTTP transports declared in `ServerConfig.transports` but not wired. Only stdio transport is operational. SSE/HTTP deferred to Epic 6 daemon mode.

---

## Introduction

**Type:** Feature

Seshat becomes useful to AI agents by exposing a Model Context Protocol (MCP) server. A developer runs `seshat serve` and AI agents (Claude Desktop, Cursor, OpenCode, etc.) connect to query project context, conventions, and record decisions — the core value proposition.

This epic builds the MCP server on `rmcp` (Rust MCP SDK) with stdio + SSE + HTTP transports, implements two query tools (`query_project_context`, `query_convention`), three write tools (`record_decision`, `update_decision`, `remove_decision`), FTS5 full-text search, golden files identification, and agent protocol documentation.

**Depends on:** Epics 1-4 (all completed). The scan pipeline, knowledge graph, convention detectors, and CLI are fully operational.

**Crate status:**
- `seshat-mcp` — minimal scaffold: `McpError` enum, doc comments. No server, no tools, no envelope.
- `seshat-graph` — has a fully implemented `cross_reference` module (973 lines, Jaccard matching, contradiction/reinforcement detection). No query implementations yet.

**Architecture principle:** `seshat-graph` = intelligence (all query logic, data access via `seshat-storage`). `seshat-mcp` = thin plumbing (parse input → call graph → format JSON envelope). MCP crate has NO direct storage access.

**Critical prerequisite:** Convention detector results (`AggregatedConvention`) are currently computed in-memory during scan and passed directly to the CLI report — they are **never persisted** to the `nodes` table. This epic must persist conventions before FTS5, `query_convention`, or golden files can work. See US-003.

## Goals

- AI agent can connect to Seshat via MCP and query project context and conventions
- AI agent can record conventions/decisions that automated detectors cannot discover
- All MCP responses follow a unified JSON envelope with context-aware `next_steps` hints
- Convention findings are persisted to the `nodes` table, enabling query and FTS5 search
- Golden files (top convention-compliant files) are surfaced as exemplars
- `seshat serve` provides clear startup/shutdown UX with repo loading feedback
- Developer can configure MCP server via `seshat.toml` `[server]` section

## Story Dependency Graph

```
US-001 (serve + rmcp)
  └─► US-002 (envelope + errors)
        ├─► US-003 (convention persistence + FTS5)  ◄── CRITICAL PATH
        │     ├─► US-004 (query_project_context + golden files)
        │     └─► US-005 (query_convention)
        ├─► US-006 (record_decision)
        │     └─► US-007 (update/remove_decision)
        └─► US-008 (agent protocol docs)  — can start in parallel
```

US-003 (convention persistence) is the critical path — US-004, US-005, US-006, and US-007 all depend on conventions being in the `nodes` table.

## User Stories

### US-001: MCP Server & `seshat serve` Command

**Description:** As a developer, I want to run `seshat serve` to start the MCP server, so that AI agents can connect and query my project.

**Acceptance Criteria:**

- [ ] `rmcp` added as workspace dependency (pin to specific version after spike — see Technical Considerations)
- [ ] `tokio` added as workspace dependency (runtime for async MCP server)
- [ ] `seshat serve` starts MCP server via `rmcp` with stdio, SSE, and HTTP transports
- [ ] Startup displays: version, loaded repo(s) from XDG data dir, transport info
- [ ] Startup shows: `Watcher: not available` (watcher deferred to Epic 9)
- [ ] If no scanned projects (no `*.db` files in data dir): error with suggestion to run `seshat scan` (FR39)
- [ ] Single-repo mode: if multiple DBs exist, use the most recently modified. Log which DB is loaded.
- [ ] Ctrl+C triggers graceful shutdown per ADR-21: drain active requests (5s timeout), close DB connections, display uptime
- [ ] `ServerConfig` extended with `host: String` (default `"127.0.0.1"`), `port: u16` (default `6174`), `transports: Vec<String>` (default `["stdio", "sse", "http"]`)
- [ ] `seshat-mcp/src/lib.rs` implements `McpServer` struct with `async fn start()` method
- [ ] Server registered in `seshat-cli` dispatch (`Command::Serve`) — replace current stub
- [ ] Tracing structured logging for all server events (NFR18-20)
- [ ] `seshat.example.toml` in repo root updated with all new `[server]` options (`host`, `port`, `transports`) and `[scan]` options (`include_submodules`), with default values and comments. File uses `.toml` extension for editor syntax highlighting. Header states: "All values shown below are the defaults."

**Technical notes:**
- `rmcp` handles MCP protocol, tool registration, and transport multiplexing
- Server discovers existing DBs from `dirs::data_dir()/seshat/repos/*.db`
- Current `ServerConfig` at `crates/seshat-core/src/config.rs` has only `log_level: String` — `host`, `port`, `transports` fields must be added
- Config loading: `AppConfig` lives in `seshat-cli`. Either extract config loading to `seshat-core` or pass `ServerConfig` from CLI to MCP server. No circular dependency.
- **rmcp spike:** Before committing to the tool/envelope design, do a minimal PoC: register one dummy tool, start stdio transport, verify Claude Desktop / OpenCode can connect. This validates rmcp API compatibility.

**Startup output (per UX-DR34-36, adapted for pre-watcher state):**
```
$ seshat serve

  seshat v0.1.0

  Loading repos:
    ✓ my-project (main) — 23 conventions, 2,847 files

  Watcher: not available
  MCP server: listening
    stdio:  enabled
    http:   http://127.0.0.1:6174

  Ready. Press Ctrl+C to stop.
```

**Shutdown output (per UX-DR37-39):**
```
  ^C
  info: Shutting down...
  info: MCP server stopped.
  info: Seshat stopped. Uptime: 2h 14m.
```

### US-002: Response Envelope & Error Handling

**Description:** As an AI agent developer, I want consistent JSON response envelope for all tools, so that I can parse any tool response with one schema.

**Acceptance Criteria:**

- [ ] Success envelope: `{status, tool, repo, branch, scope, duration_ms, data, metadata}` (per ADR-9, UX-DR62-63)
- [ ] Error envelope: `{status: "error", tool, repo, error: {code, message, suggestion}}` (per UX-DR84-86)
- [ ] `metadata` includes `next_steps: Vec<String>` — context-aware hints for next tool call (FR69)
- [ ] `scope` always `"root"` in this epic (multi-repo scoping deferred to Epic 6, but field included for forward compatibility)
- [ ] Input validation runs before graph logic; invalid input returns structured error (ADR-20)
- [ ] Every tool call logged via `tracing` with tool name, duration, result status (NFR17)
- [ ] `crates/seshat-mcp/src/envelope.rs` implements `ResponseEnvelope<T>` and `ErrorEnvelope` structs with `Serialize`
- [ ] Duration measured via `Instant::now()` → `elapsed()` for each tool call
- [ ] Error codes: `REPO_NOT_SCANNED`, `EMPTY_TOPIC`, `INVALID_INPUT`, `NODE_NOT_FOUND`, `NOT_USER_DECISION`, `INTERNAL_ERROR`
- [ ] Code snippets in responses truncated at 20 lines with `"truncated": true` flag (ADR-10)

**Technical notes:**
- `next_steps` are generated per-tool: e.g., after `query_project_context` → suggest `query_convention` for specific detected topics
- All envelope types must be `Serialize + Send + Sync` for rmcp compatibility

### US-003: Convention Persistence & FTS5 Index

**Description:** As a developer, I want convention detector results persisted to the database and indexed for full-text search, so that the MCP server can query them.

**This is the critical prerequisite story.** Currently `AggregatedConvention` results from `aggregate_findings()` are computed in-memory during `seshat scan` and passed to the report renderer — never written to the database. This story persists them and creates the FTS5 index.

**Acceptance Criteria:**

- [ ] After `aggregate_findings()` in `scan.rs`, conventions are persisted to the `nodes` table as `KnowledgeNode` records
- [ ] Convention nodes use: `nature` = finding's nature, `weight` = finding's weight, `confidence` = finding's confidence, `adoption_count`/`total_count` from aggregation, `description` = convention description
- [ ] `ext_data` JSON stores: `detector_name`, `trend`, `evidence[]` (with file, line, end_line, snippet), `source: "auto_detected"`
- [ ] Per-file convention compliance data persisted: for each file, store which conventions it follows in `ext_data` (needed for golden files in US-004). Options: (a) add `compliance_score: u32` column to `files_ir`, or (b) store per-file compliance in convention node's `ext_data.file_compliance[]`
- [ ] Re-scan: old auto-detected convention nodes are replaced (DELETE where `source = "auto_detected"` + re-insert). User-recorded decisions (`source = "user"`) are NEVER deleted or overwritten.
- [ ] Migration `V4__add_fts5.sql` creates FTS5 virtual table: `CREATE VIRTUAL TABLE conventions_fts USING fts5(description, content='', content_rowid='')`
- [ ] FTS5 index populated after convention persistence via explicit INSERT (not external content table — avoids sync footgun)
- [ ] FTS5 index refreshed: (a) after every scan, (b) after `record_decision` / `update_decision`
- [ ] `seshat-graph/src/fts.rs` implements `rebuild_fts_index()` and `search_conventions(query: &str) -> Vec<NodeId>`
- [ ] `crates/seshat-storage` gets new repository method: `find_conventions_by_branch(branch_id) -> Vec<KnowledgeNode>`
- [ ] Unit tests: persist conventions, search via FTS5, verify re-scan replaces auto-detected but preserves user decisions

**Technical notes:**
- Use a standalone FTS5 table (not `content=nodes` external content) to avoid sync complexity. The table has its own `rowid` and stores `description` + `node_id` for joining back. This is simpler and more robust than external content tables.
- Convention persistence happens in `scan.rs` after `aggregate_findings()` and before `build_report_data()`. Add a new orchestrator-like step or a function in `seshat-graph`.
- Per-file compliance: the simplest approach is to add `convention_compliance_count INTEGER DEFAULT 0` to `files_ir` table (migration V4 or V5). During convention persistence, count per-file `follows_convention = true` findings and update `files_ir`.

### US-004: `query_project_context` Tool

**Description:** As an AI agent, I want to query project context so that I understand the project's stack and structure before generating code.

**Acceptance Criteria:**

- [ ] MCP tool `query_project_context` registered with `rmcp`
- [ ] Response `data` contains: `languages[]`, `modules[]`, `dependencies` (with canonical per domain), `conventions_count`, `confidence_summary` (per UX-DR64-66)
- [ ] `data.golden_files[]`: top 5 files ranked by `convention_compliance_count` from `files_ir`, with `{path, conventions_count, last_modified}` (FR64)
- [ ] `confidence_summary`: `{high_count, medium_count, low_count, high_ratio}` — ratio of high-confidence (>85%) to total. Replaces the misleading "precision" term.
- [ ] Optional `focus_area` parameter filters results to a specific domain (e.g., "error_handling", "testing")
- [ ] Response time <1 second (NFR4)
- [ ] `crates/seshat-graph/src/project_context.rs` implements all data aggregation logic
- [ ] `crates/seshat-mcp/src/tools/project_context.rs` is thin: parse input → call graph → wrap in envelope
- [ ] `metadata.next_steps` suggests querying specific conventions based on detected patterns
- [ ] `submodules[]` field present but empty array in this epic (submodule support deferred to Epic 6)

**Technical notes:**
- `golden_files`: query `files_ir` table ordered by `convention_compliance_count DESC LIMIT 5`. Join with `last_commit_date` for `last_modified`.
- Languages: query `files_ir` grouped by `language`, count files per language.
- Dependencies: load manifest analyses from a persisted source. Currently `ScanResult.manifest_analyses` is not persisted — either persist during scan (new), or re-run `discover_manifests()` on serve startup (simpler for now).
- Modules: query `nodes` table for module-type nodes.

### US-005: `query_convention` Tool

**Description:** As an AI agent, I want to query conventions for a topic so that I know how things are done before generating code.

**Acceptance Criteria:**

- [ ] MCP tool `query_convention` registered with `rmcp`
- [ ] `topic` parameter (required) searched via FTS5 against convention descriptions (FR49)
- [ ] Response `data.conventions[]` contains: `id`, `nature`, `weight`, `confidence`, `adoption` (count, total, rate), `trend`, `description`, `source` (auto_detected/user), `user_confirmed`, `examples[]` with `{file, line, end_line, snippet, truncated}` (per UX-DR67-69)
- [ ] Returns both auto-detected conventions AND user-recorded decisions matching the topic
- [ ] Removed decisions (`ext_data.removed = true`) are filtered out
- [ ] Empty result = success with empty `conventions` array (not an error)
- [ ] Empty `topic` parameter = error `EMPTY_TOPIC` with suggestion
- [ ] `crates/seshat-graph/src/conventions.rs` implements `query_convention()` with FTS5 search + data enrichment
- [ ] `metadata` includes `query`, `results_count`, `search_type: "fts5"`

**Technical notes:**
- FTS5 query: `SELECT node_id, rank FROM conventions_fts WHERE conventions_fts MATCH ? ORDER BY rank`
- Then load full `KnowledgeNode` by IDs, extract `ext_data` for evidence/trend/source
- Code examples come from `ext_data.evidence[]` (persisted in US-003)
- Trend comes from `ext_data.trend` field (computed by detectors, persisted in US-003)

### US-006: `record_decision` Tool

**Description:** As an AI agent, I want to record conventions and decisions that automated detectors cannot discover, so that project-specific rules are captured and enforced.

**Acceptance Criteria:**

- [ ] MCP tool `record_decision` registered with `rmcp`
- [ ] Parameters: `description` (required), `nature` (Decision/Convention/Rule — default Decision), `weight` (Rule/Strong — default Strong), `category` (optional), `examples[]` (optional file references + snippets), `reason` (optional)
- [ ] Creates `KnowledgeNode` with `ext_data.source = "user"`, `ext_data.user_confirmed = true` (ADR-27)
- [ ] Node is immediately active — visible in `query_convention` results
- [ ] Node is NEVER overwritten or deleted by automated re-scanning (guarded by `source` check in US-003)
- [ ] FTS5 index updated with new node's description (INSERT into `conventions_fts`)
- [ ] Response confirms creation with node ID
- [ ] `metadata.next_steps` suggests: "Use `query_convention` to verify this decision appears in results"
- [ ] Input validation: `description` required and non-empty, `nature` must be valid enum value, `weight` must be valid

**Technical notes:**
- Stored in existing `nodes` table. Distinguished by `ext_data.source = "user"`.
- `category` maps to `ext_data.detector_name`-like grouping for filtering (e.g., "error_handling", "architecture").
- `crates/seshat-graph/src/decisions.rs` implements record/update/remove logic.

### US-007: `update_decision` and `remove_decision` Tools

**Description:** As an AI agent, I want to update or remove previously recorded decisions, so that the knowledge graph stays current with team agreements.

**Acceptance Criteria:**

- [ ] MCP tools `update_decision` and `remove_decision` registered with `rmcp`
- [ ] `update_decision` parameters: `id` (required), plus any fields to change (`description`, `nature`, `weight`, `category`, `examples`, `reason`)
- [ ] `remove_decision` parameters: `id` (required), `reason` (required — why it's being removed)
- [ ] Only user-recorded decisions (`ext_data.source = "user"`) can be modified; attempts to modify auto-detected conventions return error `NOT_USER_DECISION` with suggestion
- [ ] `update_decision`: updates node fields + `ext_data`, re-indexes FTS5
- [ ] `remove_decision`: soft-deletes (sets `ext_data.removed = true`, `ext_data.removed_reason = "..."`, `ext_data.removed_at = timestamp`) — preserved in history
- [ ] Removed decisions no longer appear in `query_convention` results (filtered in graph query)
- [ ] Response confirms update/removal with node ID

**Technical notes:**
- Soft-delete preserves audit trail. All graph queries filter out `ext_data.removed = true`.
- FTS5 re-index: DELETE old row + INSERT new for updates. DELETE only for removals.

### US-008: Agent Protocol Documentation

**Description:** As an AI agent developer, I want clear instructions for when and how to use Seshat tools, so that agents follow the understand → work → update loop correctly.

**Acceptance Criteria:**

- [ ] Protocol documented in MCP `list_tools` descriptions for each tool (visible to AI agents during tool discovery)
- [ ] Protocol: (1) Query conventions before work → (2) Do work → (3) If new convention discovered, suggest recording it
- [ ] Each tool's `description` in rmcp registration includes: purpose, when to use, parameter guidance
- [ ] Common scenarios documented: wrapper conventions, architectural decisions, team style agreements
- [ ] Tool descriptions are concise but sufficient for AI agent self-guidance (< 500 chars each)

**Technical notes:**
- rmcp's tool registration allows setting `description` per tool. This is what AI agents see in `list_tools`.
- The key insight: tool descriptions guide AI behavior without requiring explicit agent instructions.

## Functional Requirements

- FR-1: MCP server starts via `rmcp` with stdio + SSE + HTTP transports (`seshat serve`)
- FR-2: Server discovers existing scanned projects from XDG data directory; selects most recently modified DB if multiple exist
- FR-3: If no scanned projects found, return informative error with suggestion (FR39)
- FR-4: All tool responses use unified JSON envelope: `{status, tool, repo, branch, scope, duration_ms, data, metadata}` (ADR-9)
- FR-5: All tool responses include `metadata.next_steps` context-aware hints (FR69)
- FR-6: Input validation before graph logic; structured error responses (ADR-20)
- FR-7: Convention detector results persisted to `nodes` table after each scan, with FTS5 index maintained
- FR-8: Auto-detected convention nodes replaced on re-scan; user-recorded decisions never overwritten
- FR-9: Per-file convention compliance count persisted in `files_ir` for golden files computation
- FR-10: `query_project_context` returns languages, modules, dependencies, conventions_count, confidence_summary, golden_files (FR32, FR64)
- FR-11: `query_convention` searches via FTS5 and returns matching conventions with trend, confidence, examples (FR33, FR49)
- FR-12: `record_decision` creates user-sourced knowledge nodes immediately active in queries (FR65, ADR-27)
- FR-13: `update_decision` modifies user-recorded decisions; `remove_decision` soft-deletes them (FR66)
- FR-14: Only user-recorded decisions (`source = "user"`) can be modified/removed via MCP tools
- FR-15: FTS5 virtual table (standalone, not external content) for convention description search (FR49)
- FR-16: Graceful shutdown: drain requests (5s), close DB, log uptime (ADR-21)
- FR-17: All tool calls logged via `tracing` with tool name, duration, result status (NFR17-20)
- FR-18: Code snippets in responses truncated at 20 lines with `truncated` flag (ADR-10)
- FR-19: Serve command startup displays loaded repos, transport info, watcher status ("not available") (UX-DR34-36)
- FR-20: `ServerConfig` extended with `host`, `port`, `transports` fields with defaults

## Non-Goals (Out of Scope)

- **`validate_approach` tool** — deferred to Epic 7 (requires duplicate detection, graduated response logic)
- **`query_code_pattern` tool** — deferred to Epic 7 (requires code pattern indexing)
- **`query_dependencies` tool** — deferred to Epic 7 (requires dependency graph traversal)
- **Multi-repo routing** — deferred to Epic 6. This epic supports single-repo mode (most recently modified DB).
- **Vector/embedding search** — deferred to M2+ per ADR-26. FTS5 only.
- **File watcher integration** — deferred to Epic 9. Server serves static scan data. Startup shows "Watcher: not available".
- **Branch switching** — deferred to Epic 10. Always serves `main` branch.
- **Interactive convention review (TUI)** — deferred to Epic 11.
- **SSE/HTTP authentication** — not in scope for local-only server.
- **Manifest analysis persistence** — manifest data can be re-loaded from DB or re-discovered. Full persistence deferred.

## Technical Considerations

### New Dependencies

| Dependency | Purpose | Crate |
|-----------|---------|-------|
| `rmcp` | MCP SDK (stdio + SSE + HTTP) | `seshat-mcp` |
| `tokio` | Async runtime for MCP server | workspace |
| `serde`, `serde_json` | JSON serialization (already in workspace) | `seshat-mcp` |

**rmcp spike:** Before committing to the tool/envelope design, implement a minimal proof-of-concept: register one dummy tool, start stdio transport, verify a real MCP client (Claude Desktop or OpenCode) can discover and call it. This validates rmcp API compatibility and should be the first task in US-001.

### Crate Architecture

```
seshat-mcp (thin plumbing — NO direct storage access)
  ├── lib.rs              — McpServer, start_server()
  ├── envelope.rs         — ResponseEnvelope<T>, ErrorEnvelope
  ├── scope.rs            — scope detection (always "root" for now)
  └── tools/
      ├── mod.rs           — tool registration with rmcp
      ├── project_context.rs
      ├── convention.rs
      ├── record_decision.rs
      └── manage_decision.rs

seshat-graph (intelligence — accesses storage via seshat-storage)
  ├── lib.rs              — existing: GraphError, cross_reference module
  ├── project_context.rs  — query_project_context logic
  ├── conventions.rs      — convention persistence + query_convention
  ├── decisions.rs        — record/update/remove decision logic
  ├── golden_files.rs     — golden file computation from files_ir
  └── fts.rs              — FTS5 index management (rebuild, search)
```

### Database Migrations

- `V4__add_conventions_fts.sql`: Create standalone FTS5 virtual table + add `convention_compliance_count` to `files_ir`

```sql
-- Standalone FTS5 table (not external content — avoids sync complexity)
CREATE VIRTUAL TABLE IF NOT EXISTS conventions_fts USING fts5(
    description,
    node_id,
    detector_name
);

-- Per-file convention compliance for golden files
ALTER TABLE files_ir ADD COLUMN convention_compliance_count INTEGER NOT NULL DEFAULT 0;
```

### FTS5 Sync Strategy

**Explicit refresh, not triggers.** FTS5 is rebuilt in two scenarios:
1. **After scan:** `seshat-graph::fts::rebuild_fts_index()` — DELETE all rows, re-INSERT from convention nodes in `nodes` table. Called from `scan.rs` after convention persistence.
2. **After record/update/remove_decision:** Incremental update — INSERT/UPDATE/DELETE single row in `conventions_fts`.

This avoids the fragility of external content tables or database triggers.

### Concurrency

- `rmcp` handles multiple concurrent requests (especially via SSE/HTTP)
- Current `Database` uses `Arc<Mutex<Connection>>` for writes (single writer). This is adequate for MCP workload (mostly reads, rare writes from `record_decision`).
- SQLite WAL mode (already enabled) allows concurrent readers. Read queries should use a separate read-only connection pool or `PRAGMA query_only` to avoid writer lock contention.
- All `seshat-graph` query methods must be `Send + Sync` for tokio/rmcp compatibility.
- `record_decision` acquires write lock briefly — acceptable latency for rare write operations.

### Config Loading

- `AppConfig` currently lives in `crates/seshat-cli/src/config.rs`
- `seshat serve` needs `ServerConfig` (from `seshat-core`) + DB path resolution
- Approach: `seshat-cli` loads `AppConfig`, extracts `ServerConfig`, passes it to `seshat-mcp::McpServer::start(config, db)`. No circular dependency.
- DB path resolution reuses existing `resolve_db_path()` from `scan.rs` (may need extraction to shared utility).

### Performance Requirements (NFR)

- `query_project_context` P95 < 1 second (NFR4)
- `query_convention` P95 < 1 second (NFR5)
- MCP server memory < 100MB baseline (NFR10)

## Test Strategy

### Unit Tests
- `seshat-graph`: test each query function with in-memory SQLite DB populated with known data
- `seshat-mcp/envelope.rs`: test serialization of success/error envelopes
- `crates/seshat-graph/src/fts.rs`: test FTS5 index rebuild + search with known conventions

### Integration Tests
- **End-to-end:** scan fixture project → persist conventions → start server (in-process, not via CLI) → call each tool → verify response envelope structure and content
- **Convention persistence roundtrip:** scan → persist → re-scan → verify user decisions survive
- **FTS5 search quality:** insert conventions with known descriptions → search by keywords → verify relevant results ranked first

### MCP Client Testing
- Direct function calls to `seshat-graph` layer (bypassing MCP protocol) for fast, deterministic tests
- Optional: use `rmcp` test utilities (if available) to test MCP protocol compliance
- Manual smoke test: `seshat serve` + Claude Desktop connection

## Success Metrics

- AI agent (Claude Desktop / Cursor / OpenCode) can connect to `seshat serve` via stdio and receive project context
- `query_project_context` returns accurate language breakdown, dependency info, and golden files
- `query_convention` returns relevant conventions for topic queries with <1s response time
- `record_decision` persists decisions that survive re-scans
- All 5 tools return consistent JSON envelope parseable by any MCP client
- Integration test: scan fixture project → serve → query all tools → verify responses

## Open Questions

1. **rmcp version:** Which version of `rmcp` to pin? Requires spike to verify API compatibility with our tool/envelope design.
2. **SSE/HTTP port:** Default port `6174` (Kaprekar's constant). Configurable via `ServerConfig.port`.
3. **FTS5 rebuild performance:** Full rebuild (DELETE + re-INSERT) acceptable for projects with <1000 conventions? Profile on large projects.
4. **Golden files count:** Top 5 is hardcoded. Should it be configurable via `query_project_context` parameter `golden_files_limit`?
5. **Manifest data for dependencies:** Currently not persisted to DB. Re-run `discover_manifests()` on serve startup? Or persist in a new table? (Simplest: re-discover from disk since manifest files are small and fast to parse.)
