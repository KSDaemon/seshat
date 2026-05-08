# PRD: Merge-aware Decisions and DB Freshness

## Introduction

**Type:** Feature

Replace branch-scoped tracking of user decisions with a project-wide `decisions` table indexed by description hash. Make `seshat serve` and `seshat review` detect git state changes (branch label OR HEAD movement) on startup and trigger incremental sync. Add `seshat decisions` CLI subcommand. Continue working without git via a synthetic single-branch fallback.

**Why:** Five interrelated bugs surfaced during the snippet-quality merge cycle:

1. **Cross-branch decisions don't propagate.** Approving "Canonical logging library: tracing" on `feature` and merging into `main` makes the same convention re-appear in `seshat review` on `main` because user-decision state is keyed by `branch_id`.
2. **Same-branch HEAD-moved is invisible.** `seshat serve` only kicks `background_sync` when the BRANCH LABEL changes. A `git pull` on `main` (label unchanged, HEAD moved) leaves the DB stale until manual re-scan.
3. **`seshat review` has zero freshness check.** Opens DB and shows whatever's in it, regardless of git state.
4. **`create_snapshot` strips later columns** (`description_hash`, `ir_schema_version`, `last_commit_date`) when copying a branch's data.
5. **V8 has no backfill** for pre-V8 user nodes — their `description_hash` stays NULL and dedup never matches.

Bugs 4 and 5 disappear automatically when decisions move out of `nodes` into a dedicated table. **No data migration is performed:** the DB is wiped on first launch after this lands.

## Goals

- **G1.** A convention approved or rejected on ANY branch is treated as decided across ALL branches by default.
- **G2.** `seshat serve` startup detects both branch-label change AND same-branch HEAD movement, kicks background sync in either case.
- **G3.** `seshat review` startup performs a blocking incremental sync with progress display before opening the TUI.
- **G4.** Schema redesign requires no data migration: existing DBs are wiped, fresh DB picks up new schema cleanly.
- **G5.** Decisions are durable across branch deletion: a convention approved on `feature` survives `git branch -D feature`.
- **G6.** Seshat continues to operate against non-git project directories: a synthetic single-branch identity is used, freshness checks become no-ops, no errors are raised.
- **G7.** All MCP decision tools (`record_decision`, `update_decision`, `remove_decision`, `query_convention`) and the TUI confirm/reject/partial flow share one storage backend — no parallel mechanisms.

## User Stories

### US-001: V11 + V12 migrations and repo skeletons

**Description:** As a developer, I need new migration files in place before any consumer can be migrated, so that downstream stories have schema to compile against.

**Acceptance Criteria:**
- [ ] `crates/seshat-storage/migrations/V11__branches_table.sql` creates the `branches` table with `branch_id PRIMARY KEY`, `last_scanned_commit TEXT`, `last_scanned_at INTEGER`, `snapshot_source TEXT`, `created_at INTEGER NOT NULL DEFAULT (unixepoch())`.
- [ ] `crates/seshat-storage/migrations/V12__decisions_table.sql` creates the `decisions` table with the schema in §"Design — schema".
- [ ] Empty `decision_repository.rs` and `branch_repository.rs` extension stubs compile.
- [ ] `cargo build --workspace` passes.
- [ ] `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` pass.

### US-002: DecisionRepository implementation

**Description:** As a developer, I need a fully-tested decisions repository so that all consumers can read/write decisions through one API.

**Acceptance Criteria:**
- [ ] `DecisionRepository` trait with methods: `upsert`, `get_by_hash`, `get_by_hashes` (bulk), `delete`, `count_by_state`, `list`, `list_by_state`.
- [ ] `SqliteDecisionRepository` implementation.
- [ ] `Decision` struct (PRD §"Design — schema") implements `Debug + Clone + PartialEq`.
- [ ] Unit tests cover: empty table, single insert, upsert-replaces-on-conflict, bulk lookup with mixed-found/missing hashes, count by state filter, delete idempotency, list ordering.
- [ ] EXPLAIN QUERY PLAN on `get_by_hash` shows index lookup, not scan.

### US-003: BranchRepository extensions

**Description:** As a developer, I need per-branch metadata persisted in the `branches` table so that freshness checks have a sentinel to compare against.

**Acceptance Criteria:**
- [ ] `get_last_scanned_commit(&BranchId) -> Result<Option<String>>` reads from `branches`.
- [ ] `set_last_scanned_commit(&BranchId, &str) -> Result<()>` upserts `branches`, also writes `last_scanned_at = unixepoch()`.
- [ ] `ensure_branch_exists(&BranchId) -> Result<()>` is idempotent INSERT OR IGNORE.
- [ ] `list_branches` is rewritten to `SELECT branch_id FROM branches ORDER BY branch_id` instead of `SELECT DISTINCT branch_id FROM nodes`.
- [ ] `create_snapshot(source, target)` also INSERTs into `branches` with `snapshot_source = source.0`.
- [ ] Unit test `list_branches_reads_from_branches_table_not_nodes` verifies the regression cannot return.
- [ ] Unit test for `set_last_scanned_commit` upsert behaviour (overwrites previous value).

### US-004: Migrate MCP `record_decision`, `update_decision`, `remove_decision` to DecisionRepository

**Description:** As a developer, I need the MCP decision tools writing to the new table so that explicit user decisions and TUI approvals share one source of truth.

**Acceptance Criteria:**
- [ ] `seshat_mcp::tools::record_decision` writes to `decisions` with `state='recorded'`. No node is created.
- [ ] `seshat_mcp::tools::update_decision` updates existing row in `decisions` (description, reason, examples, weight).
- [ ] `seshat_mcp::tools::remove_decision` deletes from `decisions`.
- [ ] All `INSERT INTO nodes ...` SQL inside these tool implementations is removed.
- [ ] Existing MCP tool tests are updated to assert against the `decisions` table.
- [ ] `query_convention` and `query_project_context` MCP tools read from `decisions` (state='recorded' rows + state='approved'/'partial' rows count toward project knowledge).
- [ ] No regression in MCP envelope shape — `query_*` response JSON keeps the same fields, just sourced from a different table.

### US-005: TUI confirm / reject / partial migrate to DecisionRepository

**Description:** As a developer, I need the TUI review flow writing to the new table so that approvals propagate cross-branch.

**Acceptance Criteria:**
- [ ] `confirm_convention` (in `seshat-cli/src/tui/app.rs`) computes `description_hash` and UPSERTs `decisions` with `state='approved'`, `decided_on_branch=current`, examples serialised as JSON. Stops calling `record_decision`.
- [ ] `reject_convention` UPSERTs with `state='rejected'`. The auto-detected node DELETE step still runs (cosmetic; cleaner snapshot output).
- [ ] `partial_convention` UPSERTs with `state='partial'`. The "preference" node creation is dropped (preference rows now live in `decisions` with `state='partial'`).
- [ ] `optimistic concurrency` check (`expected_hash`) continues to operate on the auto-detected node's `ext_data` snapshot — the user-decided row is keyed by hash so collisions are not possible.
- [ ] Existing `tui_review_integration.rs` tests are updated to assert decision rows in `decisions`, not user nodes in `nodes`.

### US-006: Update `query_conventions_for_review` with LEFT JOIN

**Description:** As a user, I should not see conventions in the review queue that I have already decided on (any state).

**Acceptance Criteria:**
- [ ] Query rewritten as `LEFT JOIN decisions d ON d.description_hash = n.description_hash WHERE d.id IS NULL AND n.nature IN ('convention','observation') AND n.branch_id = ?1`.
- [ ] No usage of `ext_data->>'source'` or `ext_data->>'user_rejected'` remains in the review query.
- [ ] EXPLAIN QUERY PLAN confirms index usage on `decisions(description_hash)`.
- [ ] Unit test: insert two auto nodes, decide one (any state), verify only the undecided one returns.
- [ ] Unit test: bulk case — 100 auto nodes, 50 decided across all four states, verify only 50 returned.

### US-007: Update `count_confirmed_conventions`

**Description:** As a user, the review header shows the project-wide count of approved conventions, not branch-scoped.

**Acceptance Criteria:**
- [ ] `count_confirmed_conventions` becomes `SELECT COUNT(*) FROM decisions WHERE state IN ('approved','partial','recorded')`.
- [ ] No `branch_id` filter.
- [ ] Unit tests cover empty table, mixed states, large count.

### US-008: Update `persist_conventions` auto-scan dedup

**Description:** As a user, when I rescan after deciding on conventions, the decided ones must not be re-inserted as auto-detected.

**Acceptance Criteria:**
- [ ] `DELETE FROM nodes WHERE branch_id=?1 AND source='auto_detected'` no longer carries the `user_rejected` exception (rejections live in `decisions` now).
- [ ] Pre-compute `description_hash` for all aggregated conventions.
- [ ] Bulk-fetch `decisions` rows with `description_hash IN (...)` in a single query.
- [ ] For each aggregated convention with a matching decision: skip the INSERT.
- [ ] Auto-detected nodes are inserted with `description_hash` populated (column kept on `nodes` for the JOIN in US-006).
- [ ] Unit test: 100 conventions, 50 with matching decisions in any state, 50 inserted.
- [ ] Unit test: regression — bulk path issues exactly 1 SELECT against `decisions`, not N.

### US-009: Wire `last_scanned_commit` updates in scan paths

**Description:** As the system, I record the git HEAD at scan-completion time so that subsequent startups can detect divergence.

**Acceptance Criteria:**
- [ ] `scan_project` (in `seshat-cli/src/scan.rs`) reads `git rev-parse HEAD` and calls `branch_repo.set_last_scanned_commit(branch_id, head)` AFTER successful scan + persist.
- [ ] `background_sync` (in `seshat-cli/src/serve.rs`) does the same at end of run, regardless of diff/fallback path.
- [ ] `fallback_rescan` does the same.
- [ ] `execute_bulk_rescan` (in `seshat-watcher`) does the same.
- [ ] `run_detection_cycle_sync` invocations triggered by warm-tier do the same.
- [ ] Git-unavailable: skipped silently with debug log; column stays NULL.
- [ ] Integration test: scan succeeds → `branches.last_scanned_commit` matches `git rev-parse HEAD` for the active branch.

### US-010: HEAD-change detection in `run_serve`

**Description:** As a user, when I `git pull` on the same branch and restart Claude Code, seshat detects the new HEAD and rescans.

**Acceptance Criteria:**
- [ ] After `handle_branch_switch` returns final_branch, `run_serve` reads `last_scanned_commit` for that branch.
- [ ] Computes `git rev-parse HEAD`. If git unavailable: skip the comparison, no sync triggered.
- [ ] `needs_sync = sync_old_branch.is_some() || (last_scanned_commit != current_head)`.
- [ ] If `needs_sync`, spawn `background_sync` with the OLD commit hint (used for gix tree-diff if reachable; otherwise `fallback_rescan`).
- [ ] Logs include `old_head=<7-char>, new_head=<7-char>` so the user can see what triggered the sync.
- [ ] Integration test: with `last_scanned_commit=abc123`, set `git rev-parse HEAD` to `def456` (via fixture), start serve, assert `background_sync` was triggered.

### US-011: Blocking incremental sync in `run_review`

**Description:** As a user, when I run `seshat review` after changes, the TUI does not open until the DB is up-to-date with the current HEAD.

**Acceptance Criteria:**
- [ ] `run_review` reads `last_scanned_commit` for current branch.
- [ ] If different from `git rev-parse HEAD` (or `last_scanned_commit IS NULL`):
  - [ ] Print `Syncing project state to <head[..7]>...` to stdout with a spinner if stdout is a TTY (degrade to single-line summary when piped).
  - [ ] Run sync synchronously via a new `incremental_sync_blocking` function (extracted from `background_sync` body) with a progress callback that updates "Files: X / Y" on the same line at 1Hz.
  - [ ] Update `last_scanned_commit` after sync.
  - [ ] Open TUI.
- [ ] If git unavailable: skip sync silently, open TUI immediately.
- [ ] Integration test: with stale `last_scanned_commit`, run review, assert sync ran AND TUI received fresh data.
- [ ] Integration test: progress callback emits at least one update for a non-trivial diff.
- [ ] Integration test: git unavailable → TUI launches without sync, no errors.

### US-012: Git-unavailable single-branch fallback

**Description:** As a user, I can run seshat against a non-git directory (e.g., a download, a tmp dir, a git-less project) and decisions still work; freshness checks are quietly skipped.

**Acceptance Criteria:**
- [ ] `detect_branch` returns `"main"` when no `.git` is found (existing fallback). Document this as the synthetic-branch identity.
- [ ] Freshness comparisons (`run_serve` and `run_review`) treat `git rev-parse HEAD` failure as "no change detected" — sync NOT triggered, no warnings.
- [ ] All scan paths set `last_scanned_commit = NULL` when git is unavailable (no synthetic hash).
- [ ] Decision flow operates as on a single-branch project: all decisions decided on `branch="main"`, all queries scoped to `branch="main"`.
- [ ] Integration test: scan + review + decide + rescan in a non-git tmp dir → decisions persist, no errors.

### US-013: `seshat decisions list` CLI

**Description:** As a user, I can list all decisions made in this project from the command line.

**Acceptance Criteria:**
- [ ] New subcommand `seshat decisions list [--state approved|rejected|partial|recorded] [--branch <branch>] [--format json|table]`.
- [ ] Default format is table: `state | hash | description | decided_on_branch | decided_at`.
- [ ] `--format json` outputs an array of `Decision` JSON objects.
- [ ] No state filter → list all.
- [ ] `--branch` filters by `decided_on_branch`.
- [ ] Unit test: empty table, populated table, JSON output is valid JSON.

### US-014: `seshat decisions forget` CLI

**Description:** As a user, I can remove a decision so that the convention re-enters the review queue on next scan.

**Acceptance Criteria:**
- [ ] New subcommand `seshat decisions forget <hash> [--yes]`.
- [ ] Supports lookup by full description_hash or by ambiguity-free prefix (≥4 chars).
- [ ] Prints the matched decision and prompts `Forget this decision? [y/N]` unless `--yes`.
- [ ] On confirmation: DELETE from `decisions`. Subsequent `seshat scan + review` will re-emit the convention.
- [ ] Error: hash not found, ambiguous prefix, multiple hashes match.
- [ ] Integration test: forget approved decision → next scan re-emits it.

### US-015: `seshat decisions export` and `import` CLI

**Description:** As a user, I can back up decisions or share them across machines.

**Acceptance Criteria:**
- [ ] `seshat decisions export <file>` writes the decisions table as a JSON array to `<file>`.
- [ ] `seshat decisions import <file>` reads the JSON array and UPSERTs into `decisions`. Conflicts: latest `decided_at` wins (silently). `--strict` flag fails on any conflict instead.
- [ ] Round-trip test: export → wipe → import → table identical.

### US-016: Cross-branch decisions integration test

**Description:** As QA, the merge-and-no-reprompt scenario must be locked behind a regression test.

**Acceptance Criteria:**
- [ ] New file `crates/seshat-cli/tests/cross_branch_decisions.rs`.
- [ ] Test `approve_on_feature_persists_after_merge_to_main`: scan on `main`, scan on `feature`, approve convention on `feature`, simulated merge (move `main` ref to `feature`'s HEAD), restart scan on `main`, assert convention NOT in review queue.
- [ ] Test `reject_on_feature_persists_after_merge_to_main`: same with reject.
- [ ] Test `decision_survives_branch_deletion`: approve on `feature`, delete the branch, run scan on `main`, assert convention NOT in queue, assert decision row still exists.

### US-017: Freshness integration tests

**Description:** As QA, the serve-detects-HEAD-change and review-blocks-on-sync paths must be locked.

**Acceptance Criteria:**
- [ ] New file `crates/seshat-cli/tests/serve_freshness.rs`.
- [ ] Test `serve_detects_branch_label_change_and_syncs` (existing path — guard regression).
- [ ] Test `serve_detects_same_branch_head_change_and_syncs`.
- [ ] Test `serve_skips_sync_when_head_unchanged`.
- [ ] Test `serve_skips_sync_when_git_unavailable`.
- [ ] New file `crates/seshat-cli/tests/review_freshness.rs`.
- [ ] Test `review_blocks_on_sync_when_head_changed`.
- [ ] Test `review_skips_sync_when_head_unchanged`.
- [ ] Test `review_progress_updates_emitted_during_sync` (mock progress callback).
- [ ] Test `review_handles_git_unavailable_gracefully` (no-git tmp dir).

### US-018: Update existing tests

**Description:** As a developer, I need tests that reference the old user-node + ext_data schema to be migrated to the new model so CI stays green.

**Acceptance Criteria:**
- [ ] All assertions in `crates/seshat-detectors/tests/snippet_quality.rs` that check `ext_data.source = 'user'` or `description_hash` on nodes are updated to query `decisions`.
- [ ] All assertions in `crates/seshat-cli/tests/tui_review_integration.rs` are updated similarly.
- [ ] Any `INSERT INTO nodes` test fixture that relied on `source='user'` for user-decision testing is rewritten to insert into `decisions`.
- [ ] CI green: full workspace `cargo test --workspace` passes.

### US-019: README + ADR documentation

**Description:** As a maintainer, I need the design and the wipe-DB upgrade path documented so future developers and users understand the change.

**Acceptance Criteria:**
- [ ] New ADR `_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md` (or wherever ADRs live) documents:
  - Decision-table-vs-user-node trade-off
  - Why no migration
  - Git-optional fallback semantics
  - Worktree concurrency limitation
  - Future extensions (per-branch overrides, conflict resolution)
- [ ] README updated with `seshat decisions` subcommand reference.
- [ ] CHANGELOG entry: "BREAKING: DB schema redesigned. Existing DBs are incompatible — delete `~/.local/share/seshat/repos/<project>.db` and rescan."
- [ ] Smoke test doc `docs/smoke-tests/merge-aware-decisions.md` documenting US-001..US-016 manual verification steps.

## Functional Requirements

### Schema

- **FR-1.** Migration V11 creates `branches` table (PK on `branch_id`).
- **FR-2.** Migration V12 creates `decisions` table (PK on `description_hash`).
- **FR-3.** `description_hash` continues to use `compute_description_hash` from `seshat-graph::decisions` (SHA-256 of normalised description, 16 hex chars). No format change.
- **FR-4.** `nodes.description_hash` column is preserved (already populated by V8 + auto-scan); no longer used for user-decision dedup, only for the LEFT JOIN against `decisions`.
- **FR-5.** No data migration step is performed. Users wipe the DB after upgrade.

### Decision storage

- **FR-6.** All user-recorded decisions (TUI confirm/reject/partial AND MCP `record_decision`/`update_decision`) are stored in the `decisions` table.
- **FR-7.** `decisions` is keyed by `description_hash` (PRIMARY KEY). UPSERT on conflict replaces the existing row.
- **FR-8.** `decisions.state` is one of: `approved`, `rejected`, `partial`, `recorded`.
- **FR-9.** `decisions.decided_on_branch` records the branch active at decision time, for audit only — does not affect lookup.
- **FR-10.** `convention_decisions` is NOT scoped by `branch_id`. One row per `description_hash`, project-wide.

### Auto-scan integration

- **FR-11.** `persist_conventions` bulk-fetches matching `decisions` rows in one SELECT and skips INSERT for any auto-convention whose hash has a decision row in any state.
- **FR-12.** `query_conventions_for_review` excludes any auto-convention whose hash has a row in `decisions` (any state).
- **FR-13.** `count_confirmed_conventions` reads from `decisions` filtered by `state IN ('approved','partial','recorded')`.

### Freshness

- **FR-14.** `branches.last_scanned_commit` is updated at the end of every scan path (`scan_project`, `background_sync`, `fallback_rescan`, `execute_bulk_rescan`).
- **FR-15.** `branches.last_scanned_at` is updated alongside.
- **FR-16.** `run_serve` startup compares `branches.last_scanned_commit` for the resolved branch with `git rev-parse HEAD`. If different (or NULL when git is available), `background_sync` is spawned regardless of branch-label change.
- **FR-17.** `run_review` startup performs a blocking incremental sync (the same body as `background_sync`, exposed as `incremental_sync_blocking`) before opening the TUI when commits differ.
- **FR-18.** Progress callback for `incremental_sync_blocking` emits `(processed, total)` updates ≥ 1Hz.

### Git-optional

- **FR-19.** When `.git` is absent OR `git rev-parse HEAD` fails:
  - `detect_branch` returns `"main"` (existing).
  - All freshness comparisons skip silently with debug log.
  - `last_scanned_commit` stays `NULL`.
  - All scan / review / serve paths function normally; no warnings, no errors.

### CLI

- **FR-20.** New CLI subcommand group: `seshat decisions <list|forget|export|import>`.
- **FR-21.** `seshat decisions list` supports `--state`, `--branch`, `--format json|table` flags.
- **FR-22.** `seshat decisions forget <hash>` accepts full hash or ambiguity-free prefix (≥4 chars). Prompts for confirmation unless `--yes`.
- **FR-23.** `seshat decisions export <file>` writes JSON array.
- **FR-24.** `seshat decisions import <file>` UPSERTs from JSON array; conflicts resolved by latest `decided_at` unless `--strict`.

### MCP tools

- **FR-25.** `record_decision`, `update_decision`, `remove_decision` MCP tools all operate against `decisions` table. No node creation.
- **FR-26.** `query_convention` MCP tool returns the union of: undecided auto-detected `nodes` + `decisions` rows with `state` in `(approved, partial, recorded)`.
- **FR-27.** `query_project_context` includes a count of `decisions` rows by state in its summary.

## Non-Goals

- **NG1.** Concurrent `seshat serve` instances on different worktrees of the same main repo. Single global `metadata.current_branch` race remains. Documented as known limitation.
- **NG2.** Detached HEAD optimisation. Each unique commit hash becomes a `branch_id`. Documented.
- **NG3.** Per-branch decision overrides. The schema's `decided_on_branch` is audit-only; lookup is project-wide. A future iteration could add a `scope` column.
- **NG4.** Preserving any existing user data. The DB is regenerated.
- **NG5.** Conflict resolution UI. `seshat decisions import` resolves conflicts by `decided_at` silently (or fails under `--strict`).
- **NG6.** Cross-machine decision sync infrastructure. `decisions export/import` provides the file-based path; no daemon, no shared remote.
- **NG7.** Versioning of description-hash format. The hash format is part of the release contract; if detector wording changes, all decisions become stale and must be re-decided.

## Design Considerations

### Schema

```sql
-- V11: branches table.
-- Replaces the implicit "SELECT DISTINCT branch_id FROM nodes" pattern.
CREATE TABLE IF NOT EXISTS branches (
    branch_id            TEXT PRIMARY KEY,
    last_scanned_commit  TEXT,
    last_scanned_at      INTEGER,
    snapshot_source      TEXT,
    created_at           INTEGER NOT NULL DEFAULT (unixepoch())
);
```

```sql
-- V12: decisions table.
-- Single source of truth for all user-recorded knowledge:
--   state='approved' / 'rejected' / 'partial' — TUI review of auto-detected
--   state='recorded'                          — explicit decision via MCP record_decision
CREATE TABLE IF NOT EXISTS decisions (
    description_hash     TEXT NOT NULL PRIMARY KEY,
    description          TEXT NOT NULL,
    state                TEXT NOT NULL CHECK (state IN ('approved','rejected','partial','recorded')),
    nature               TEXT NOT NULL CHECK (nature IN ('convention','decision','preference','fact')),
    weight               TEXT NOT NULL CHECK (weight IN ('rule','strong')),
    category             TEXT,
    reason               TEXT,
    examples             TEXT,                  -- JSON: [{file, line, end_line, snippet}, ...]
    decided_on_branch    TEXT NOT NULL,
    decided_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_decisions_state             ON decisions(state);
CREATE INDEX IF NOT EXISTS idx_decisions_decided_on_branch ON decisions(decided_on_branch);
```

### Repository surface

```rust
// seshat-storage::repository::decision_repository

pub struct Decision {
    pub description_hash: String,
    pub description: String,
    pub state: DecisionState,
    pub nature: DecisionNature,
    pub weight: DecisionWeight,
    pub category: Option<String>,
    pub reason: Option<String>,
    pub examples: Vec<ExampleEvidence>,
    pub decided_on_branch: BranchId,
    pub decided_at: i64,
    pub updated_at: i64,
}

pub enum DecisionState  { Approved, Rejected, Partial, Recorded }
pub enum DecisionNature { Convention, Decision, Preference, Fact }
pub enum DecisionWeight { Rule, Strong }

pub trait DecisionRepository {
    fn upsert(&self, d: Decision) -> Result<()>;
    fn get_by_hash(&self, hash: &str) -> Result<Option<Decision>>;
    fn get_by_hashes(&self, hashes: &[&str]) -> Result<HashMap<String, Decision>>;
    fn delete(&self, hash: &str) -> Result<()>;
    fn count_by_state(&self, state: DecisionState) -> Result<usize>;
    fn list(&self) -> Result<Vec<Decision>>;
    fn list_by_state(&self, state: DecisionState) -> Result<Vec<Decision>>;
}
```

### Review query (after migration)

```sql
SELECT n.id, n.description, n.nature, n.weight, n.confidence,
       n.adoption_count, n.total_count, n.ext_data, n.description_hash
FROM nodes n
LEFT JOIN decisions d ON d.description_hash = n.description_hash
WHERE n.nature IN ('convention', 'observation')
  AND n.branch_id = ?1
  AND d.description_hash IS NULL
ORDER BY n.confidence DESC
```

### Persist-conventions dedup (after migration)

```rust
let hashes: Vec<String> = aggregated.iter()
    .map(|c| compute_description_hash(&c.description))
    .collect();

let decisions: HashMap<String, Decision> = decision_repo
    .get_by_hashes(&hashes.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;

for (conv, hash) in aggregated.iter().zip(&hashes) {
    if decisions.contains_key(hash) {
        // Any state suppresses re-insertion.
        continue;
    }
    insert_auto_node(conv, hash);
}
```

### TUI confirm flow

```rust
fn confirm_convention(conn, branch_id, description, examples) {
    let hash = compute_description_hash(description);
    decision_repo.upsert(Decision {
        description_hash: hash,
        description: description.to_owned(),
        state: DecisionState::Approved,
        nature: DecisionNature::Convention,
        weight: DecisionWeight::Strong,
        category: None,
        reason: Some("Confirmed via seshat review TUI".to_owned()),
        examples,
        decided_on_branch: BranchId::from(branch_id),
        decided_at: unix_now(),
        updated_at: unix_now(),
    })
}
```

### Run_serve startup (HEAD-change branch)

```rust
let needs_sync = sync_old_branch.is_some() || {
    let last = branch_repo.get_last_scanned_commit(&final_branch).ok().flatten();
    let head = git_rev_parse_head(&project_root);
    match (last, head) {
        (Some(last), Some(head)) => last != head,
        (None, Some(_))          => true,         // never scanned this branch
        _                        => false,        // git unavailable
    }
};

if needs_sync {
    spawn_background_sync(...);
}
```

### Run_review startup (blocking sync)

```rust
fn run_review(project_path) -> Result<()> {
    let resolved = resolve_project(project_path, "review")?;
    let branch_id = detect_branch(&resolved.project_root);
    let conn = open_db(&resolved.db_path)?;
    let branch_repo = SqliteBranchRepository::new(conn.clone());

    if let (Some(last), Some(head)) = (
        branch_repo.get_last_scanned_commit(&BranchId::from(&branch_id)).ok().flatten(),
        git_rev_parse_head(&resolved.project_root),
    ) {
        if last != head {
            print_progress_header(&head);
            incremental_sync_blocking(
                &resolved.project_root,
                &branch_id,
                &last,
                &head,
                &conn,
                &mut tui_progress_callback,
            )?;
            branch_repo.set_last_scanned_commit(&BranchId::from(&branch_id), &head)?;
        }
    }
    // Else: git unavailable → silent skip.

    run_review_tui_with_conn(&branch_id, &conn)
}
```

## Technical Considerations

- **No data migration.** This is a deliberate trade-off. Existing seshat installs require a one-time DB wipe (documented in CHANGELOG and README). This is acceptable because seshat is pre-1.0 and not in production use.
- **`compute_description_hash` is shared** between MCP tools and TUI. Both compute identical hashes for identical descriptions.
- **Bulk decision lookup** in `persist_conventions` uses `WHERE description_hash IN (?, ?, ...)`. SQLite's parameter limit is 999; for safety, batch into chunks of ≤500.
- **Snapshot copy logic** (`create_snapshot`) does NOT need to copy `decisions` — they are project-wide, branch-independent. This is a positive side-effect of the redesign.
- **Performance:** the JOIN on `decisions(description_hash)` index is O(1) lookup per auto-node row. For a 500-convention review query, the join adds ~500 index probes — negligible.
- **TUI optimistic concurrency.** The current `expected_hash` mechanism on auto-node `ext_data` is preserved for the auto-node's own state. The decision row itself does not need optimistic concurrency because it is keyed by `description_hash` and UPSERT is idempotent for the same description text.
- **Git command shelling.** `git rev-parse HEAD` is invoked via `std::process::Command`. Already used by `detect_branch`. No new external dependency.
- **Synthetic branch identity** in non-git directories: the existing `detect_branch` fallback to `"main"` is sufficient. No new code needed for that path beyond the freshness skips.

## Success Metrics

- Cross-branch approval scenario (US-016) passes a fresh-checkout-and-pull cycle without re-prompting.
- `seshat review` startup latency: incremental sync completes in < 5 s on a 1000-file project with 10-file diff.
- 100% of CI checks (fmt, clippy, all tests) green at every commit in the implementation order.
- Manual smoke-test (US-001..US-016 from the smoke-test doc) passes on a real `seshat` + `walt-chat-backend` workflow.

## Open Questions

- **Q1.** Should `seshat decisions list` include FTS search (`--query <text>`)? *Default:* No, follow-up.
- **Q2.** Should the blocking `seshat review` sync be opt-out via `--no-sync`? *Default:* Yes, add the flag for emergency / debug use.
- **Q3.** When the user wipes the DB and re-scans, should the TUI show "this is your first scan" hint? *Default:* No, behaviour is identical to a fresh project.
- **Q4.** Should `decisions.examples` allow updating without changing other fields (i.e., a "merge examples" UPSERT mode)? *Default:* No, full row replace is fine for now.
- **Q5.** Concurrency: if two MCP tools update_decision the same hash concurrently, last-write-wins. Is that acceptable? *Default:* Yes; a future feature could add a version column for optimistic concurrency.

## Implementation Order

Strict — each step is a separate commit, CI green at every step:

1. **US-001** Migrations + repo skeletons.
2. **US-002** DecisionRepository + tests.
3. **US-003** BranchRepository extensions + tests.
4. **US-008** persist_conventions rewrite + tests (no consumer migration yet — works with empty `decisions`).
5. **US-005** TUI confirm/reject/partial migration.
6. **US-006** + **US-007** review query + count refactor.
7. **US-018** Update existing tests (CI must stay green here).
8. **US-004** MCP tools migration (last in storage-layer migration to keep MCP queries working through the transition).
9. **US-009** Wire `last_scanned_commit` writes.
10. **US-010** run_serve HEAD-change detection.
11. **US-011** run_review blocking sync.
12. **US-012** Git-unavailable fallback verification (mostly tests; behaviour falls out from US-009/10/11).
13. **US-013**, **US-014**, **US-015** CLI subcommand.
14. **US-016**, **US-017** Cross-branch and freshness integration tests.
15. **US-019** Documentation.

## Addendum: post-review clarifications

Clarifications added after the chunked code review. These resolve
ambiguities or under-specified behaviours that the original PRD left
implicit. Each item references the review finding it addresses.

### A1. `DecisionRepository::upsert` takes `&Decision` (S1)

The "Repository surface" sketch listed `fn upsert(&self, d: Decision)`
(by value). The implementation takes `&Decision`, which is the
idiomatic Rust form and avoids a clone at every callsite. Treat the
borrowed signature as authoritative.

### A2. `description_hash` is content-derived; updates migrate the PK (S2 + S3)

`description_hash` is a 16-character hex prefix of `SHA-256` over the
normalised description text (via `compute_description_hash`). The
truncation gives 64 bits of identity space. Birthday-collision
probability becomes appreciable around ~2³² distinct descriptions —
acceptable for the foreseeable corpus (single-project knowledge bases
have hundreds to low thousands of decisions, not billions). If a
project is projected to exceed that, switch to the full 64-char hash
in a future migration.

The PK ↔ description invariant is mandatory:

> `description_hash == compute_description_hash(description)`

When `update_decision` rewrites `description`, it MUST recompute the
hash and migrate the row to the new PK (DELETE old + INSERT new in a
single transaction via `DecisionRepository::rekey`). MCP clients that
hold a hash from a previous response must re-fetch by description if
they get `NODE_NOT_FOUND`. This is preferred over freezing a stale PK
that no longer matches the row's content, because the stale PK
silently breaks dedup-by-hash everywhere downstream
(`persist_conventions`, cross-branch propagation through G1, MCP
collision detection).

### A3. `create_snapshot` re-snapshot semantics (S4)

`create_snapshot(source, target)` is intended for one-shot branch
materialisation (e.g., the watcher creating a new branch's snapshot
from the previous commit). Calling it a second time against an
existing `target` IS allowed and overwrites `branches.snapshot_source`
with the new `source`. Invariant: callers MUST NOT pass `source ==
target` — the combination produces a self-referential snapshot row
with no meaningful semantics. Implementations should `debug_assert!`
this in tests and trust the invariant in production.

### A4. `decisions.examples` corruption policy is fail-closed (S5)

If `decisions.examples` (TEXT JSON) cannot be deserialised into
`Vec<ExampleEvidence>`, the repository surfaces
`StorageError::SerializationError` and the corresponding
`list()`/`list_by_state()`/`get_by_hashes()` call returns `Err` for
the entire batch — one bad row poisons the listing. This is
intentional: the only writers in the codebase produce well-formed
JSON, so corruption indicates either (a) external mutation of the DB
file or (b) a bug. Fail-closed surfaces the problem instead of
silently dropping examples.

### A5. `decisions.decided_on_branch` is an audit-only string (S6)

The column records the branch active when the decision was made. It
has no foreign key to `branches`, no cascade behaviour, and is NOT
consulted by any lookup query (the index on it exists for historical
inspection only — see FR-9). When a git branch is deleted
(US-016 / G5), the corresponding `decisions` rows survive and their
`decided_on_branch` becomes a dangling string referring to a branch
that no longer exists in `branches`. This is by design: G5 mandates
that decisions outlive their originating branch, and the audit field
is informational. Tools displaying decisions to users should treat
`decided_on_branch` as a hint that may not resolve in the current
git state.

### A6. `decisions.state` lifecycle and `partial` entry path (S7)

The four legal states have these origins:

- `recorded` — written by MCP `record_decision`. The MCP-mutable
  subset.
- `approved` — written by TUI `confirm_convention`.
- `rejected` — written by TUI `reject_convention`.
- `partial` — written by TUI `partial_convention`. Pre-V12 this lived
  in a separate `preference` node; V12 collapses it into `decisions`.

There is no transition path between states through the public API.
`update_decision` and `remove_decision` (MCP) refuse to mutate or
delete rows whose state is not `recorded` (returns
`NOT_USER_DECISION`); rows with state ∈ {approved, rejected, partial}
are owned by the TUI flow and only the TUI can re-decide them. To
"override" a TUI decision, an agent must re-run the TUI review on
the same convention; the upsert at TUI commit time replaces the row
in place.

`record_decision` always writes `state='recorded'`. `partial` is NOT
reachable from the MCP path.

### A7. Legacy `id` / `node_id` envelope shim (H3)

The MCP envelope for `record_decision`, `update_decision`, and
`remove_decision` returns BOTH the new `description_hash` field
(authoritative) AND legacy `data.id: 0` / `metadata.node_id: 0`
(integer sentinel). The legacy fields exist only to keep pre-V12
clients parsing — they always carry the sentinel zero and contain no
information. Schedule for removal one release after V12 ships; grep
for `LEGACY_ID_SENTINEL` in `seshat-graph/src/decisions.rs` to find
the cleanup site.

## Failure-mode checklist

For each story, the implementer must verify:

- [ ] Fresh DB creates V11 + V12 tables.
- [ ] `decisions` UPSERT honours `ON CONFLICT(description_hash)`.
- [ ] `branches` UPSERT (via `set_last_scanned_commit`) honours `ON CONFLICT(branch_id)`.
- [ ] All bulk paths use ≤500 params per SELECT (chunked beyond that).
- [ ] `last_scanned_commit` is written at the END of each scan path (so a mid-scan crash leaves the OLD value for retry, not the partial new one).
- [ ] Git-unavailable paths emit zero warnings to stdout/stderr.
- [ ] All new types implement `Debug + Clone`.
- [ ] `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` pass.
- [ ] All existing tests still pass.
