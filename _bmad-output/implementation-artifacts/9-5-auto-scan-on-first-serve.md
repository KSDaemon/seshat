# Story 9.5: Auto-Scan on First `seshat serve`

Status: ready-for-dev

## Story

As a **developer**,
I want `seshat serve` to automatically scan my project in the background on startup when no database exists,
so that I get a zero-config experience without running `seshat scan` manually.

## Acceptance Criteria

1. **Given** `seshat serve` starts in a directory with no existing DB
   **Then** server starts successfully (no error), creates an empty DB, and begins scanning the project in the background

2. **Given** background scan is in progress
   **When** AI agent calls any MCP tool
   **Then** wait for scan to complete, then return normal response with `"auto_scanned": true, "first_run": true` in metadata

3. **Given** background scan completed before first MCP call
   **Then** all tool calls work normally with `"auto_scanned": true, "first_run": true` in first response metadata only

4. **Given** project exceeds `auto_scan_limit` files (default 50,000)
   **Then** cancel auto-scan and return error on MCP calls: `"Project too large for auto-scan. Run: seshat scan"`

5. **Given** `seshat serve` starts in a non-git directory
   **Then** auto-scan still proceeds (branch="main", no git dates, no submodules — graceful degradation already supported by `scan_project`)

6. **Given** auto-scan fails
   **Then** return error on MCP calls with actionable message and `seshat scan --verbose` suggestion

7. **Given** `seshat serve` starts with existing DB
   **Then** no auto-scan triggered, normal behavior unchanged (backward compatible)

8. **Given** auto-scan is running
   **Then** startup banner shows: `Files: 0 (auto-scanning...)`, `Watcher: starting (after scan)`

9. **Given** auto-scan completes successfully
   **Then** watcher starts automatically (not before, since watcher needs a populated DB)

## Tasks / Subtasks

- [ ] **Task 1: Add `ServeTarget` enum and `resolve_serve_db_or_project_root()`** (AC: 1, 7)
  - [ ] In `crates/seshat-cli/src/db.rs`, add:
    ```rust
    pub(crate) enum ServeTarget {
        ExistingDb { db_path: PathBuf },
        AutoScan { project_root: PathBuf, db_path: PathBuf },
    }
    ```
  - [ ] New function `resolve_serve_db_or_project_root(explicit_repo: Option<&Path>) -> Result<ServeTarget, CliError>`
  - [ ] Logic mirrors `resolve_serve_db` but when no `.db` file found:
    - Determine `project_root`: explicit repo arg → git root (walk up from cwd) → cwd itself
    - Compute `db_path` via `resolve_db_path(&project_root)`
    - Return `ServeTarget::AutoScan { project_root, db_path }`
  - [ ] When `.db` exists: return `ServeTarget::ExistingDb { db_path }` (zero behavior change)
  - [ ] When no DB and no determinable project root: error as before
  - [ ] Add `auto_scan_limit: usize` field to `ScanConfig` (default: 50_000)

- [ ] **Task 2: Add `ScanState` type to `seshat-mcp`** (AC: 2, 3, 6)
  - [ ] In `crates/seshat-mcp/src/server.rs`, add:
    ```rust
    use std::sync::Mutex;
    use tokio::sync::Notify;

    pub struct ScanState {
        inner: Arc<Mutex<ScanStateInner>>,
        notify: Arc<Notify>,
    }

    enum ScanStateInner {
        NotNeeded,
        InProgress,
        Complete { auto_scanned: bool },
        Failed { error_message: String },
    }
    ```
  - [ ] `ScanState::not_needed()` — constructor for existing-DB case
  - [ ] `ScanState::in_progress()` — constructor for auto-scan case
  - [ ] `ScanState::mark_complete()` — transition InProgress → Complete, notify waiters
  - [ ] `ScanState::mark_failed(msg)` — transition InProgress → Failed, notify waiters
  - [ ] `async fn wait_for_scan(&self)` — if InProgress, await `notify.notified()`; return immediately otherwise
  - [ ] `fn auto_scanned(&self) -> bool` — true if state is Complete and auto_scanned=true
  - [ ] `fn is_first_run(&self) -> bool` — true if auto_scanned (first scan ever for this project)
  - [ ] `fn error_message(&self) -> Option<String>` — Some if Failed

- [ ] **Task 3: Add `AUTO_SCAN_FAILED` error code** (AC: 6)
  - [ ] In `crates/seshat-mcp/src/envelope.rs`, add variant to `ErrorCode`:
    ```rust
    AutoScanFailed,
    ```
  - [ ] Add display mapping: `Self::AutoScanFailed => "AUTO_SCAN_FAILED"`
  - [ ] Add error message template with `seshat scan --verbose` suggestion

- [ ] **Task 4: Wire scan-await guard into `execute_tool`** (AC: 2, 3, 6)
  - [ ] Add `scan_state: ScanState` field to `McpServer`
  - [ ] Update `McpServer::new()` and `McpServer::with_embedding()` to accept `ScanState`
  - [ ] In the `execute_tool` helper (used by all tool handlers), before executing any tool logic:
    ```rust
    self.scan_state.wait_for_scan().await;
    if let Some(err) = self.scan_state.error_message() {
        return Err(/* AUTO_SCAN_FAILED envelope */);
    }
    ```
  - [ ] In first-response metadata, include:
    ```rust
    if self.scan_state.auto_scanned() {
        metadata.insert("auto_scanned".into(), serde_json::Value::Bool(true));
        metadata.insert("first_run".into(), serde_json::Value::Bool(true));
    }
    ```
  - [ ] Only include `auto_scanned`/`first_run` in the **first** successful response after auto-scan (use `AtomicBool` flag `first_response_sent` to gate this)

- [ ] **Task 5: Launch background scan in `run_serve`** (AC: 1, 4, 5, 7, 8, 9)
  - [ ] In `crates/seshat-cli/src/serve.rs`, replace `resolve_serve_db` call with `resolve_serve_db_or_project_root`
  - [ ] Match on `ServeTarget`:
    - `ExistingDb { db_path }` → current flow unchanged, `ScanState::not_needed()`
    - `AutoScan { project_root, db_path }` → new auto-scan flow:
      1. Create empty DB: `Database::open(&db_path)` (refinery migrations auto-apply)
      2. Create `ScanState::in_progress()`
      3. Spawn background scan task:
         ```rust
         let scan_state_clone = scan_state.clone();
         let scan_config = config.scan.clone();
         let db_path_clone = db_path.clone();
         tokio::spawn(async move {
             let result = tokio::task::spawn_blocking(move || {
                 scan_project(&project_root, &scan_config, &db)
             }).await;
             match result {
                 Ok(Ok(_scan_result)) => scan_state_clone.mark_complete(),
                 Ok(Err(scan_err)) => scan_state_clone.mark_failed(scan_err.to_string()),
                 Err(join_err) => scan_state_clone.mark_failed(join_err.to_string()),
             }
         });
         ```
      4. Start MCP server immediately (don't wait for scan)
      5. After `ScanState` transitions to Complete → start watcher
  - [ ] For watcher start after auto-scan: use a second spawned task that awaits `scan_state.wait_for_scan()` then calls `start_watcher()`

- [ ] **Task 6: File count pre-check** (AC: 4)
  - [ ] Before spawning full `scan_project`, run lightweight file discovery:
    ```rust
    let discovery_result = discover_files(&project_root, &scan_config)?;
    if discovery_result.files.len() > scan_config.auto_scan_limit {
        scan_state.mark_failed(format!(
            "Project too large for auto-scan ({} files). Run: seshat scan",
            discovery_result.files.len()
        ));
        // Don't spawn scan, just continue with empty DB
        return; // MCP calls will get AUTO_SCAN_FAILED error
    }
    ```
  - [ ] Import `discover_files` from `seshat_scanner` in `serve.rs` (it's already public)
  - [ ] Since `discover_files` is sync, wrap in `spawn_blocking` or call it before the async block

- [ ] **Task 7: Update startup banner** (AC: 8)
  - [ ] When `ServeTarget::AutoScan`:
    ```
    seshat v0.x.x

      Repo:         my-project
      Branch:       main
      Files:        0 (auto-scanning...)
      Conventions:  0
      Database:     ~/.local/share/seshat/repos/my-project.db
      Watcher:      starting (after scan)
    ```
  - [ ] When `ServeTarget::ExistingDb`: no change to current banner

- [ ] **Task 8: Add `auto_scan_limit` to config** (AC: 4)
  - [ ] In `crates/seshat-core/src/config.rs`, add to `ScanConfig`:
    ```rust
    pub auto_scan_limit: usize,
    ```
  - [ ] Default: `50_000`
  - [ ] In `seshat.toml.example`, add under `[scan]`:
    ```toml
    # auto_scan_limit = 50000
    ```

- [ ] **Task 9: Tests** (AC: 1–9)
  - [ ] Unit tests in `db.rs`:
    - `resolve_serve_db_or_project_root_returns_auto_scan_when_no_db`
    - `resolve_serve_db_or_project_root_returns_existing_db_when_present`
    - `resolve_serve_db_or_project_root_uses_cwd_when_no_git`
  - [ ] Unit tests in `server.rs`:
    - `scan_state_not_needed_returns_immediately`
    - `scan_state_in_progress_waits_for_complete`
    - `scan_state_in_progress_waits_for_failed`
    - `scan_state_failed_returns_error_message`
    - `scan_state_auto_scanned_flag`
  - [ ] Unit tests in `envelope.rs`:
    - `auto_scan_failed_error_code_serializes`
  - [ ] Integration test in `crates/seshat-cli/tests/`:
    - `serve_auto_scan_blocks_first_tool_call`: create temp dir with source files, start serve (no prior scan), call tool, verify response contains data + auto_scanned=true
    - `serve_auto_scan_too_large`: temp dir with >limit files, start serve, call tool, verify AUTO_SCAN_FAILED error
    - `serve_existing_db_no_auto_scan`: existing DB, start serve, verify no auto-scan triggered

## Dev Notes

### Architecture Context

**Data flow (auto-scan path):**
```
seshat serve (no DB)
  → resolve_serve_db_or_project_root() → ServeTarget::AutoScan
  → Database::open(db_path)  (creates empty DB with migrations)
  → ScanState::in_progress()
  → spawn_blocking(scan_project)
  → start MCP server immediately
  → [first MCP tool call arrives]
      → wait_for_scan() (blocks if scan in progress)
      → scan completes → mark_complete() → notify()
      → tool executes normally, metadata includes auto_scanned=true
  → [background] watcher starts after scan completes
```

**Data flow (existing DB path — no change):**
```
seshat serve (existing DB)
  → resolve_serve_db_or_project_root() → ServeTarget::ExistingDb
  → current flow unchanged
  → ScanState::not_needed()
  → wait_for_scan() returns immediately
```

### Key: `scan_project` is synchronous

`scan_project` uses `rayon` (blocking). Must wrap in `tokio::task::spawn_blocking`:
```rust
tokio::task::spawn_blocking(move || {
    scan_project(&project_root, &scan_config, &db)
})
```

### Key: Watcher cannot start before scan

The file watcher (`start_watcher`) needs a populated DB to do incremental updates. Starting it before scan completes would cause race conditions. Solution: start watcher in a separate spawned task that awaits `scan_state.wait_for_scan()`.

### Key: Non-git directories work fine

`scan_project` already handles non-git directories:
- `collect_git_file_dates` returns empty `HashMap` (trends = Unknown)
- `discover_files` uses `ignore` crate, doesn't require git
- Branch defaults to `"main"` (hardcoded in orchestrator.rs line 194)
- No submodules detected (no `.gitmodules`)

### Key: `discover_files` for pre-check

`discover_files` is fast (just file listing via `ignore` crate, no parsing). Use it to check file count before committing to the expensive scan. The function is at `crates/seshat-scanner/src/discovery.rs` and is publicly re-exported.

However, `discover_files` returns `Result<DiscoveryResult, ScanError>` and requires `ScanConfig`. Since `scan_project_with_progress` internally calls `discover_files` again, there's a minor duplication. This is acceptable because discovery is fast (typically <1s even for large projects).

### Key: `auto_scan_limit` check timing

The limit check must happen **before** spawning the expensive scan task. Two approaches:
1. **Pre-discovery check**: Call `discover_files` separately, check count, then call `scan_project` (which re-discovers). Minor duplication but clean separation.
2. **Inside orchestrator**: Add early-exit check in `scan_project_with_progress` after discovery. Requires adding a new `ScanError` variant.

Approach 1 is simpler and keeps `seshat-scanner` unchanged. The duplication cost is negligible.

### Existing patterns to reuse

**`Database::open()`** — creates DB + runs migrations, already works with new paths:
```rust
// serve.rs line 84
let db = Database::open(&db_path).map_err(|e| CliError::CommandFailed { ... })?;
```

**`scan_project()`** — fully sync, takes `&Path, &ScanConfig, &Database`:
```rust
// crates/seshat-scanner/src/orchestrator.rs line 151
pub fn scan_project(root: &Path, config: &ScanConfig, db: &Database) -> Result<ScanResult, ScanError>
```

**`start_watcher()`** — async, takes `WatcherParams, PathBuf, PathBuf, ...`:
```rust
// serve.rs lines 166-189 — current watcher start pattern
```

**`ProjectConnection::new()`** — wraps `Arc<Mutex<Connection>>`:
```rust
// serve.rs line 126-130
let root = ProjectConnection::new(db.connection().clone(), repo_info.name.clone(), repo_info.branch.to_string());
```

### `ScanState` thread safety

`ScanState` must be `Send + Sync + Clone` (held by `McpServer` which is `Clone`).
- `inner: Arc<Mutex<ScanStateInner>>` — thread-safe state transitions
- `notify: Arc<Notify>` — tokio async notification primitive (Send + Sync)
- All methods are `&self` (immutable) — mutation goes through `Mutex`

### `first_run` metadata — only once

The `auto_scanned`/`first_run` metadata should appear in only the **first** successful MCP response after auto-scan. Use an `AtomicBool` inside `ScanState`:
```rust
first_response_sent: Arc<AtomicBool>,
```
In `execute_tool`, after successful tool execution:
```rust
if scan_state.auto_scanned() && !scan_state.first_response_sent.swap(true, Ordering::Relaxed) {
    metadata.insert("auto_scanned".into(), Value::Bool(true));
    metadata.insert("first_run".into(), Value::Bool(true));
}
```

### What NOT to touch

- `crates/seshat-scanner/src/orchestrator.rs` — no changes needed, `scan_project` works as-is
- `crates/seshat-watcher/` — no changes, watcher start is just moved later in serve.rs
- `crates/seshat-storage/` — no changes, `Database::open` creates DB with migrations already
- Existing tool handlers in `crates/seshat-mcp/src/tools/` — no changes, `execute_tool` guard handles everything
- `crates/seshat-cli/src/scan.rs` — no changes

### Crate dependency note

`crates/seshat-cli` already depends on `seshat-scanner` (for the `scan` command). The `discover_files` function is available. No new crate dependencies needed.

### Error handling for auto-scan

When auto-scan fails, the MCP server is already running and accepting connections. Tool calls should return a structured error:
```json
{
  "status": "error",
  "tool": "query_project_context",
  "repo": "my-project",
  "error": {
    "code": "AUTO_SCAN_FAILED",
    "message": "Auto-scan failed: <scan_error_message>",
    "suggestion": "Run: seshat scan --verbose /path/to/project"
  }
}
```

### File List

```
crates/seshat-cli/src/db.rs                  ← MODIFY: add ServeTarget + resolve_serve_db_or_project_root
crates/seshat-cli/src/serve.rs               ← MODIFY: auto-scan launch logic, watcher start after scan, banner
crates/seshat-mcp/src/server.rs              ← MODIFY: ScanState type, execute_tool guard, first_run metadata
crates/seshat-mcp/src/envelope.rs            ← MODIFY: AUTO_SCAN_FAILED error code
crates/seshat-core/src/config.rs             ← MODIFY: auto_scan_limit field
seshat.toml.example                          ← MODIFY: document auto_scan_limit
```

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
