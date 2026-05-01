# Story 11.2: Branch Switch Orchestration (ADR-14)

**Status:** ready-for-dev

**Epic:** 11 — Branch-Aware Knowledge Graph

**FRs covered:** FR17 (per-branch snapshots), FR18 (instant branch switch), FR19 (background sync), FR20 (GC deleted branches)
**ARCH:** ADR-14 (branch switch flow)
**NFR covered:** NFR8 — Branch switch <2s

---

## Story

As a **developer using `seshat serve`**,
I want Seshat to detect `git checkout` and instantly switch knowledge graph context,
so that my AI agent always works against the correct branch without manual re-scan.

---

## Acceptance Criteria

### AC 1: Unified `detect_branch` implementation

**Given** any git repo (main repo, worktree, detached HEAD)
**When** `detect_branch(path)` is called
**Then** a single implementation in `crates/seshat-cli/src/db.rs` handles all cases:
  - Normal repo: reads `.git/HEAD`
  - Worktree: parses `gitdir:` in `.git` file, reads resolved HEAD
  - Detached HEAD: returns abbreviated commit hash (7+ chars hex)
  - Path `..` components: normalized before resolution
**And** the duplicate `detect_branch_from_path()` in `crates/seshat-watcher/src/lib.rs` is REMOVED
**And** watcher code calls the shared function from `seshat-cli::detect_branch`

### AC 2: Fix `ExistingDb` branch detection bug

**Given** `seshat serve` starts with an existing DB (non-auto-scan path)
**When** it detects the branch
**Then** it uses `project_root` from `repo_metadata` (not `db_path.parent()`)
**And** the detected branch is used for the switch handler

### AC 3: Replace all `BranchId::from("main")` hardcodes in production

**Given** any production code path
**When** a `BranchId` is needed
**Then** it comes from `detect_branch()` (dynamic detection), never hardcoded
**And** the remaining fallback locations are all instrumented with `tracing::debug!`
**And** test code hardcodes are unchanged (tests pass their own parameter)

Affected production locations:
- `crates/seshat-cli/src/db.rs:75` — `load_project_info()`
- `crates/seshat-cli/src/scan.rs:96` — `run_scan()`
- `crates/seshat-cli/src/scan.rs:281` — submodule branch fallback
- `crates/seshat-cli/src/serve.rs:81-85` — `detect_branch()` fallback
- `crates/seshat-cli/src/serve.rs:180-181` — `handle_auto_scan_snapshot()` for non-main
- `crates/seshat-cli/src/serve.rs:201` — snapshot source branch
- `crates/seshat-storage/src/repository/branch_repository.rs:145` — `QueryReturnedNoRows`

### AC 4: ADR-14 branch switch handler (replace bulk rescan)

**Given** Seshat is serving (`seshat serve` with watcher enabled)
**When** `.git/HEAD` changes (detected by watcher notify events)
**Then** instead of bulk rescan, the following runs:

1. `detect_branch()` → `new_branch_id`
2. If new branch == current branch → **no-op** (just metadata refresh)
3. If snapshot exists for `new_branch_id` in DB → `switch_branch(new_branch_id)` — instant (<1s)
4. If NO snapshot exists → `create_snapshot(current → new)` → `switch_branch(new_branch_id)`
5. Spawn background sync: `tokio::spawn(sync_changed_files(old_commit_hash, new_branch_id))`

**And** the old bulk-rescan-on-HEAD-change code at `watcher/src/lib.rs:263` and `watcher/src/hot_tier.rs:76-84` is REPLACED

### AC 5: Background sync — diff-based incremental update

**Given** a branch switch just happened
**When** background sync runs
**Then** it uses `gix` to diff old commit tree vs new commit tree
**And** re-parses only files that changed between commits (not full project)
**And** upserts changed files' IR into `files_ir` for the new branch
**And** deletes IR for files removed in the new branch
**And** once sync completes: sets `has_pending_changes = true` → warm tier fires next cycle (rebuilds convention aggregates)
**And** total sync time is proportional to changed files, not project size

### AC 6: MCP metadata during background sync

**Given** background sync is in progress
**When** ANY MCP tool is called
**Then** responses include `metadata.syncing: true` and `metadata.snapshot_based: true`
**And** once sync completes: `syncing` and `snapshot_based` are removed from metadata

### AC 7: Branch GC integration

**Given** `seshat serve` starts (existing functionality)
**Then** `gc_branch_snapshots()` runs on startup (already implemented ✅)
**And** periodic GC runs every hour via background task (already implemented ✅)
**And** GC never deletes the current branch or `main`/`master` (already implemented ✅)

### AC 8: Integration tests

All tests in `crates/seshat-cli/tests/worktree_integration.rs` (5 existing + 7 new = 12 total)

**New tests:**

| # | Test | Coverage |
|---|------|----------|
| 1 | `branch_switch_via_watcher_updates_metadata` | Watcher .git/HEAD change → current_branch updated |
| 2 | `branch_switch_to_existing_snapshot_is_instant` | Snapshot exists → switch time <2s |
| 3 | `branch_switch_creates_snapshot_when_missing` | No snapshot → create then switch |
| 4 | `background_sync_reparses_changed_files_only` | git diff → only changed files re-parsed |
| 5 | `detached_head_returns_commit_hash` | Detached HEAD → hash (not "main") |
| 6 | `unified_detect_branch_same_behavior_in_serve_and_watcher` | Both code paths call same function |
| 7 | `detect_branch_normalizes_gitdir_path_components` | `..` in gitdir path → normalized |

### AC 9: Detached HEAD handling

**Given** repo is in detached HEAD state
**When** `detect_branch()` is called
**Then** it returns the abbreviated commit hash (e.g., `"b801a98"`)
**And** this hash is used as `BranchId` (treating it as a branch name)
**And** `query_project_context` metadata includes `"detached_head": true`

---

## Tasks / Subtasks

### Task 1: Unify `detect_branch` — merge two implementations

- [ ] Move `detect_branch_from_path()` logic from `crates/seshat-watcher/src/lib.rs` into `crates/seshat-cli/src/db.rs::detect_branch()`
- [ ] Merge best of both: path normalization from db.rs + simple loop from watcher
- [ ] Add detached HEAD handling: parse HEAD → if not `ref:` → return commit hash
- [ ] Add `..` component normalization: resolve path through `canonicalize()` or manual component stripping
- [ ] Remove `fn detect_branch_from_path()` from `crates/seshat-watcher/src/lib.rs`
- [ ] Update watcher call sites to use `seshat_cli::detect_branch(&path)`
- [ ] Update `serve.rs::detect_branch()` to be a thin wrapper or removed
- [ ] Unit tests: normal repo, worktree file, worktree nested, detached HEAD, no git
- [ ] Typecheck passes: `cargo check -p seshat-watcher -p seshat-cli`

### Task 2: Fix `ExistingDb` branch detection path

- [ ] In `serve.rs::run_serve()`, for `ExistingDb` path:
  - Read `repo_metadata` to get `project_root` (already available via `repo_info.project_root`)
  - Pass `project_root` to `detect_branch()` instead of `db_path.parent()`
- [ ] Unit test: mock serve flow, assert detect_branch receives correct path
- [ ] Typecheck passes

### Task 3: Replace 8 `BranchId::from("main")` hardcodes

- [ ] `db.rs:75` — `load_project_info()`: use `branch_repo.get_current_branch()` result directly (already does: `.unwrap_or_else(|_| BranchId::from("main"))`)
  - Change to: `.unwrap_or_else(|e| { tracing::debug!(%e, "using default branch"); BranchId::from("main") })` — same behavior, better observability
- [ ] `scan.rs:96` — `run_scan()`: same pattern
- [ ] `serve.rs:81-85` — already correct (fallback with debug log)
- [ ] `serve.rs:180-181, 201` — `handle_auto_scan_snapshot()`: already dynamic, just verify no regressions
- [ ] `branch_repository.rs:145` — `get_current_branch()` fallback: keep as is (storage layer must work standalone)
- [ ] `serve.rs:682` — `open_submodule_connections()`: same pattern as db.rs
- [ ] Instrument all remaining fallbacks with `tracing::debug!`
- [ ] Typecheck passes

### Task 4: Replace watcher bulk-rescan with ADR-14 snapshot switch

- [ ] In `crates/seshat-watcher/src/hot_tier.rs:76-84`:
  - Replace `info!("Branch switch detected, triggering full rescan")` + `on_bulk_rescan(path)` with:
    - `detect_branch()` → `new_branch`
    - Send `SwitchEvent { old_branch, new_branch }` through a new channel to serve
- [ ] In `crates/seshat-cli/src/serve.rs`:
  - Listen for `SwitchEvent` from watcher channel
  - Implement `handle_branch_switch(db, new_branch, old_branch)`:
    1. Check if `new_branch` snapshot exists (`list_branches`)
    2. If YES → `switch_branch(new_branch)` + log "instant switch"
    3. If NO → `create_snapshot(old → new)` → `switch_branch(new)`
    4. Spawn `tokio::spawn(async { background_sync(db, old, new).await })`
  - Ensure handler runs async and doesn't block the watcher task
- [ ] Replace `on_bulk_rescan` callback with `on_branch_switch` callback in `WatcherHandle`/params
- [ ] Instrument with timings: `tracing::info!(elapsed_ms = %, "Branch switch completed")`
- [ ] Typecheck passes

### Task 5: Background sync — diff-based incremental update

- [ ] Implement `async fn background_sync(db, old_branch, new_branch)`
  1. Get old commit hash: stored in `repo_metadata` (or via `gix::head_commit`)
  2. Get new commit via `gix::head_commit()`
  3. `gix::object::tree::diff(old_tree, new_tree)` → `Vec<(ChangeType, PathBuf)>`
  4. For each changed file:
     - Modified → re-parse via `parse_file()`, upsert into `files_ir` for `new_branch`
     - Added → same
     - Removed → `delete_by_path` from `files_ir` for `new_branch`
  5. On completion: emit `warm_tier::mark_pending()` (existing mechanism)
  6. On error: `tracing::error!(%error, "Background sync failed")` — non-fatal
- [ ] Handle case where `gix::tree::diff` fails (old commit not available) → fall back to full rescan
- [ ] Integration test: modify a file between branches, assert only that file re-parsed

### Task 6: MCP metadata — `syncing` and `snapshot_based`

- [ ] Add `sync_in_progress: Arc<AtomicBool>` to `ProjectConnection` or `McpServer`
- [ ] Background sync sets it to `true` at start, `false` at completion
- [ ] In `McpServer::execute_tool()` (or response envelope builder):
  - If `sync_in_progress.load(Ordering::Relaxed)`: add `"syncing": true, "snapshot_based": true` to `_metadata`
- [ ] Ensure all 5 tools propagate this metadata (if already handled by envelope, just verify)
- [ ] Typecheck passes

### Task 7: Integration tests (7 new tests in `worktree_integration.rs`)

- [ ] `branch_switch_via_watcher_updates_metadata`
  - Create repo on main, scan, start watcher, checkout feature → assert `get_current_branch() == "feature"`
- [ ] `branch_switch_to_existing_snapshot_is_instant`
  - Pre-create snapshot for feature branch, trigger switch → assert switch time <2s
- [ ] `branch_switch_creates_snapshot_when_missing`
  - Ensure no snapshot for branch, trigger switch → assert snapshot created
- [ ] `background_sync_reparses_changed_files_only`
  - Create file in old branch (committed), switch to new branch (same file absent) → assert background sync removes file IR for new branch
- [ ] `detached_head_returns_commit_hash`
  - `git checkout HEAD~0` (detach) → `detect_branch()` returns hash, not "main"
- [ ] `unified_detect_branch_same_behavior_in_serve_and_watcher`
  - Call `detect_branch()` from serve AND watcher paths → assert same result
- [ ] `detect_branch_normalizes_gitdir_path_components`
  - Create worktree, add `../..` in gitdir path → assert function normalizes correctly

### Task 8: Code review + final cleanup

- [ ] Run full test suite: `cargo test`
- [ ] Run clippy: `cargo clippy -- -D warnings`
- [ ] Run fmt: `cargo fmt -- --check`
- [ ] Code review (adversarial review workflow)
- [ ] Update `epics.md` — mark Epic 11 as **[COMPLETED]**
- [ ] Mark this story file as complete, add Dev Agent Record

---

## File List

Files to create:
- _NONE_ — all new code goes into existing files

Files to modify:
- `crates/seshat-cli/src/db.rs` — unified `detect_branch()`, worktree handling (already mostly there)
- `crates/seshat-cli/src/serve.rs` — fix ExistingDb path, ADR-14 switch handler, MCP metadata
- `crates/seshat-watcher/src/lib.rs` — remove `detect_branch_from_path()`, add switch event channel
- `crates/seshat-watcher/src/hot_tier.rs` — replace bulk rescan with switch event emission
- `crates/seshat-cli/src/scan.rs` — instrument fallbacks
- `crates/seshat-cli/tests/worktree_integration.rs` — 7 new tests
- `_bmad-output/planning-artifacts/epics.md` — mark Epic 11 COMPLETED

Files already correct (no changes needed):
- `crates/seshat-storage/src/repository/branch_repository.rs` — fully implemented
- `crates/seshat-scanner/src/orchestrator.rs` — already accepts `BranchId` parameter
- `crates/seshat-watcher/src/warm_tier.rs` — no branch logic
- `crates/seshat-watcher/src/events.rs` — no branch logic

---

## Code Patterns (Seshat conventions)

- **Error types per crate**: `CliError` in `seshat-cli`, `WatcherError` in `seshat-watcher`
- **Async**: `tokio::spawn` for background tasks, `tokio::sync` channels (mpsc, oneshot, broadcast)
- **Logging**: `tracing::info!` for key events (branch switch detected), `tracing::debug!` for fallbacks, `tracing::warn!` for non-fatal errors
- **DB**: `Arc<Mutex<Connection>>` for write access — repository traits already handle this
- **Tests**: `#[cfg(test)] mod tests` at bottom of each file, `tempfile::TempDir` for filesystem tests, in-memory DB for unit tests
- **Branch**: `BranchId(pub String)` via `seshat_core` — newtype with `Display` and `From<String>`
- **git**: `gix` crate (already a dependency) for branch listing, commit resolution; raw HEAD file reading for simple current-branch detection
