---
date: 2026-05-17
type: prd
size: medium (~0.5-1 day)
scope: Move workspace_crates from global repo_metadata to per-branch storage
roadmap_tag: "#fw5-branch-crates"
languages: language-agnostic (Rust + Python + JS/TS all benefit)
depends_on: V11 branches table (Epic 14, already landed)
---

# PRD: Per-Branch `workspace_crates` Storage (FW-5)

**Author:** Kostik (drafted by Claude)
**Date:** 2026-05-17
**Status:** Ready for implementation
**Type:** Schema migration + plumbing (no new MCP surface)

---

## Part I: Problem Statement

### Current State

`workspace_crates` — the list of internal crate/package names that drives
`is_likely_internal` and `resolve_internal_crate_import` in
`crates/seshat-graph/src/dependencies.rs` — is currently persisted to the
**global** `repo_metadata` table:

- **Write side** (`crates/seshat-scanner/src/orchestrator.rs:501`):
  ```rust
  meta_repo.set("workspace_crates", &json)
  ```
- **Read side** (`crates/seshat-graph/src/dependencies.rs:541`):
  ```rust
  pub fn load_internal_names(conn: &Arc<Mutex<Connection>>, branch_id: &str) -> Vec<String> {
      // branch_id is accepted but IGNORED — read goes to global repo_metadata
      ...
      match repo.get("workspace_crates") { ... }
  }
  ```

`repo_metadata` is a flat `(key, value)` store with no branch dimension.
There is exactly **one** `workspace_crates` row per project, regardless of
how many branches the project has.

### What Breaks

Scenario: a developer working on `feat/split-graph` splits the `seshat-graph`
crate into two: `seshat-graph-core` and `seshat-graph-query`. On
`feat/split-graph`, `workspace_crates` should be:

```json
["seshat_core", "seshat_scanner", "seshat_graph_core", "seshat_graph_query",
 "seshat_mcp", "seshat_cli", ...]
```

On `main`, it should still be:

```json
["seshat_core", "seshat_scanner", "seshat_graph", "seshat_mcp", "seshat_cli", ...]
```

But because storage is global, whichever branch was scanned **last** wins.
Both branches read the same list. This causes:

| Direction | Failure mode |
|---|---|
| Last scan was on `feat/split-graph`, now on `main` | `is_likely_internal("seshat_graph_core::foo")` → `true` on `main`, parser tries to resolve a path that does not exist → silent miss, `dependents[]` understates the graph |
| Last scan was on `main`, now on `feat/split-graph` | `is_likely_internal("seshat_graph_core::foo")` → `false` on `feat/split-graph`, internal imports drop to external → blast_radius and `query_dependencies` results are wrong |
| Worktree of the same repo on a different branch | Same as above — racy and order-dependent |

### Impact Beyond Rust

The same `workspace_crates` row is **shared** by Rust, Python (FW-2/FW-4),
and the upcoming JS/TS monorepo detection (`spec-jsts-monorepo-detection.md`).
Per-branch correctness is therefore **not** a Rust-specific concern —
any project type where the workspace structure can differ between branches
hits the same race.

### Why Now

1. **Epic 14 already landed the `branches` table** (V11 migration,
   `crates/seshat-storage/migrations/V11__branches.sql`). The branch-scoped
   storage substrate exists; we are not introducing the concept.
2. **JS/TS monorepo detection is about to land** (separate spec). Once it
   does, the race surface roughly doubles — every npm workspace user gets
   exposed.
3. **Worktree support is in production.** Multiple `seshat serve` instances
   on different worktrees of the same main repo (Epic 14 known limitation
   D4) compete on the global slot.

### Root Cause

`load_internal_names(conn, branch_id)` was given a `branch_id` parameter
during the Epic-14 refactor as forward-looking surface area, but the
storage layer was not updated to use it. This PRD closes that loop.

---

## Part II: Design

### Storage Decision

Two viable schemas. **Recommend (B)** for extensibility; **(A)** is
acceptable as a smaller change if extensibility is not desired.

#### (A) Column on `branches` table — smaller diff

V14 migration:

```sql
ALTER TABLE branches ADD COLUMN workspace_crates TEXT;  -- JSON array
```

- ✅ Single new column, ~3 lines of SQL.
- ✅ Naturally tied to branch lifecycle (CASCADE on branch delete is the
  existing branches-table behaviour).
- ❌ Adds a domain-specific column to a generic metadata table. Next
  per-branch field (e.g. per-branch detector trends) needs another `ALTER`.

#### (B) New `branch_metadata` table — recommended

V14 migration:

```sql
CREATE TABLE IF NOT EXISTS branch_metadata (
    branch_id  TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (branch_id, key),
    FOREIGN KEY (branch_id) REFERENCES branches(branch_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_branch_metadata_branch_id ON branch_metadata(branch_id);
```

- ✅ Mirrors `repo_metadata` exactly, with a branch dimension. Future
  per-branch keys (e.g. `manifest_hash`, `last_workspace_scan_at`) reuse
  the same table.
- ✅ FK + ON DELETE CASCADE inherits the GC story already wired for
  `branches` (Story 11.2 — branches deleted by GC carry their metadata
  rows with them).
- ✅ Symmetric API: `BranchMetadataRepository` mirrors
  `RepoMetadataRepository`. The graph layer's `load_internal_names`
  rewrite is a one-line site swap.
- ❌ Slightly more migration code than (A).

### Pre-1.0 — No Data Migration

Per the project's "no backward compatibility" rule (CLAUDE.md memory),
this is a **destructive** schema change:

- V14 creates the new table empty.
- Existing `repo_metadata.workspace_crates` row is **left in place** but
  becomes ignored by the reader after this PRD.
- A subsequent V15 migration deletes the orphan row to keep the schema
  tidy (separate, two-line cleanup migration — optional, can ship later).
- First scan on each branch repopulates the per-branch slot. Until then,
  `load_internal_names` returns `[]` (correct empty fallback — all imports
  classified as external, identical to the "no manifest" case).

No user-visible action required beyond rescan-on-first-use, which
`seshat serve` does automatically via auto-scan.

### Code Changes

| File | Change |
|---|---|
| `crates/seshat-storage/migrations/V14__branch_metadata.sql` (new) | Schema (see (B) above) |
| `crates/seshat-storage/src/repository/branch_metadata_repository.rs` (new) | `BranchMetadataRepository` trait with `get(branch_id, key) -> Option<String>`, `set(branch_id, key, value)`, `list(branch_id) -> Vec<(String, String)>`, `delete(branch_id, key)`. Sqlite impl identical in shape to `repo_metadata_repository.rs`. |
| `crates/seshat-storage/src/repository/mod.rs` | Module declaration + re-export |
| `crates/seshat-scanner/src/orchestrator.rs:501` | Replace `meta_repo.set("workspace_crates", &json)` with `branch_meta.set(branch_id, "workspace_crates", &json)`. `branch_id` is available in scope (passed through `OrchestratorContext`). |
| `crates/seshat-graph/src/dependencies.rs:541` | Replace global read with `branch_meta.get(branch_id, "workspace_crates")`. `branch_id` is already a parameter — just stop ignoring it. |
| `crates/seshat-watcher/src/hot_tier.rs` | Incremental re-scan path also writes to the new slot (verify the orchestrator entry point is shared so no extra change needed; otherwise mirror the change here). |
| `crates/seshat-storage/src/repository/branch_repository.rs::create_snapshot` | When forking a branch (e.g. on first commit on a new branch with worktree support), **copy** the source branch's `branch_metadata` rows to the new `branch_id`. This is the same pattern as `nodes`/`edges`/`files_ir` copy in `create_snapshot`. |

### `create_snapshot` Behaviour (Important)

Today `create_snapshot` copies `nodes`, `edges`, and `files_ir` rows for a
new `branch_id`. The new branch inherits the source branch's view. After
this PRD, it must **also** copy `branch_metadata` rows so the new branch
inherits the parent's `workspace_crates` until the first scan refreshes
them. Otherwise queries on a freshly-snapshotted branch return `[]` until
the watcher's warm tier triggers — a regression in observable behaviour.

Test required: `create_snapshot_copies_branch_metadata`.

### Migration Test

Existing fixture `crates/seshat-storage/tests/migrations.rs` (verify path)
must include:
- V14 applies cleanly on a DB at V13.
- V14 is idempotent (re-apply is a no-op).
- After V14, `branch_metadata` is queryable and `repo_metadata` is unchanged.

---

## Part III: User Stories

### US-001: V14 migration creates `branch_metadata` table

**Description:** Storage maintainer needs the new table and indices in
place so subsequent stories can land their plumbing.

**Acceptance Criteria:**

- [ ] New file `crates/seshat-storage/migrations/V14__branch_metadata.sql`
  matches the schema in Part II (B).
- [ ] Migration applies cleanly on (a) empty DB, (b) existing DB at V13.
- [ ] FK to `branches(branch_id)` with `ON DELETE CASCADE` validated by a
  test: insert a `branches` row, insert a `branch_metadata` row referencing
  it, delete the branch, assert the metadata row is gone.
- [ ] Idempotency: applying V14 twice does not error (the `IF NOT EXISTS`
  clause is in place).

### US-002: `BranchMetadataRepository` trait + Sqlite impl

**Description:** Scanner and graph crates need a typed API to read/write
per-branch metadata so they don't sprinkle raw SQL.

**Acceptance Criteria:**

- [ ] New trait `BranchMetadataRepository` in
  `crates/seshat-storage/src/repository/branch_metadata_repository.rs`.
- [ ] Methods: `get(&self, branch_id: &str, key: &str) -> Result<Option<String>, _>`,
  `set(&self, branch_id: &str, key: &str, value: &str) -> Result<(), _>`,
  `list(&self, branch_id: &str) -> Result<Vec<(String, String)>, _>`,
  `delete(&self, branch_id: &str, key: &str) -> Result<(), _>`.
- [ ] `SqliteBranchMetadataRepository` implements the trait; UPSERT
  semantics for `set` (`INSERT ... ON CONFLICT(branch_id, key) DO UPDATE`).
- [ ] Unit tests mirror `repo_metadata_repository.rs::tests` — get/set
  round-trip, overwrite, list, delete, isolation between two branch_ids.
- [ ] `cargo test -p seshat-storage` passes.

### US-003: Orchestrator writes `workspace_crates` per-branch

**Description:** Move the persist site in
`crates/seshat-scanner/src/orchestrator.rs:501` to the new repository.

**Acceptance Criteria:**

- [ ] After a scan on `branch_id = "X"`, `branch_metadata` contains exactly
  one row `(X, "workspace_crates", <json>)`. The old `repo_metadata` write
  is removed.
- [ ] Re-scanning on the same `branch_id` overwrites the value (UPSERT).
- [ ] Re-scanning on a different `branch_id` adds a separate row — both
  rows coexist; neither overwrites the other.
- [ ] Existing test `scan_persists_workspace_crates_with_local_packages_union`
  (`orchestrator.rs:1273`) is updated to assert the per-branch slot.
- [ ] New test: `scan_two_branches_isolates_workspace_crates` — runs the
  scan against two different `branch_id`s with different fixture
  manifests, asserts no cross-contamination.

### US-004: Graph reads `workspace_crates` per-branch

**Description:** Make `load_internal_names(conn, branch_id)` actually use
the `branch_id` parameter.

**Acceptance Criteria:**

- [ ] `load_internal_names` reads from `branch_metadata` keyed by
  `branch_id` and `"workspace_crates"`.
- [ ] Falls back to `Vec::new()` if the row is absent (same correctness as
  today's empty-list fallback).
- [ ] All existing tests in `dependencies.rs::tests` (`:1894+`) that seed
  `workspace_crates` via `seed_workspace_crates_in_repo_metadata` are
  updated to seed `branch_metadata` instead. The helper function is
  renamed accordingly.
- [ ] New test: `query_dependencies_uses_per_branch_workspace_crates` —
  seed two branches, run `query_dependencies` against each, assert
  divergent internal-name resolution.

### US-005: `create_snapshot` copies `branch_metadata` rows

**Description:** When a new branch is forked from an existing one, its
metadata must follow until the first scan refreshes it.

**Acceptance Criteria:**

- [ ] `BranchRepository::create_snapshot` SQL extended to include a
  `INSERT INTO branch_metadata (branch_id, key, value, updated_at)
   SELECT ?new_branch_id, key, value, updated_at FROM branch_metadata
   WHERE branch_id = ?source_branch_id` step.
- [ ] New test `create_snapshot_copies_branch_metadata` in
  `branch_repository.rs::tests` parallel to `create_snapshot_copies_nodes_and_files`
  (`:311`).
- [ ] All snapshot operations remain transactional.

### US-006: Cross-branch regression suite

**Description:** Lock the contract that switching branches no longer
contaminates internal-name resolution.

**Acceptance Criteria:**

- [ ] New integration test
  `crates/seshat-cli/tests/cross_branch_workspace_crates.rs` (or extend
  the existing `cross_branch_decisions.rs` pattern from Epic 14):
  - Set up a fixture git repo with two branches `main` and `feature`.
  - On `main`, `Cargo.toml` declares `members = ["crate_a"]`.
  - On `feature`, `Cargo.toml` declares `members = ["crate_a", "crate_b"]`.
  - Scan each branch. Switch between them. Assert that
    `query_dependencies` against `crate_a/src/lib.rs` sees the right set
    of internal names on each branch.
- [ ] Test runs in <10s (no embedding generation; mock or `--no-embeddings`).

### US-007: Documentation + ADR

**Description:** Record the schema decision so future contributors don't
revisit (A) vs (B).

**Acceptance Criteria:**

- [ ] Append a section "Per-branch metadata" to the existing ADR 14.1
  (`_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md`)
  documenting the (B) decision and why `branch_metadata` is preferred
  over an ALTER on `branches`. OR a new ADR
  `_bmad-output/planning-artifacts/15-1-branch-metadata.md` if Kostik
  prefers separation.
- [ ] `CHANGELOG.md` `[Unreleased]` entry under `### Breaking` (DB schema
  change) and `### Added` (per-branch isolation).
- [ ] `roadmap.md` `#fw5-branch-crates` tag marked `✅ IMPLEMENTED`.

---

## Part IV: Verification Plan

### Unit

- `cargo test -p seshat-storage` — V14 migration, repository CRUD,
  branch-snapshot metadata copy
- `cargo test -p seshat-scanner` — per-branch persist
- `cargo test -p seshat-graph` — per-branch read

### Integration

- `crates/seshat-cli/tests/cross_branch_workspace_crates.rs`
- Existing `cross_branch_decisions.rs` should still pass (decisions are
  project-wide; this PRD doesn't touch them — guard against regression).

### Smoke

- Clone any non-trivial Rust workspace, scan on two branches with
  different crate sets, `seshat status` shows correct per-branch counts,
  `query_dependencies` returns branch-appropriate results.

### Lints

- `cargo clippy --workspace -- -D warnings`
- `cargo fmt --check`
- `cargo doc --no-deps`

---

## Part V: Non-Goals

- **Migrating the existing `repo_metadata.workspace_crates` row** into
  the new table. Project pre-1.0; no data migration. Existing row is
  ignored after this PRD and may be deleted in a follow-up V15.
- **Other `repo_metadata` keys.** Only `workspace_crates` moves.
  `manifest_hashes`, `last_scanned_at`, etc. stay where they are.
- **Per-submodule `branch_metadata`.** Submodules each have their own DB
  with their own `branches` table — already isolated. No cross-scope
  concern here.
- **Backporting to older DBs without rescan.** First scan after the
  migration repopulates per-branch slots; until then, the fallback to
  empty list is correct.

---

## Part VI: Risks

- **Risk:** `create_snapshot` semantics change observably — a fresh
  branch fork now sees workspace_crates immediately instead of `[]` until
  next scan. **Mitigation:** this is a strict improvement; document in
  CHANGELOG `### Fixed`.
- **Risk:** Tests that mock `repo_metadata` for `workspace_crates`
  silently keep passing because the helper still seeds the global slot.
  **Mitigation:** rename the helper and grep all call sites; the rename
  forces every test to be touched.
- **Risk:** `BranchMetadataRepository` ergonomics tempt callers to use
  it for everything per-branch, including hot-path reads. **Mitigation:**
  cache the deserialized `Vec<String>` in `query_dependencies` (already
  the pattern today — one read per query call, not per import).

---

## Part VII: Estimated Effort

| Phase | Effort |
|---|---|
| V14 migration + repository skeleton + unit tests | 1.5h |
| Orchestrator + watcher persist sites | 0.5h |
| Graph read site + dependencies tests rewrite | 1.5h |
| `create_snapshot` copy + tests | 1h |
| Cross-branch integration suite | 1.5h |
| Docs / ADR / CHANGELOG | 0.5h |
| Lints + smoke + buffer | 1h |
| **Total** | **~0.5–1 day** |

---

## Part VIII: Open Questions

1. **(B) `branch_metadata` table or (A) column on `branches`?** Recommend
   (B) for extensibility. Kostik to confirm before US-001 starts.
2. **V15 cleanup of orphan global row** — ship in this PRD or as a
   follow-up housekeeping commit? Recommend follow-up to keep this PRD
   focused; the orphan row costs <100 bytes per DB and is harmless.
3. **Embed `branch_metadata.workspace_crates` JSON in `seshat status`
   verbose output?** Useful for debugging cross-branch issues but expands
   the status output. Out of scope for FW-5; revisit if helpful in
   practice.
