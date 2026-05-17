# ADR 15.1: Per-Branch Metadata (FW-5)

**Status:** Accepted
**Date:** 2026-05-18
**Tag:** `#fw5-branch-crates`
**Supersedes:** the implicit "`workspace_crates` is a single global value under `repo_metadata`" contract introduced with the D9 dynamic-workspace-crate detection (Tech Debt cleanup, 2026-05-09).
**Related:** ADR 14.1 (merge-aware decisions — the precedent for the `branches` table), Roadmap `#fw5-branch-crates`, PRD `.ralph/prd.json` on `feat/per-branch-workspace-crates`.

---

## Context

D9 (Tech Debt cleanup, 2026-05-09) replaced a hard-coded workspace-crate
list with a dynamically-detected one. The scanner now reads each project's
`Cargo.toml` / `pyproject.toml` / `package.json`, extracts the set of
internal crate/package names, and persists the result so the graph layer
can later distinguish "internal" imports (resolvable to a file in the
project) from "external" ones (third-party dep, foreign crate).

D9 wrote this set into the existing `repo_metadata` table under the key
`workspace_crates`. `repo_metadata` is a flat `(key, value)` KV table —
**project-wide**, with no branch dimension. At the time, that was
deliberate: workspace membership rarely changes within a project, and the
extra column / table felt like premature scoping.

Three things changed after D9 landed:

1. **Epic 14 (merge-aware decisions) made `branch_id` a first-class
   dimension** across the storage layer. `branches`, `branch_metadata`
   doesn't exist yet but `nodes`, `edges`, `files_ir`, `symbol_definitions`,
   `symbol_imports` are now all branch-scoped. The mental model shifted
   from "project state" to "project state at branch X".

2. **The watcher and TUI both serve queries against arbitrary `branch_id`
   values.** `query_dependencies(branch_id="feature", …)` is a routine
   call when reviewing a feature branch's diff. The graph layer's internal
   resolver reads `workspace_crates` to decide which imports are internal,
   but does so *without* the branch dimension — so a query on `feature`
   sees `main`'s workspace, and vice versa.

3. **A concrete contamination scenario emerged.** Branch `main` declares
   `[workspace] members = ["crate_a"]`; branch `feature` adds `crate_b`
   (`members = ["crate_a", "crate_b"]`). After scanning both branches:

   - `repo_metadata.workspace_crates` contains whichever branch was
     scanned last (clobbered, not merged).
   - Querying `crate_a/src/lib.rs` on `main` may resolve `use crate_b::…`
     as internal (because the last `feature` scan polluted the global
     slot), even though `crate_b` is absent from `main`'s working tree.
   - The user sees a `dependents` graph that links files that don't exist
     on the branch being inspected.

The minimum-effort fix would have been to (a) prefix the
`repo_metadata` key with the branch id (e.g. `workspace_crates:feature`)
or (b) add a `workspace_crates_json` column to the `branches` table. We
rejected both — see **Decision** below.

---

## Decision

### D1. Introduce a dedicated `branch_metadata` table, not a column on `branches`

Two designs were considered:

**Option (A) — Column on `branches`.** Add `workspace_crates TEXT` (JSON)
to `branches`. Today's only branch-scoped key would live there. Future
per-branch keys would each add another column (`pyproject_packages`,
`tsconfig_paths`, …).

**Option (B) — New `branch_metadata` KV table.** A `(branch_id, key, value,
updated_at)` table with PK `(branch_id, key)`. Mirrors the existing
project-wide `repo_metadata` table, just with a `branch_id` dimension.

We chose **(B)**.

#### Trade-off table

| Concern | (A) Column on `branches` | (B) `branch_metadata` table |
|---|---|---|
| Schema cost per new key | new migration, new column | zero migrations — new key is just another row |
| Sparse-value semantics | NULL ambiguous ("not scanned" vs "scanned, empty workspace") | row presence is the signal |
| Cascade on branch deletion | already cascaded | `ON DELETE CASCADE` FK to `branches(branch_id)` |
| Snapshot copy | one column per copy site | one `INSERT … SELECT` covers all keys |
| Repository surface | grow the `BranchRepository` trait per key | one `BranchMetadataRepository.{get,set,list,delete}` covers all keys |
| Mirrors existing pattern | no precedent | mirrors `repo_metadata` exactly |
| FK to `branches`? | implicit (column is on `branches`) | explicit (`FOREIGN KEY (branch_id) REFERENCES branches(branch_id) ON DELETE CASCADE`) |
| Read path in graph layer | typed accessor per key | one `BranchMetadataRepository.get(branch_id, key)` |

The decisive factor: this is unlikely to be the **only** per-branch key. The
roadmap already calls out `#deep-submodules` and `#submodule-inherit` work
that will need per-branch state; FW-1 (glob workspace members) and FW-3
(nested manifests) may eventually compute per-branch derived sets too. A
table makes each new key a no-migration addition; a column-per-key design
costs a migration every time.

The cost of (B) is one extra JOIN per read site, but the read sites are
infrequent (once per query in the graph layer, once per scan in the
scanner) and the table is small.

#### Why prefix-the-key in `repo_metadata` was rejected

A third option — keep `repo_metadata`, but prefix keys with the branch id
(`workspace_crates:feature`) — was rejected for three reasons:

- **No FK to `branches`.** Stale prefixed keys would survive branch
  deletion. We'd need bespoke GC code in `delete_branch`.
- **Listing all keys for a branch becomes a LIKE scan** (`WHERE key LIKE
  'foo:%'`). With the dedicated table, it's an index hit on
  `idx_branch_metadata_branch_id`.
- **The branch dimension is now structural, not optional.** Burying it in
  a string prefix loses that structure.

### D2. The FK is `ON DELETE CASCADE`

Branch deletion (`delete_branch` in `branch_repository.rs`) wipes everything
keyed by `branch_id`. The cascade keeps that path simple: no new `DELETE
FROM branch_metadata WHERE branch_id = ?` line is needed in `delete_branch`.
The SQLite-level cascade does the work, and `PRAGMA foreign_keys = ON` (set
in `Database::open` at `db.rs:64`) makes it actually fire.

### D3. `create_snapshot` copies `branch_metadata` rows

When a new branch is forked from an existing one (typically by the watcher
on detecting a `git checkout -b`), `create_snapshot` already copies
`nodes`, `edges`, `files_ir`, and the V13 symbol-index tables. Without an
extra copy, the snapshotted branch starts with an empty `branch_metadata`
and `query_dependencies` would temporarily fall back to "no internal
crates" until the next full scan refreshes the row.

The fix is a single additional `INSERT … SELECT` inside the existing
snapshot transaction:

```sql
INSERT INTO branch_metadata (branch_id, key, value, updated_at)
SELECT ?new_branch_id, key, value, updated_at
  FROM branch_metadata
 WHERE branch_id = ?source_branch_id;
```

The copy is **eventually overwritten** by the next scan on the new
branch — but until then, the snapshotted branch behaves like its parent.
This matches the precedent set by the V13 symbol-index copy in
US-007 of Epic 14.

### D4. No data migration from `repo_metadata`

Two paths were considered:

- **Backfill.** Read `repo_metadata.workspace_crates`, copy it into
  `branch_metadata` under every existing `branch_id`. Risky: that value
  reflects whichever branch was scanned last, so copying it to every
  branch carries forward the bug.
- **Wipe-and-rescan.** Leave the legacy `repo_metadata.workspace_crates`
  row alone (it's never read), require users to rescan to populate
  `branch_metadata` correctly per-branch.

We chose wipe-and-rescan. Seshat is pre-1.0; the assumption (as in ADR
14.1) is "license to break". The legacy row is left in place — it's
harmless dead data; we don't `DELETE` it to keep the migration purely
additive. A future cleanup can remove it.

### D5. No backward-compat read fallback

The graph layer's `load_internal_names` is rewired to read
`branch_metadata.workspace_crates` only. If the row is absent (fresh DB,
not-yet-scanned branch), it returns `Vec::new()` — the same fallback as
before. We do **not** fall through to `repo_metadata.workspace_crates`,
because doing so would re-introduce the contamination bug for users who
upgraded without rescanning.

The trade-off is one rescan after upgrade. In exchange we get a clean
read path with no per-branch override logic.

---

## Consequences

### Positive

- **Cross-branch contamination of internal-name resolution is fixed.**
  `query_dependencies` on `main` no longer sees `feature`'s workspace
  membership, and vice versa. Locked behind a regression test
  (`crates/seshat-cli/tests/cross_branch_workspace_crates.rs`).
- **One new key, zero new migrations from here on.** Future per-branch
  state (e.g. per-branch `tsconfig` path-aliases, per-branch detected
  test framework) lands as additional rows in `branch_metadata`, not
  schema changes.
- **Branch deletion is still a single `DELETE FROM branches WHERE
  branch_id = ?`.** The cascade handles `branch_metadata` for free.
- **Snapshot copies mirror the existing pattern** in
  `create_snapshot` — one more `INSERT … SELECT` inside the same
  transaction.
- **Repository surface is symmetrical with `RepoMetadataRepository`.**
  Callers familiar with the project-wide KV will recognise the shape;
  the only difference is the extra `branch_id: &str` parameter.

### Negative / Trade-offs

- **One-time rescan required on upgrade** to populate
  `branch_metadata.workspace_crates`. Documented in CHANGELOG. Users who
  don't rescan will see all imports resolve as external (today's
  empty-list fallback path) until they do — no broken behaviour, just
  reduced internal-name resolution until the next scan.
- **Legacy `repo_metadata.workspace_crates` row is now dead data.** Left
  in place to keep the migration additive. A future cleanup migration
  can `DELETE` it; the current migration intentionally does not, so
  rollback (if ever needed pre-1.0) is trivially "redeploy the old
  binary".
- **One extra read on every `query_dependencies` call.** Measured as
  negligible (a single indexed lookup on a small table). Eliminating
  the duplicate JSON parse on hot paths is a future optimisation, not
  blocked by this schema choice.

### Neutral

- **`repo_metadata` continues to exist** for genuinely project-wide
  keys (e.g. `current_branch`). The two tables are siblings, not
  competitors.
- **The `branch_metadata.updated_at` column is informational.** Today
  no caller reads it; it's set on every UPSERT (`unixepoch()` on
  INSERT, copied from `excluded` on conflict) so future debugging /
  cache-invalidation logic has a timestamp to reason about.

---

## Implementation Notes

The work landed across seven user stories on `feat/per-branch-workspace-crates`,
each commit CI-green standalone:

| Story | Title | Key files |
|---|---|---|
| US-001 | V14 migration creates `branch_metadata` table | `crates/seshat-storage/migrations/V14__branch_metadata.sql`, `crates/seshat-storage/src/db.rs::tests` |
| US-002 | `BranchMetadataRepository` trait + Sqlite impl | `crates/seshat-storage/src/repository/branch_metadata_repository.rs`, `repository/mod.rs`, `lib.rs` |
| US-003 | Orchestrator writes `workspace_crates` per-branch | `crates/seshat-scanner/src/orchestrator.rs` |
| US-004 | Graph reads `workspace_crates` per-branch | `crates/seshat-graph/src/dependencies.rs` (`load_internal_names`) |
| US-005 | `create_snapshot` copies `branch_metadata` rows | `crates/seshat-storage/src/repository/branch_repository.rs` |
| US-006 | Cross-branch regression integration test | `crates/seshat-cli/tests/cross_branch_workspace_crates.rs` |
| US-007 | Documentation, ADR, CHANGELOG entry | this file, `CHANGELOG.md`, `roadmap.md` |

Highlights:

- V14 migration: `crates/seshat-storage/migrations/V14__branch_metadata.sql`.
  Auto-discovered by `refinery::embed_migrations!("migrations")` in
  `crates/seshat-storage/src/db.rs`.
- New repository: `crates/seshat-storage/src/repository/branch_metadata_repository.rs`
  with `BranchMetadataRepository` trait (defined in `repository/mod.rs` to
  match the convention used by every other `*Repository` trait in the
  crate) and `SqliteBranchMetadataRepository` impl.
- Write site cut over: `crates/seshat-scanner/src/orchestrator.rs:500` —
  `meta_repo.set("workspace_crates", …)` → `branch_meta.set(&branch.0,
  "workspace_crates", &json)`.
- Read site cut over: `crates/seshat-graph/src/dependencies.rs`
  (`load_internal_names`) — `branch_id` parameter is no longer ignored;
  the function reads via `SqliteBranchMetadataRepository.get(branch_id,
  "workspace_crates")` with `Vec::new()` fallback.
- Snapshot copy: `crates/seshat-storage/src/repository/branch_repository.rs`
  (`create_snapshot`) — one extra `INSERT … SELECT` inside the existing
  transaction.
- Regression test: `crates/seshat-cli/tests/cross_branch_workspace_crates.rs`
  drives the full real-git + scan + `query_dependencies` flow on a
  two-branch fixture in ~0.1 s.

The PRD (`.ralph/prd.json` on `feat/per-branch-workspace-crates`) is the
canonical task list. Per-story implementation notes — including the
"learnings" that fed back into Codebase Patterns in
`.ralph/progress.txt` — live in the `notes` field of each PRD story.

---

## References

- PRD: `.ralph/prd.json` on `feat/per-branch-workspace-crates`
- Progress log: `.ralph/progress.txt`
- CHANGELOG: top-level `CHANGELOG.md`, `[Unreleased]` section
- Roadmap: `_bmad-output/planning-artifacts/roadmap.md` (`#fw5-branch-crates`)
- Related ADR: `_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md`
  (precedent for branch-scoped storage; D1 of that ADR explains the
  `decisions` table choice the same way D1 here explains
  `branch_metadata`)
