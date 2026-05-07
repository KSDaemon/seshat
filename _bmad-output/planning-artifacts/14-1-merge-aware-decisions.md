# ADR 14.1: Merge-aware Decisions and DB Freshness

**Status:** Accepted
**Date:** 2026-05-07
**Epic:** 14 — Merge-aware Decisions
**Supersedes:** the implicit "decisions live in `nodes` with `ext_data.source = 'user'`" contract established before V11/V12.
**Related:** ADR-14 (branch switch flow), ADR-23 (branch snapshots), Story 11.1 (branch detection + worktree), Story 11.2 (branch switch orchestration), PRD `.ralph/tasks/prd-merge-aware-decisions.md`.

---

## Context

Five interrelated bugs surfaced during the snippet-quality merge cycle:

1. **Cross-branch decisions don't propagate.** Approving a convention on
   `feature` and merging into `main` re-emits the same convention in
   `seshat review` on `main`, because user-decision state was keyed by
   `branch_id`.
2. **Same-branch HEAD-moved is invisible.** `seshat serve` only kicked
   `background_sync` when the BRANCH LABEL changed. A `git pull` on `main`
   (label unchanged, HEAD moved) left the DB stale until manual rescan.
3. **`seshat review` had zero freshness check.** It opened the DB and showed
   whatever was in it, regardless of git state.
4. **`create_snapshot` stripped later-added columns** (`description_hash`,
   `ir_schema_version`, `last_commit_date`) when copying a branch's data,
   silently dropping evidence-quality and dedup metadata on every branch
   switch.
5. **V8 had no backfill** for pre-V8 user nodes — their `description_hash`
   stayed NULL and dedup never matched.

The minimum-effort fix would have been to add a `decisions_branchless` column
or to backfill `description_hash` retroactively. We rejected those for the
reasons in **Decision** below.

This ADR documents the design choices made by the work that landed under
US-001..US-018 on `feat/merge-aware-decisions`.

---

## Decision

### D1. Move user decisions from `nodes` to a dedicated `decisions` table

A new V12 migration creates the `decisions` table keyed by
`description_hash`. All user-recorded decisions (TUI confirm/reject/partial
AND MCP `record_decision`/`update_decision`) are stored here. Auto-detected
nodes stay in `nodes`. The review query becomes a `LEFT JOIN decisions ON
decisions.description_hash = nodes.description_hash WHERE
decisions.description_hash IS NULL`.

#### Decision-table-vs-user-node trade-off

The earlier design overloaded `nodes` with both auto-detected and
user-recorded rows, distinguished by `ext_data.source`. That made every
review query do a JSON predicate against `ext_data->>'source'` and made
cross-branch propagation impossible without a project-wide `nodes` view that
violated the `branch_id` foreign key.

A dedicated `decisions` table buys five things at once:

| Concern | `nodes + ext_data` | `decisions` table |
|---|---|---|
| Cross-branch propagation | impossible without breaking `branch_id` FK | trivial — no `branch_id` |
| Review query | JSON predicate on `ext_data` | indexed JOIN on `description_hash` |
| Survives branch deletion | requires explicit "user" copy step in `delete_branch` | rows live independently of branches |
| Survives `create_snapshot` | requires copying every "user" node and renaming `branch_id` | rows are project-wide; snapshot copies nothing |
| Bug 4 (column-drop on snapshot) | risk re-emerges every time a column is added | doesn't apply — snapshot doesn't copy decisions |
| Bug 5 (V8 backfill) | needs a one-shot backfill migration | doesn't apply — fresh table starts populated |

The cost is a small dedup query (`bulk-fetch decisions for hashes IN (...)`)
in `persist_conventions`. Empirically negligible on projects with ≤ 1000
decisions; the bulk lookup chunks at 500 hashes per query to stay under
SQLite's `SQLITE_MAX_VARIABLE_NUMBER`.

#### Why no migration

Seshat is pre-1.0 and not yet in production use. A schema migration that
preserved existing user data would have required:

- a backfill that recomputes `description_hash` for every pre-V8 user node,
- a copy step that moves `nodes WHERE ext_data->>'source' = 'user'` into
  `decisions`,
- a delete step that removes the now-duplicate user rows from `nodes`,
- careful handling of the `(branch_id, description)` ↔ `description_hash`
  mapping when the same description was approved on multiple branches.

Even getting that right, the schema redesign also fixed bugs 4 and 5
silently — meaning the migration would carry forward known-broken rows. The
trade-off was: ship a clean, correct schema with a one-time DB wipe and an
explicit CHANGELOG entry, OR ship a fragile migration that preserves bad
data for the sake of "no work for the user". We chose the wipe.

The CHANGELOG and the new top-level README both document the wipe path
(`rm ~/.local/share/seshat/repos/<project>.db && seshat scan`) so users
have a single, mechanical recovery action.

### D2. Detect git state changes on every CLI startup

Two detection points:

- **`seshat serve`** — at startup, after resolving the branch, compare
  `branches.last_scanned_commit` for that branch against
  `git rev-parse HEAD`. If different OR if the branch label changed,
  spawn `background_sync` (non-blocking). Logs include
  `old_head=<7-char>, new_head=<7-char>` so the trigger is visible.
- **`seshat review`** — at startup, do the same check, but if stale, run
  the sync **synchronously** before opening the TUI. Emits a
  `Syncing project state to <head[..7]>...` header and a
  `Files: X / Y` progress line at ≥ 1 Hz.

Both points are gated by a single freshness helper
(`check_branch_freshness`) returning a 3-variant enum
(`UpToDate | Stale { old, new } | GitUnavailable`). The two CLI entry
points consume the same enum; the only difference is whether they
`thread::spawn` the sync or call it inline.

The shared sync body is exposed as `incremental_sync_blocking` (extracted
from the previous `background_sync` body) with an
`Option<&dyn Fn(usize, usize)>` progress callback so both paths share
exactly one implementation.

### D3. Git-optional fallback semantics

Seshat must continue to work in non-git directories (downloads, tmp dirs,
git-less projects). The fallback is documented in §G6 of the PRD and
implemented as:

- **`detect_branch` returns `"main"`** when no `.git` is found. This is
  the synthetic-branch identity. All decisions are decided on
  `branch="main"`; all queries are scoped to `branch="main"`. From the
  user's perspective the project behaves like a single-branch repo.
- **Freshness comparisons are no-ops.** `git rev-parse HEAD` is read via
  `gix`; on failure (no `.git`, corrupt repo, refs missing) the helper
  returns `GitUnavailable` and the freshness gate skips the sync silently
  with a `debug!` log. The `last_scanned_commit` column stays `NULL`.
- **No warnings, no errors.** A non-git directory must not pollute
  stdout/stderr on serve startup or review startup. Any visible message
  in this path is treated as a regression.
- **Scan paths set `last_scanned_commit = NULL`** when git is
  unavailable. We do NOT invent a synthetic hash; the column's
  `NULL`-ness is the signal that "this scan was done without git".

The two log levels are intentional and distinct:

| Event | Log level | Meaning |
|---|---|---|
| Sentinel write failed in storage | `warn!` | Unexpected — investigate |
| Git unavailable, sentinel skipped | `debug!` | Expected — invisible by default |

### D4. Worktree concurrency limitation (known, documented)

`metadata.current_branch` is a single global value. Two concurrent
`seshat serve` instances on different worktrees of the same main repo will
race on this value. We considered:

- **(a)** scoping `current_branch` per `(repo_root, worktree_path)` —
  rejected because it spreads worktree awareness through every read site
  of `current_branch`,
- **(b)** removing `current_branch` entirely and re-detecting on every
  query — rejected because of the cost of the `gix::discover` per query,
- **(c)** documenting the limitation and deferring — accepted.

The practical impact: in a multi-worktree setup, the "current branch"
visible to MCP `query_*` tools is the branch of whichever serve instance
last wrote to `metadata.current_branch`. The DECISIONS themselves are
project-wide and unaffected. Per-branch SCANS are unaffected — each
worktree scans its own branch and stores nodes under its own `branch_id`.
Only the "what branch is the server currently looking at?" pointer
flickers.

This is acceptable because the typical multi-worktree user is doing two
distinct workflows (e.g. main repo + hotfix worktree) and the per-worktree
serve instance is the source of truth for that worktree's MCP traffic.

### D5. Future extensions (not in scope)

The current schema deliberately leaves room for two future iterations:

- **Per-branch decision overrides.** The `decisions.decided_on_branch`
  column is currently audit-only — lookup is always project-wide. A
  future migration could add a `scope` column (`"global"` vs
  `"branch:<name>"`) and a query-time `WHERE scope IN ('global',
  'branch:'||?)` predicate. The current schema does not block that
  evolution: `decided_on_branch` can be repurposed without a destructive
  migration.
- **Conflict resolution UI.** `seshat decisions import` resolves conflicts
  silently by `decided_at` (latest wins) or aborts under `--strict`. A
  future iteration could expose a per-row "keep mine / take theirs"
  picker in the TUI. The import seam already returns a structured
  `ImportSummary { total, inserted, updated, skipped }` so the UI has a
  natural set of counters to display.

Both extensions are out of scope for this epic. The point of this section
is to make the deferral explicit so a future contributor reading the
schema doesn't assume the omissions are oversights.

---

## Consequences

### Positive

- One source of truth for user-recorded decisions across the TUI, MCP
  tools, and the review query.
- Cross-branch propagation works by default — the merge scenario in
  US-016 passes regression-locked.
- `create_snapshot` no longer needs to copy decisions; bug 4 (column-drop
  on snapshot) cannot recur for decision rows.
- No retroactive `description_hash` backfill needed; bug 5 dissolves.
- The MCP envelope shape is preserved for `query_*` tools (purely
  additive: a `decisions` count appears in `query_project_context`); only
  the mutation tools (`record_decision`/`update_decision`/
  `remove_decision`) take a hash-keyed identifier instead of an integer
  rowid.
- Freshness detection covers both branch-label change and same-branch
  HEAD movement, closing a long-standing class of "DB is stale because I
  pulled" surprises.
- Non-git directories continue to work with zero output changes.

### Negative / Trade-offs

- **One-time DB wipe** for every existing seshat install. Documented in
  CHANGELOG and the new top-level README. Mechanical recovery: delete
  the per-project `.db` file under `~/.local/share/seshat/repos/` and
  rerun `seshat scan`. Pre-1.0 license to break is the underlying
  assumption — not extensible to post-1.0 schema changes.
- **MCP mutation tools take a `description_hash` instead of a rowid.**
  Any existing scripts that captured a numeric `id` from
  `record_decision` and threaded it back into `update_decision` /
  `remove_decision` need to switch to passing the hash. Seshat is
  pre-1.0, so no compatibility shim was added.
- **Worktree concurrency limitation** documented in D4 above.
- **Detached HEAD optimisation deferred.** Each unique commit hash
  becomes a `branch_id` in detached-HEAD mode. This may pollute the
  `branches` table on workflows that frequently `git checkout <sha>` to
  inspect history. Cleanup is an existing GC concern (Story 11.2 branch
  GC).

### Neutral

- The dedup path in `persist_conventions` issues exactly one extra SELECT
  against `decisions` per scan (chunked at 500 hashes/query). Performance
  cost: negligible on the projects measured during implementation.
- The `nodes.description_hash` column is preserved (no rename, no drop)
  so the LEFT JOIN against `decisions` works without a schema-rewrite
  step.

---

## Implementation Notes

The work landed across 19 user stories in strict order (US-001 → US-019).
Each commit is CI-green standalone. Implementation order is documented in
the PRD §"Implementation Order" and in the sequence of git commits on
`feat/merge-aware-decisions`.

Highlights:

- V11 + V12 migrations: `crates/seshat-storage/migrations/V11__branches_table.sql`,
  `V12__decisions_table.sql`. Auto-discovered by
  `refinery::embed_migrations!("migrations")` in
  `crates/seshat-storage/src/db.rs`.
- New repository: `crates/seshat-storage/src/repository/decision_repository.rs`
  with `DecisionRepository` trait and `SqliteDecisionRepository` impl.
- Freshness helper: `check_branch_freshness` in `crates/seshat-cli/src/db.rs`.
- Sync extraction: `incremental_sync_blocking` in
  `crates/seshat-cli/src/serve.rs` (formerly the body of `background_sync`).
- New CLI surface: `seshat decisions list|forget|export|import` in
  `crates/seshat-cli/src/decisions.rs` and `args.rs`.
- Smoke-test plan: `docs/smoke-tests/merge-aware-decisions.md`.

The PRD's failure-mode checklist (§"Failure-mode checklist") is the
canonical list of invariants every implementer verified before marking a
story done.

---

## References

- PRD: `.ralph/tasks/prd-merge-aware-decisions.md`
- CHANGELOG: top-level `CHANGELOG.md`, entry under `[0.2.0]`
- Smoke tests: `docs/smoke-tests/merge-aware-decisions.md`
- Related ADRs: `_bmad-output/planning-artifacts/story-11-1-branch-detection-worktree.md`,
  `_bmad-output/implementation-artifacts/11-2-branch-switch-adr14.md`
