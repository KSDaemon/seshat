# PRD: Multi-Repository & Submodule Support (Epic 6)

## Introduction

**Type:** Feature

Seshat gains the ability to automatically scan git submodules as separate knowledge graphs with full scope isolation. AI agents get correct conventions regardless of whether they're working in the root project or a submodule — scope is determined automatically from the file path or set explicitly. All five MCP tools support scoped queries and writes transparently.

**Depends on:** Epic 5 (all completed). MCP server operational with 5 tools, `repo`/`scope` parameters already present in all tool schemas (currently ignored).

**What this epic does NOT include:** Daemon mode (`--daemon`), HTTP/SSE transport, multi-project serving from one process. These are deferred to a future epic. The server continues to operate in single-project stdio mode.

## Goals

- Submodules are scanned automatically into separate .db files (one per submodule)
- Submodule databases stored in directory structure mirroring mount paths
- AI agent queries and writes are scoped correctly: root vs submodule
- Scope auto-detected from `file_path` parameter or set explicitly via `scope`
- All 5 MCP tools support transparent scoping (including write tools)
- `seshat status` shows all projects and submodules with useful metadata
- Changed submodule commit triggers automatic rescan of that submodule
- Submodules can be scanned in parallel for performance

## Story Dependency Graph

```
US-001 (DB structure + migrations + metadata)
  └─► US-002 (scan flow: N+1 orchestrator + parallel submodules + progress)
        └─► US-003 (change detection: commit_hash compare + auto-rescan)
              └─► US-004 (scope.rs + McpServer redesign + eager connection loading)
                    └─► US-005 (file_path + scope in all 5 tools + auto-scope routing)
                          └─► US-006 (seshat serve with submodule connections)
                                └─► US-007 (seshat status command)
US-008 (repo parameter activation) — independent, lowest priority
```

## User Stories

### US-001: Submodule Database Structure & Migrations

**Description:** As a developer, I want each submodule stored in a separate database file organized by mount path, with metadata linking parent and child, so that submodule conventions are fully isolated.

**Acceptance Criteria:**

- [ ] Root project DB: `$XDG_DATA/seshat/repos/{project_name}.db`
- [ ] Submodule DB: `$XDG_DATA/seshat/repos/{project_name}/{mount_path}.db`
  Example: `repos/walt-chat-backend/external/walt-portal.db`
- [ ] Parent directories created automatically (e.g., `repos/walt-chat-backend/external/`)
- [ ] Each submodule DB is a full independent Seshat database: own files_ir, nodes, conventions_fts, etc.
- [ ] Migration V5 adds two new tables (applied to all DBs):
  - `submodules` table in root DB: `relative_path`, `name`, `db_path`, `commit_hash`, `created_at`, `updated_at`
  - `repo_metadata` table in all DBs: key-value store for `parent_project`, `mount_path`, `project_name`, `file_count`, `convention_count`, `last_scan_time`
- [ ] `repo_metadata` populated after each scan with summary stats (file_count, convention_count, last_scan_time) — enables fast `seshat status` without opening every DB
- [ ] `db.rs` updated: `resolve_submodule_db_path(project_name: &str, mount_path: &str) -> PathBuf`
- [ ] `seshat-storage`: new `SubmoduleRepository` trait + `SqliteSubmoduleRepository`
- [ ] `seshat-storage`: new `RepoMetadataRepository` trait + `SqliteRepoMetadataRepository`
- [ ] Unit tests: create root + submodule DBs, write/read metadata, verify isolation

**Migration V5 SQL:**

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

**Technical notes:**
- Mount path comes from `.gitmodules` `path` field (already parsed by `detect_submodule_paths()` in discovery.rs — currently private, needs to become `pub(crate)` or `pub`)
- `commit_hash` comes from `git rev-parse HEAD` in the submodule directory
- If submodule directory is empty (not initialized), skip — do not create DB
- `repo_metadata` summary cache is updated at end of each scan: `file_count`, `convention_count`, `last_scan_time` as ISO 8601

### US-002: Submodule Scan Flow with Parallel Scanning

**Description:** As a developer, I want `seshat scan` to automatically detect and scan submodules into separate databases, with parallel scanning for performance.

**This is the core engineering story.** The current scan orchestrator takes a single `&Database` and writes everything to it. For submodules, the orchestrator is called N+1 times: once per submodule, then once for root.

**Acceptance Criteria:**

- [ ] `seshat scan <path>` with submodules present: scans root AND each submodule into separate .db files
- [ ] `seshat scan <path> --exclude-submodules` skips submodule scanning (only root)
- [ ] `ScanConfig` field change: `include_submodules: bool` (default false) → `exclude_submodules: bool` (default false). Field rename ensures existing `seshat.toml` with `include_submodules = false` doesn't silently change behavior.
- [ ] `--include-submodules` flag removed from `Command::Scan`, replaced with `--exclude-submodules`
- [ ] `seshat.example.toml` updated accordingly
- [ ] Scan flow (orchestrated in `scan.rs`, NOT inside the scan orchestrator):
  1. Parse `.gitmodules` → list of `(mount_path, name)`
  2. Filter uninitialized submodules (empty dir / no `.git`) → skip with warning
  3. For each initialized submodule, in **parallel** (via `rayon` or `tokio::spawn_blocking`):
     - Open/create submodule DB at `repos/{project}/{mount_path}.db`
     - Call `scan_project_with_progress(submodule_path, config, &submodule_db, progress_cb)`
     - Update `repo_metadata` in submodule DB
  4. Scan root project (excluding submodule directories) — existing behavior
  5. Update `submodules` table in root DB with current `commit_hash` for each submodule
  6. Update `repo_metadata` in root DB
- [ ] `detect_submodule_paths()` in discovery.rs made `pub` (or moved to a shared location)
- [ ] Uninitialized submodules: warning in scan output, not in submodules table

**New `ScanProgress` variants:**

```rust
/// A submodule was detected during discovery.
SubmoduleDetected { name: String, mount_path: String },
/// Submodule scanning has started.
ScanningSubmodule { name: String, done: usize, total: usize },
/// Submodule scan complete.
SubmoduleScanDone { name: String },
/// Submodule is up to date (commit_hash unchanged).
SubmoduleUpToDate { name: String, commit_hash: String },
/// Submodule skipped (not initialized).
SubmoduleSkipped { name: String, reason: String },
```

**Progress UX for scan.rs:**

```
  ✓ Discovering files... 720 found
  ✓ Detected 2 submodules: external/walt-portal, libs/shared
  ✓ Collecting git history... done
  ✓ Scanning root... 720/720
  ✓ Building module graph... done
  ✓ Analyzing manifests & docs... done
  ✓ Scanning submodule external/walt-portal... 1204/1204
  ✓ Scanning submodule libs/shared... 340/340
  ✓ Analyzing conventions... 720/720
```

If submodules scan in parallel, progress spinners update concurrently (both visible). Each submodule gets its own spinner line.

**Technical notes:**
- The existing `scan_project_with_progress()` function is reused as-is for each submodule. It already handles discovery → parse → persist → graph → manifests → docs. We just call it with a different root path and DB.
- Root project scan must exclude submodule directories. This is already implemented in `discovery.rs` via `filter_entry`. When `exclude_submodules = false` (default = scan submodules), root discovery STILL excludes submodule dirs from the root scan because each submodule has its own DB. The `exclude_submodules` flag controls whether the separate submodule scans happen, not whether submodule files go into root.
- Convention detection + FTS5 rebuild happens per-DB (already the case since each `scan_project_with_progress` call does this).

### US-003: Submodule Change Detection & Auto-Rescan

**Description:** As a developer, I want Seshat to detect when a submodule's commit has changed and rescan only the changed ones, so that rescans are fast.

**Acceptance Criteria:**

- [ ] On `seshat scan`, for each submodule: compare stored `commit_hash` (from `submodules` table in root DB) with current HEAD in submodule directory
- [ ] If hash differs: full rescan of submodule DB
- [ ] If hash matches: skip submodule scan, show "up to date" with `SubmoduleUpToDate` progress event
- [ ] If submodule is new (not in `submodules` table): full scan, add to table
- [ ] If submodule removed from `.gitmodules`: remove row from `submodules` table, leave orphaned DB on disk (user cleans up via `seshat status`)
- [ ] If submodule directory exists but is empty (not initialized): skip with `SubmoduleSkipped` progress event
- [ ] Progress output:
  ```
  ✓ Submodule external/walt-portal: up to date (abc1234)
  ✓ Submodule libs/shared: rescanning (hash changed def5678 → 9012abc)
  ```

**Technical notes:**
- Getting current commit hash: `git -C {submodule_path} rev-parse HEAD` or read `.git/modules/{name}/HEAD`
- Consider using `gix` crate for git operations instead of shelling out (consistent with Rust ecosystem, already considered in architecture doc)

### US-004: Scope Resolution Module & McpServer Redesign

**Description:** As a system architect, I want a scope resolution layer and redesigned McpServer that holds connections to all submodule databases, so that scoped queries route to the correct knowledge graph.

**Acceptance Criteria:**

- [ ] New `crates/seshat-mcp/src/scope.rs` module with scope resolution logic
- [ ] `McpServer` struct redesigned — single `conn` replaced with:
  ```rust
  pub struct McpServer {
      tool_router: ToolRouter<Self>,
      config: ServerConfig,
      /// Root project connection + metadata.
      root: ProjectConnection,
      /// Submodule connections keyed by mount path (eagerly loaded).
      submodules: HashMap<String, ProjectConnection>,
      /// Submodule mount paths sorted longest-first (for prefix matching).
      mount_paths: Vec<String>,
  }

  pub struct ProjectConnection {
      pub conn: Arc<Mutex<Connection>>,
      pub name: String,
      pub branch: String,
  }
  ```
- [ ] **Eager loading:** All submodule DB connections opened at startup (not lazy). Passed as `HashMap<mount_path, Arc<Mutex<Connection>>>` to McpServer.
- [ ] Scope resolution function:
  ```rust
  /// Resolve which DB connection to use based on request context.
  /// Priority: explicit scope > file_path auto-detect > default root.
  fn resolve_scope(
      &self,
      scope: Option<&str>,
      file_path: Option<&str>,
  ) -> Result<(&ProjectConnection, String), ErrorCode>
  ```
- [ ] **File path prefix matching:** `file_path` matched against `mount_paths` (longest prefix wins). Example: `external/walt-portal/src/App.tsx` matches mount point `external/walt-portal`.
- [ ] **Explicit scope:** `scope: "external/walt-portal"` (full mount path) → direct lookup in `submodules` HashMap. Short name `"walt-portal"` also tried if full path not found; if ambiguous → `UNKNOWN_SCOPE` error listing options.
- [ ] **Default:** no scope + no file_path → root connection, scope = `"root"`
- [ ] New error codes added to `ErrorCode` enum:
  - `UnknownScope` — scope value doesn't match any known mount path
  - `RepoNotFound` — repo parameter doesn't match loaded project
- [ ] `start_stdio_with_shutdown()` signature updated: takes root conn + submodule connections instead of single conn
- [ ] Response envelope `scope` field reflects actual scope used
- [ ] All tool handlers updated: call `self.resolve_scope()` instead of using `self.conn` directly
- [ ] Unit tests: scope resolution with various inputs (explicit, file_path, default, ambiguous, unknown)

**Technical notes:**
- Mount paths sorted longest-first ensures `libs/shared/utils` matches `libs/shared` before `libs`.
- For projects with no submodules: `submodules` HashMap is empty, all queries go to root. Zero overhead.
- `ProjectConnection` wraps conn + name + branch for each scope. The branch for submodules is the parent branch (submodules are pinned by commit hash, not branch).

### US-005: `file_path` Parameter in All 5 MCP Tools

**Description:** As an AI agent, I want to pass the current file path to any MCP tool and have it automatically route to the correct scope, so that I get relevant results without manually determining scope.

**Acceptance Criteria:**

- [ ] `file_path: Option<String>` added to ALL 5 tool request schemas:
  - `ProjectContextRequest`
  - `QueryConventionRequest`
  - `RecordDecisionRequest`
  - `UpdateDecisionRequest`
  - `RemoveDecisionRequest`
- [ ] `#[schemars(description)]` for file_path: "File path relative to project root. Used for automatic scope detection — if the file belongs to a submodule, the query/write targets that submodule's knowledge graph."
- [ ] All 5 tool handlers use `self.resolve_scope(req.scope.as_deref(), req.file_path.as_deref())` to determine the correct DB connection
- [ ] Write tools (`record_decision`, `update_decision`, `remove_decision`): decision written to the scoped DB, not root. If agent records a decision while editing `external/walt-portal/src/api.ts`, it goes to walt-portal.db.
- [ ] Tool descriptions updated to mention file_path and scope:
  - `query_project_context`: "Pass file_path for automatic scope detection when working in submodules"
  - `query_convention`: "Pass file_path or scope to query submodule-specific conventions"
  - `record_decision`: "Pass file_path or scope to record decisions in the correct submodule"
- [ ] `file_path` normalization: stripped of leading `/` or `./`, treated as relative to project root

### US-006: `seshat serve` with Submodule Connections

**Description:** As a developer, I want `seshat serve` to load the root project and all submodule connections at startup, so that AI agents can query any part of my project immediately.

**Acceptance Criteria:**

- [ ] `seshat serve` loads root project DB (as before)
- [ ] On startup, reads `submodules` table from root DB
- [ ] Opens ALL submodule DB connections eagerly
- [ ] Passes root + submodule connections to `McpServer::new()`
- [ ] Startup output shows submodules:
  ```
  Repo:         walt-chat-backend
  Branch:       main
  Files:        720
  Conventions:  58
  Submodules:
    └─ external/walt-portal (1204 files, 18 conventions)
  ```
- [ ] If submodule DB doesn't exist on disk (orphaned reference in table): warning, skip that submodule
- [ ] `serve.rs` refactored: loads submodule info from `submodules` table, resolves DB paths, opens connections, builds `HashMap<String, ProjectConnection>`

### US-007: `seshat status` Command

**Description:** As a developer, I want `seshat status` to show all my scanned projects and submodules with useful metadata, so that I can manage my Seshat databases and identify orphans.

**Acceptance Criteria:**

- [ ] `seshat status` lists all projects from XDG repos directory
- [ ] For each root project DB: read `repo_metadata` table for summary (fast, no full scan)
- [ ] For each root project: read `submodules` table, resolve submodule DB paths, read their `repo_metadata`
- [ ] Tree structure output:
  ```
  seshat              main    192 files    92 conventions    4.2 MB    5m ago
  walt-chat-backend   main    720 files    58 conventions    6.3 MB    2h ago
    └─ external/walt-portal    1204 files  18 conventions    8.1 MB    2h ago
  rust-project        main     45 files    12 conventions    1.1 MB    3d ago
  ```
- [ ] Columns: project name, branch, file count, convention count, DB size, last modified (human-readable relative time)
- [ ] Orphaned submodule DBs (parent removed or submodule removed from `.gitmodules`) shown with `⚠` warning
- [ ] Full DB paths shown with `--verbose`
- [ ] Replace current `seshat status` stub in `lib.rs`
- [ ] `Command::Status` in args.rs: add `--verbose` flag

### US-008: `repo` Parameter Activation (Low Priority)

**Description:** As an AI agent developer, I want the `repo` parameter in tool schemas validated against the loaded project, so that mismatches produce clear errors.

**Acceptance Criteria:**

- [ ] In project mode (single project): if `repo` provided and doesn't match `self.root.name`, return `REPO_NOT_FOUND` error with message: "Loaded project is '{name}'. The repo parameter is optional in single-project mode."
- [ ] `repo` value matched case-insensitively against project name
- [ ] Response envelope `repo` field reflects actual project name

**Note:** This is the lowest priority story. `repo` has minimal value in single-project stdio mode (there's only one project). Real value arrives with daemon mode (future epic).

## Functional Requirements

- FR-1: Submodules scanned by default into separate .db files (one per submodule, one level deep)
- FR-2: Submodule DBs stored at `repos/{project}/{mount_path}.db` mirroring git mount structure
- FR-3: Parent DB stores submodule metadata; child DB stores parent reference + summary cache
- FR-4: `--exclude-submodules` flag to skip submodule scanning (replaces `--include-submodules`)
- FR-5: Changed commit_hash triggers automatic submodule rescan; unchanged = skip
- FR-6: Submodules can be scanned in parallel for performance
- FR-7: Auto-scope from `file_path` parameter via longest-prefix matching against mount points
- FR-8: Explicit `scope` parameter for direct submodule targeting (full mount path)
- FR-9: Default scope is root project when no scope context provided
- FR-10: Scope reflected accurately in response envelope
- FR-11: All 5 MCP tools support `file_path` parameter for transparent auto-scoping
- FR-12: Write tools (record/update/remove_decision) write to the scoped DB, not root
- FR-13: `seshat serve` eagerly loads root + all submodule DB connections
- FR-14: `seshat status` shows tree of all projects/submodules with cached metadata
- FR-15: `repo_metadata` table caches summary stats for fast `seshat status`
- FR-16: Uninitialized submodules skipped with warning
- FR-17: Submodule branch derived from parent repository branch; commit_hash stored separately
- FR-18: `repo` parameter validated in project mode (lowest priority)
- FR-19: Scope matching uses full mount path; short names disambiguated or rejected if ambiguous

## Non-Goals (Out of Scope)

- **Daemon mode** (`--daemon`, HTTP/SSE transport, multi-project serving) — deferred to future epic
- **Recursive submodules** — only first-level submodules supported
- **Automatic garbage collection** of orphaned DBs — user manages via `seshat status` + manual deletion
- **Cross-scope queries** (`scope: "all"`) — deferred to Epic 7
- **Submodule-to-root convention inheritance** — each scope is independent
- **File watcher for submodules** — deferred to Epic 9

## Technical Considerations

### Database Migrations

**V5:** Add `submodules` table and `repo_metadata` table (applied to ALL Seshat databases):

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

### New Error Codes

Add to `ErrorCode` enum in `crates/seshat-mcp/src/envelope.rs`:

```rust
/// Scope value doesn't match any known submodule mount path.
UnknownScope,
/// Repo parameter doesn't match the loaded project name.
RepoNotFound,
```

### New `ScanProgress` Variants

Add to enum in `crates/seshat-scanner/src/orchestrator.rs`:

```rust
/// A submodule was detected during discovery.
SubmoduleDetected { name: String, mount_path: String },
/// Submodule scanning progress.
ScanningSubmodule { name: String, done: usize, total: usize },
/// Submodule scan complete.
SubmoduleScanDone { name: String },
/// Submodule unchanged — skipped.
SubmoduleUpToDate { name: String, commit_hash: String },
/// Submodule skipped (not initialized or other reason).
SubmoduleSkipped { name: String, reason: String },
```

### Scan Flow (orchestrated in scan.rs)

```
seshat scan ~/Projects/walt-chat-backend
  1. Open/create root DB
  2. Parse .gitmodules → [(mount_path, name)]
  3. Filter: skip uninitialized (empty dir, no .git) → SubmoduleSkipped events
  4. For each initialized submodule:
     a. Read stored commit_hash from root DB submodules table
     b. Get current commit_hash: git rev-parse HEAD in submodule dir
     c. If hash matches → SubmoduleUpToDate event, skip
     d. If hash differs or new:
        - Open/create submodule DB at repos/{project}/{mount_path}.db
        - Write parent metadata to submodule DB repo_metadata
        - Call scan_project_with_progress(submodule_path, config, &sub_db, cb)
        - Write summary to submodule DB repo_metadata (file_count, convention_count)
  5. Submodule scans run in PARALLEL (rayon::scope or tokio tasks)
  6. Wait for all submodule scans to complete
  7. Scan root project (excluding submodule directories) — reuse existing code
  8. Update submodules table in root DB with current commit_hashes
  9. Write summary to root DB repo_metadata
```

### Scope Resolution (scope.rs)

```rust
/// Resolve which DB connection to use.
/// Priority: explicit scope > file_path auto-detect > default root.
pub fn resolve_scope(
    scope: Option<&str>,
    file_path: Option<&str>,
    root: &ProjectConnection,
    submodules: &HashMap<String, ProjectConnection>,
    mount_paths: &[String],  // sorted longest-first
) -> Result<(&ProjectConnection, String), ErrorCode> {
    // 1. Explicit scope
    if let Some(s) = scope {
        if s == "root" { return Ok((root, "root".into())); }
        if let Some(conn) = submodules.get(s) { return Ok((conn, s.into())); }
        // Try short name match
        let matches: Vec<_> = submodules.iter()
            .filter(|(path, _)| path.ends_with(&format!("/{s}")) || path == s)
            .collect();
        match matches.len() {
            1 => return Ok((matches[0].1, matches[0].0.clone())),
            0 => return Err(ErrorCode::UnknownScope),
            _ => return Err(ErrorCode::UnknownScope), // ambiguous
        }
    }
    // 2. File path prefix match (longest prefix wins)
    if let Some(fp) = file_path {
        for mount in mount_paths {  // sorted longest-first
            if fp.starts_with(mount) {
                if let Some(conn) = submodules.get(mount.as_str()) {
                    return Ok((conn, mount.clone()));
                }
            }
        }
    }
    // 3. Default: root
    Ok((root, "root".into()))
}
```

### Breaking Change: --include-submodules → --exclude-submodules

Current Epic 4: `ScanConfig.include_submodules: bool` (default `false`), `--include-submodules` CLI flag.
New Epic 6: `ScanConfig.exclude_submodules: bool` (default `false`), `--exclude-submodules` CLI flag.

The **field rename** (not just default flip) ensures existing `seshat.toml` files with `include_submodules = false` produce a deserialization warning/ignore rather than silently changing behavior. `#[serde(default)]` on the new field means missing = `false` = scan submodules.

### Crate Changes Summary

| Crate | Changes |
|-------|---------|
| `seshat-core` | `ScanConfig`: rename `include_submodules` → `exclude_submodules` (default false) |
| `seshat-scanner` | `detect_submodule_paths()` made pub; new `get_submodule_commit_hash()` |
| `seshat-storage` | Migration V5; `SubmoduleRepository`; `RepoMetadataRepository` |
| `seshat-mcp` | New `scope.rs`; `McpServer` redesign with `ProjectConnection` + `HashMap`; all handlers call `resolve_scope()`; 2 new `ErrorCode` variants; `file_path` in all 5 request structs |
| `seshat-cli` | `scan.rs`: N+1 scan orchestration with parallel submodules; `serve.rs`: eager submodule loading; `args.rs`: `--exclude-submodules`, status command; `db.rs`: submodule path resolution; new `status.rs` |
| `seshat-graph` | No structural changes (already takes `conn` parameter) |

## Test Strategy

### Unit Tests
- `scope.rs`: explicit scope, file_path auto-detect, default, ambiguous, unknown, longest-prefix
- `SubmoduleRepository`: CRUD on submodules table
- `RepoMetadataRepository`: write/read summary cache
- `resolve_submodule_db_path()`: path construction with mount paths

### Integration Tests
- Scan fixture project with mock submodule → verify two DBs created
- Scan again with same commit → verify submodule skipped
- Scan with changed commit → verify submodule rescanned
- `query_convention` with `file_path` in submodule → returns submodule conventions
- `record_decision` with `file_path` in submodule → written to submodule DB
- `seshat status` output format with submodules

## Success Metrics

- `seshat scan` on project with submodules creates correct DB structure in <2x single-project time
- `query_convention` with `file_path` in submodule returns submodule-specific conventions
- `record_decision` with `file_path` in submodule writes to submodule DB
- `seshat status` shows tree with all projects and submodules
- Submodule rescan triggered only when commit_hash changes
- All existing tests pass (single-project mode: empty submodules HashMap, zero overhead)

## Open Questions

1. **`file_path` normalization:** Relative to project root? Strip leading `./`? Handle absolute paths by stripping project root prefix?
2. **Parallel submodule scanning:** `rayon::scope` (sync, current scan is sync) vs `tokio::spawn_blocking` (async, but scan is called from sync context in `scan.rs`)? Rayon is simpler since the scan pipeline is already rayon-based.
3. **Git commit hash retrieval:** Shell out to `git rev-parse HEAD` or use `gix` crate? `gix` is more Rust-idiomatic but adds a dependency.
