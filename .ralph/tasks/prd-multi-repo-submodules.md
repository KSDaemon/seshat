# PRD: Multi-Repository & Submodule Support (Epic 6)

## Introduction

**Type:** Feature

Seshat gains the ability to manage multiple repositories and automatically scan git submodules as separate knowledge graphs. AI agents get correct conventions regardless of whether they're working in the root project or a submodule — scope is determined automatically from the file path or set explicitly.

**Depends on:** Epic 5 (all completed). MCP server operational with 5 tools, `repo`/`scope` parameters already present in all tool schemas (currently ignored).

**What this epic does NOT include:** Daemon mode (`--daemon`), HTTP/SSE transport, multi-project serving from one process. These are deferred to a future epic. The server continues to operate in single-project stdio mode.

## Goals

- Submodules are scanned automatically into separate .db files (one per submodule)
- Submodule databases stored in directory structure mirroring mount paths
- AI agent queries are scoped correctly: root vs submodule, auto-detected or explicit
- `seshat status` shows all projects and submodules with useful metadata
- Changed submodule commit triggers automatic rescan of that submodule
- `repo` and `scope` parameters in MCP tools become functional (not ignored)

## User Stories

### US-001: Invert Submodule Default — Scan Submodules by Default

**Description:** As a developer, I want submodules scanned automatically when I run `seshat scan`, so that I don't have to remember a special flag for the common case.

**Acceptance Criteria:**

- [ ] `seshat scan <path>` with submodules present: scans root AND each submodule into separate .db files
- [ ] `seshat scan <path> --exclude-submodules` skips submodule scanning (only root)
- [ ] `ScanConfig.include_submodules` renamed/replaced: default is now `true` (scan submodules)
- [ ] `--include-submodules` flag removed or aliased to no-op with deprecation warning
- [ ] `--exclude-submodules` flag added to `Command::Scan`
- [ ] `seshat.example.toml` updated: `exclude_submodules = false` (new default)
- [ ] Scan output shows submodule scanning progress:
  ```
  ✓ Discovering files... 720 found
  ✓ Detected 1 submodule: external/walt-portal
  ✓ Scanning root project... 720/720
  ✓ Scanning submodule external/walt-portal... 1204/1204
  ```
- [ ] Uninitialized submodules (empty directory) skipped with warning

### US-002: Submodule Database Structure

**Description:** As a developer, I want each submodule stored in a separate database file organized by mount path, so that submodule conventions don't mix with the root project.

**Acceptance Criteria:**

- [ ] Root project DB: `$XDG_DATA/seshat/repos/{project_name}.db`
- [ ] Submodule DB: `$XDG_DATA/seshat/repos/{project_name}/{mount_path}.db`
  Example: `repos/walt-chat-backend/external/walt-portal.db`
- [ ] Parent directories created automatically (e.g., `repos/walt-chat-backend/external/`)
- [ ] Each submodule scanned independently: own files_ir, nodes, conventions, FTS5
- [ ] Root DB stores submodule metadata: new `submodules` table with `relative_path`, `name`, `db_path`, `commit_hash`
- [ ] Child DB stores parent metadata: `repo_metadata` entries for `parent_project` and `mount_path`
- [ ] Migration V5 adds `submodules` table and `repo_metadata` table
- [ ] `db.rs` updated: `resolve_submodule_db_path(root: &Path, mount_path: &str) -> PathBuf`

**Technical notes:**
- Mount path comes from `.gitmodules` `path` field (already parsed by `detect_submodule_paths()` in discovery.rs)
- `commit_hash` comes from `git rev-parse HEAD` in the submodule directory, or from `.git/modules/{name}/HEAD`
- If submodule directory is empty (not initialized), skip with warning — do not create DB

### US-003: Submodule Change Detection & Auto-Rescan

**Description:** As a developer, I want Seshat to detect when a submodule's commit has changed and rescan it automatically, so that conventions stay current without manual intervention.

**Acceptance Criteria:**

- [ ] On `seshat scan`, for each submodule: compare stored `commit_hash` (from `submodules` table) with current HEAD in submodule directory
- [ ] If hash differs: rescan submodule (full scan of submodule DB)
- [ ] If hash matches: skip submodule scan (show "up to date" in progress)
- [ ] If submodule is new (not in `submodules` table): full scan, add to table
- [ ] If submodule removed from `.gitmodules`: remove from `submodules` table (leave orphaned DB — user cleans up via `seshat status`)
- [ ] Progress output for skipped submodules:
  ```
  ✓ Submodule external/walt-portal: up to date (abc1234)
  ```

### US-004: Scope Detection & Query Routing

**Description:** As an AI agent, I want my queries automatically routed to the correct knowledge graph based on file path or explicit scope, so that I get relevant conventions for the code I'm working on.

**Acceptance Criteria:**

- [ ] `scope.rs` module created in `seshat-mcp/src/` with scope resolution logic
- [ ] **Auto-scope via file_path:** Add optional `file_path: Option<String>` parameter to `query_convention` and `query_project_context` request schemas
- [ ] File path prefix-matched against submodule mount points from `submodules` table
  - `external/walt-portal/src/App.tsx` → matches mount point `external/walt-portal` → use walt-portal.db
  - `src/api/handler.py` → no match → use root DB
- [ ] **Explicit scope:** `scope: "walt-portal"` or `scope: "external/walt-portal"` — direct lookup
- [ ] **Priority:** explicit `scope` > `file_path` auto-detect > default root
- [ ] **Default:** no scope + no file_path → root project DB
- [ ] Scope field in response envelope reflects the actual scope used (not just "root")
- [ ] `McpServer` struct gains access to submodule DB connections (lazy loaded)
- [ ] `record_decision` / `update_decision` / `remove_decision` also support `scope` — decisions recorded in the correct DB

### US-005: Serve with Submodule Support

**Description:** As a developer, I want `seshat serve` to load the root project and make submodules available for scoped queries, so that AI agents can query any part of my project.

**Acceptance Criteria:**

- [ ] `seshat serve` loads root project DB (as before)
- [ ] On startup, reads `submodules` table from root DB
- [ ] Submodule DBs loaded **lazily** — opened on first scoped query, not at startup
- [ ] Startup output shows submodules:
  ```
  Repo:         walt-chat-backend
  Branch:       main
  Files:        720
  Conventions:  58
  Submodules:   1 (external/walt-portal)
  ```
- [ ] `McpServer` struct extended: single `conn` replaced with `RootProject` struct holding root conn + submodule registry
- [ ] Query with `scope: "walt-portal"` → opens `walt-chat-backend/walt-portal.db` lazily → returns submodule conventions
- [ ] Error for unknown scope: `UNKNOWN_SCOPE` error code with list of available scopes

### US-006: `seshat status` Command

**Description:** As a developer, I want `seshat status` to show all my scanned projects and submodules with useful metadata, so that I can manage my Seshat databases.

**Acceptance Criteria:**

- [ ] `seshat status` lists all `.db` files from XDG repos directory
- [ ] Tree structure shows parent/child relationships:
  ```
  seshat            main    192 files    92 conventions    4.2 MB    5m ago
  walt-chat-backend main    720 files    58 conventions    6.3 MB    2h ago
    └─ external/walt-portal  main  1204 files  18 conventions  8.1 MB  2h ago
  rust-project      main     45 files    12 conventions    1.1 MB    3d ago
  ```
- [ ] Columns: project name, branch, file count, convention count, DB size, last modified (human-readable relative time)
- [ ] Orphaned submodule DBs (parent removed) shown with warning indicator
- [ ] Full DB path shown with `--verbose`
- [ ] Replace current `seshat status` stub in args.rs/lib.rs

### US-007: `repo` Parameter Activation

**Description:** As an AI agent developer, I want the `repo` parameter in tool schemas to actually work, so that I can target specific projects when the context is ambiguous.

**Acceptance Criteria:**

- [ ] `repo` parameter in all 5 tool request schemas: when provided, overrides the auto-detected repo
- [ ] In project mode (single project): `repo` parameter validated against loaded project name; mismatch returns `REPO_NOT_FOUND` error with available repo name
- [ ] `repo` value can be project name (`"walt-chat-backend"`) or absolute path
- [ ] Response envelope `repo` field reflects the actual repo used

## Functional Requirements

- FR-1: Submodules scanned by default into separate .db files (one per submodule, one level deep)
- FR-2: Submodule DBs stored at `repos/{project}/{mount_path}.db` mirroring git mount structure
- FR-3: Parent DB stores submodule metadata (path, name, commit_hash); child DB stores parent reference
- FR-4: `--exclude-submodules` flag to skip submodule scanning
- FR-5: Changed commit_hash triggers automatic submodule rescan
- FR-6: Unchanged submodules skipped with "up to date" message
- FR-7: Auto-scope from `file_path` parameter via prefix matching against mount points
- FR-8: Explicit `scope` parameter for direct submodule targeting
- FR-9: Default scope is root project when no scope context provided
- FR-10: Scope reflected accurately in response envelope
- FR-11: `seshat serve` loads root + lazy submodule DBs
- FR-12: `seshat status` shows tree of all projects/submodules with metadata
- FR-13: `repo` parameter validated and functional in project mode
- FR-14: Uninitialized submodules skipped with warning
- FR-15: Submodule branch derived from parent repository branch; commit_hash stored separately

## Non-Goals (Out of Scope)

- **Daemon mode** (`--daemon`, HTTP/SSE transport, multi-project serving) — deferred to future epic
- **Recursive submodules** — only first-level submodules supported
- **Automatic garbage collection** of orphaned DBs — user manages via `seshat status` + manual deletion
- **Cross-scope queries** (`scope: "all"`) — deferred to Epic 7
- **Submodule-to-root convention inheritance** — each scope is independent
- **File watcher for submodules** — deferred to Epic 9

## Technical Considerations

### Database Migrations

**V5:** Add `submodules` table and `repo_metadata` table:

```sql
CREATE TABLE IF NOT EXISTS submodules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    relative_path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    db_path TEXT NOT NULL,
    commit_hash TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS repo_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

### Crate Changes

| Crate | Changes |
|-------|---------|
| `seshat-core` | Rename/update `ScanConfig` submodule field |
| `seshat-scanner` | Orchestrator: scan submodules into separate DBs |
| `seshat-storage` | New `SubmoduleRepository` + `RepoMetadataRepository` |
| `seshat-mcp` | New `scope.rs`, extend `McpServer` with submodule registry |
| `seshat-cli` | `args.rs`: `--exclude-submodules`, `status` command; `db.rs`: submodule path resolution; `serve.rs`: submodule loading |
| `seshat-graph` | Query functions accept conn parameter (already do) — no structural changes |

### Submodule Scan Flow

```
seshat scan ~/Projects/walt-chat-backend
  1. Parse .gitmodules → [(relative_path, url)]
  2. For each submodule:
     a. Check if initialized (directory non-empty + .git exists)
     b. Get current commit_hash (git rev-parse HEAD in submodule dir)
     c. Compare with stored hash in root DB submodules table
     d. If changed/new: full scan → create/update submodule DB
     e. If unchanged: skip
     f. Update submodules table with current hash
  3. Scan root project (excluding submodule directories)
```

### Scope Resolution Flow (scope.rs)

```
resolve_scope(req) -> (conn, scope_name):
  1. If explicit scope provided: lookup in submodules table → return submodule conn
  2. If file_path provided: prefix match against submodule mount points → return matching conn
  3. Default: return root conn, scope = "root"
```

### Breaking Change: --include-submodules → --exclude-submodules

Current Epic 4 behavior: submodules excluded by default, `--include-submodules` to include.
New Epic 6 behavior: submodules scanned by default, `--exclude-submodules` to skip.

Migration: `ScanConfig.include_submodules: bool` (default false) → `ScanConfig.exclude_submodules: bool` (default false). The field name change ensures that existing `seshat.toml` files with `include_submodules = false` don't accidentally change behavior.

## Success Metrics

- `seshat scan` on project with submodules creates correct DB structure
- `query_convention` with `file_path` in submodule returns submodule-specific conventions
- `query_convention` with explicit `scope` returns correct scope conventions
- `seshat status` shows tree with all projects and submodules
- Submodule rescan triggered only when commit_hash changes
- All existing tests pass (single-project mode unaffected)

## Open Questions

1. **`file_path` parameter format:** Relative to project root? Absolute? Need to define normalization.
2. **Submodule name collision:** Two submodules with same leaf name but different mount paths (e.g., `libs/shared` and `vendor/shared`). Use full mount path as scope identifier?
3. **`seshat status` performance:** For projects with many submodules, listing all DBs + reading metadata could be slow. Cache in root DB?
