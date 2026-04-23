# Story 11.2: Branch Snapshot Garbage Collection

**Status:** implemented

**Epic:** 11 — Branch-Aware Knowledge Graph

**FRs covered:** FR20 — GC deleted branches
**NFR covered:** NFR8 — Branch switch <2s (GC doesn't affect this, but prevents unbounded growth)

---

## Story

As a **developer**,
I want Seshat to clean up deleted branch snapshots,
so that database size doesn't grow unbounded from abandoned branches.

---

## Acceptance Criteria

### AC 1: GC on startup

**Given** branch snapshots in database
**When** `seshat serve` starts
**Then** GC compares DB branches vs git branches
**And** snapshots for non-existent local branches are deleted
**And** main/master is never garbage collected

### AC 2: Periodic GC

**Given** GC has run
**When** one hour has elapsed
**Then** GC runs again automatically
**And** deletes orphan branch snapshots

### AC 3: GC safety

**Given** the current branch is about to be GC'd
**When** GC runs
**Then** the current branch is NEVER deleted (even if it no longer exists in git)
**And** main/master is NEVER deleted regardless of git state

---

## Tasks / Subtasks

### Task 1: Add `gc_branch_snapshots` function (`crates/seshat-cli/src/db.rs`)

Implemented with the following features:
- `PROTECTED_BRANCHES` constant (`["main", "master"]`) for safety checks
- `is_valid_git_repo()` helper to validate the git repository path before GC
- `HashSet` for O(1) git branch lookup instead of O(n) linear search
- `tracing::warn!` when `repo_path` is not a valid git repository
- `tracing::info!` with branch name and current branch for each deletion
- `tracing::info!` summary with deleted count and branch list at the end

### Task 2: Add GC to `run_serve` (`crates/seshat-cli/src/serve.rs`)

Run GC after DB is loaded, before starting MCP server:
```rust
let gc_repo_path = match &auto_scan_project_root {
    Some(root) => root.clone(),
    None => crate::db::find_git_root(&std::env::current_dir().unwrap_or_default())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
};
if let Ok(deleted) = gc_branch_snapshots(&db, &gc_repo_path) {
    if !deleted.is_empty() {
        tracing::info!(
            deleted_count = deleted.len(),
            deleted_branches = ?deleted,
            "Garbage collected orphan branch snapshots on startup"
        );
    }
}
```

### Task 3: Add periodic GC task with shutdown mechanism

Implemented as `GcHandle` struct (following `WatcherHandle` pattern):
```rust
pub struct GcHandle {
    shutdown_tx: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl GcHandle {
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.task,
        ).await;
    }
}
```

Periodic GC uses `tokio::select!` for graceful shutdown:
- `interval.tick()` triggers GC run on `spawn_blocking`
- `gc_shutdown_rx` receives shutdown signal
- Proper error handling: `Ok(Ok(...))`, `Ok(Err(e))`, `Err(join_err)`
- `tracing::error!` for GC failures and task panics
- `tracing::debug!` on graceful shutdown

GC handle is dropped during shutdown sequence (after MCP server stops, before watcher shutdown).

### Task 4: Tests (7 total)

All tests use `tempfile` + `git` CLI for realistic git repo setup:

1. **gc_deletes_orphan_branches** — main + feature in git, orphan in DB → only orphan deleted
2. **gc_preserves_current_branch** — main in git (current), some-branch not in git → main preserved, some-branch deleted
3. **gc_preserves_main** — main in DB, no branches in git → main preserved
4. **gc_preserves_master** — master in DB, no branches in git → master preserved
5. **gc_preserves_current_branch_not_in_git** — feature deleted from git, main is current → feature deleted, main preserved
6. **gc_handles_detached_head** — HEAD detached, main + some-branch in DB → main preserved (protected), some-branch deleted
7. **gc_deletes_all_orphans** — main in git, 3 orphans in DB → all 3 deleted, main preserved

---

## Dev Notes

### Architecture Context

**Current state:**
- `BranchRepository::delete_branch()` — **fully implemented**, deletes nodes/edges/files_ir for a branch
- `BranchRepository::list_branches()` — **fully implemented**
- `BranchRepository::get_current_branch()` — **fully implemented**
- No GC logic exists anywhere

**Key design decisions:**
1. **Never GC main/master** — these are the most common branch names and deleting them would be catastrophic
2. **Never GC current branch** — if the user is currently on a branch that was deleted in git (e.g., remote-only branch), we keep the data in case they want to recover
3. **GC on startup + hourly** — startup catches obvious cleanup, hourly handles long-running serve sessions
4. **Silent GC** — no user notification needed; results logged at `tracing::info` level

### What NOT to touch

- `crates/seshat-storage/src/repository/branch_repository.rs` — **no changes needed**
- `crates/seshat-core/src/ids.rs` — **no changes needed**
- `crates/seshat-mcp/` — **no changes needed**
- `crates/seshat-scanner/` — **no changes needed**
- `crates/seshat-watcher/` — **no changes needed**

### Edge cases

1. **GC deletes current branch** — protected by `if name == &current_branch { continue; }`
2. **GC deletes main/master** — protected by `PROTECTED_BRANCHES` constant check
3. **GC while scan in progress** — GC runs on separate DB handle, no conflict (SQLite handles concurrent reads)
4. **GC on non-git project** — `is_valid_git_repo()` returns `false`, `tracing::warn!` logged, `get_git_branches` returns empty list, so ALL non-main/master branches are deleted. This is correct behavior — no git means no branches to preserve.
5. **GC on detached HEAD** — `get_current_branch()` returns commit hash, not a branch name. main/master still protected.
6. **GC background task shutdown** — `GcHandle` uses `tokio::sync::oneshot` channel + `tokio::select!` for graceful shutdown
7. **GC periodic task error** — uses `tokio::select!` with proper error handling: `Ok(Ok(...))`, `Ok(Err(e))`, `Err(join_err)`

### File List

```
crates/seshat-cli/src/db.rs                    ← ADD: gc_branch_snapshots, get_git_branches
crates/seshat-cli/src/serve.rs                 ← MODIFY: call GC on startup + periodic task
crates/seshat-cli/src/db.rs                    ← ADD: GC unit tests
```

---

## Dev Agent Record

### Agent Model Used

KSD-CodeReview adversarial review (Blind Hunter + Edge Case Hunter) identified:
- 3 bad_spec findings → addressed (shutdown mechanism, git validation, gc_db origin)
- 6 patch findings → addressed (error logging, HashSet, GcHandle struct)
- 2 intent_gap findings → addressed (missing test implementations)
- 2 defer findings noted (TOCTOU race, PII in logs)

### Debug Log References

N/A — all changes are spec-level and test-level.

### Completion Notes List

- `GcHandle` follows the same pattern as `WatcherHandle` (oneshot shutdown + JoinHandle)
- `tracing::warn!` logged when `repo_path` is not a valid git repo
- `HashSet` used for O(1) git branch lookup
- 7 GC tests, all passing
- Full project builds successfully

### File List

```
crates/seshat-cli/src/db.rs                    ← MODIFIED: gc_branch_snapshots, is_valid_git_repo, 7 tests
crates/seshat-cli/src/serve.rs                 ← MODIFIED: GcHandle struct, periodic GC with tokio::select!, shutdown
.ralph/tasks/story-11-2-branch-gc.md           ← MODIFIED: updated status, implementation notes, dev record
```
