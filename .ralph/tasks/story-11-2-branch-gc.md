# Story 11.2: Branch Snapshot Garbage Collection

**Status:** ready-for-dev

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

### Task 1: Add `gc_branch_snapshots` function (`crates/seshat-cli/src/db.rs` or new `gc.rs`)

```rust
/// Garbage collect branch snapshots for branches that no longer exist in git.
///
/// Compares branches in the database against branches in git.
/// Deletes snapshots for branches that exist in DB but not in git.
///
/// Safety: never deletes the current branch or "main"/"master".
pub(crate) fn gc_branch_snapshots(
    db: &Database,
    git_root: Option<&Path>,
) -> Result<Vec<String>, CliError> {
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());

    // Get all branches in DB
    let db_branches = branch_repo.list_branches().map_err(|e| {
        CliError::CommandFailed {
            command: "serve".to_owned(),
            reason: format!("failed to list branches: {e}"),
        }
    })?;

    if db_branches.is_empty() {
        return Ok(Vec::new());
    }

    // Get current branch (never GC this)
    let current_branch = branch_repo.get_current_branch().unwrap_or_else(|_| {
        BranchId::from("main")
    });

    // Get git branches
    let git_branches = match git_root {
        Some(root) => get_git_branches(root),
        None => Vec::new(),
    };

    let mut deleted = Vec::new();

    for branch in &db_branches {
        // Never GC main/master
        if branch.0 == "main" || branch.0 == "master" {
            continue;
        }

        // Never GC current branch
        if branch == &current_branch {
            continue;
        }

        // Never GC if branch still exists in git
        if git_branches.contains(&branch.0) {
            continue;
        }

        // Delete this orphan branch
        branch_repo.delete_branch(branch).map_err(|e| {
            CliError::CommandFailed {
                command: "serve".to_owned(),
                reason: format!("failed to delete branch '{}': {e}", branch.0),
            }
        })?;

        deleted.push(branch.0.clone());
    }

    Ok(deleted)
}

/// Get list of local git branch names for the repository at `root`.
///
/// Uses `gix` to discover branches. Returns empty list if not a git repo.
fn get_git_branches(root: &Path) -> Vec<String> {
    let mut branches = Vec::new();

    if let Ok(repo) = gix::discover(root) {
        // Get local branches (refs/heads/*)
        if let Ok(refs) = repo.references() {
            let mut all_refs = refs.all().ok()?;
            while let Some(entry) = all_refs.next().ok()? {
                let entry = entry.ok()?;
                if let Some(name) = entry.name().short_name() {
                    if name.starts_with("refs/heads/") {
                        if let Some(branch) = name.strip_prefix("refs/heads/") {
                            branches.push(branch.to_string());
                        }
                    }
                }
            }
        }
    }

    branches
}
```

### Task 2: Add GC to `run_serve` (`crates/seshat-cli/src/serve.rs`)

Run GC after DB is loaded, before starting MCP server:

```rust
// After loading DB, before starting watcher:
let gc_result = crate::db::gc_branch_snapshots(&db, Some(&project_root));
if let Ok(deleted) = &gc_result {
    if !deleted.is_empty() {
        tracing::info!(
            deleted_branches = ?deleted,
            "Garbage collected orphan branch snapshots"
        );
    }
}
```

### Task 3: Add periodic GC task

Add a background task that runs GC every hour:

```rust
// In the tokio block of run_serve:
let gc_db = db.clone();
let gc_root = project_root.clone();

tokio::spawn(async move {
    let interval = tokio::time::Duration::from_secs(3600); // 1 hour
    loop {
        tokio::time::sleep(interval).await;

        let branch_repo = SqliteBranchRepository::new(gc_db.connection().clone());
        let current = branch_repo.get_current_branch().unwrap_or_else(|_| BranchId::from("main"));
        let db_branches = branch_repo.list_branches().unwrap_or_default();

        let git_branches = get_git_branches(&gc_root);

        let mut deleted = Vec::new();
        for branch in &db_branches {
            if branch.0 == "main" || branch.0 == "master" { continue; }
            if branch == &current { continue; }
            if git_branches.contains(&branch.0) { continue; }

            if branch_repo.delete_branch(branch).is_ok() {
                deleted.push(branch.0.clone());
            }
        }

        if !deleted.is_empty() {
            tracing::info!(deleted_branches = ?deleted, "GC: deleted orphan branch snapshots");
        }
    }
});
```

### Task 4: Tests

```rust
#[test]
fn gc_deletes_orphan_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("gc-test.db");
    let db = Database::open(&db_path).expect("open");

    // Set up: main branch + feature branch in DB
    let branch_repo = SqliteBranchRepository::new(db.connection().clone());
    branch_repo.switch_branch(&BranchId::from("main")).unwrap();
    branch_repo.create_snapshot(&BranchId::from("main"), &BranchId::from("feature/old")).unwrap();

    // Create a fake git root with only main
    let git_root = tempfile::tempdir().unwrap();
    init_git_repo(git_root.path());
    // Only main exists in git

    let deleted = gc_branch_snapshots(&db, Some(git_root.path())).unwrap();
    assert_eq!(deleted, vec!["feature/old"]);

    // Verify feature/old is gone
    let branches = branch_repo.list_branches().unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0], BranchId::from("main"));
}

#[test]
fn gc_preserves_current_branch() {
    // Current branch = feature/old, even though it doesn't exist in git
    // Should NOT be deleted
}

#[test]
fn gc_preserves_main() {
    // Even if main doesn't exist in git, it should NOT be deleted
}

#[test]
fn gc_preserves_master() {
    // master is also protected
}
```

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

1. **GC deletes current branch** — protected by `if branch == &current { continue; }`
2. **GC deletes main/master** — protected by explicit name check
3. **GC while scan in progress** — GC runs on separate DB handle, no conflict (SQLite handles concurrent reads)
4. **GC on non-git project** — `get_git_branches` returns empty list, so ALL non-main/master branches are deleted. This is correct behavior — no git means no branches to preserve.
5. **GC on detached HEAD** — `get_git_branches` returns empty list (no refs/heads/*). Same as non-git — only main/master preserved.

### File List

```
crates/seshat-cli/src/db.rs                    ← ADD: gc_branch_snapshots, get_git_branches
crates/seshat-cli/src/serve.rs                 ← MODIFY: call GC on startup + periodic task
crates/seshat-cli/src/db.rs                    ← ADD: GC unit tests
```

---

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
