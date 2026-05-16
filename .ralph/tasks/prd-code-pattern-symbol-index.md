# PRD: `query_code_pattern` symbol-index enrichment (Quick Win)

## Introduction

**Type:** Feature

Enrich `query_code_pattern` MCP tool responses with per-symbol **dependent files**, **blast radius**, and **aggregated call sites** by introducing a pre-computed symbol index in SQLite. Today the tool returns the symbol's definition and a flat list of call sites; agents asking «расскажи мне про эту функцию» have to infer the wiring (who imports it, how far does a rename ripple) themselves. After this change, one MCP call gives them the full picture.

**Scope is intentionally narrow:** internal symbols only (functions, types, exports defined in project source files — both `pub` and private). External packages stay in their current `dependency_usage` path; unifying them into the symbol index is a separate architectural change (deferred).

**Pre-computed, not on-the-fly:** dependent-file and blast-radius lookups must hit indexed SQLite tables populated at scan time and maintained by the watcher's hot tier. No per-query IR scan beyond what already happens for call-site collection.

## Goals

- Add `dependent_files` to every symbol match — list of files that `import` (Rust `use`, TS `import`, Python `from … import …`) the symbol's name.
- Add `blast_radius` (`low` / `medium` / `high`) to every symbol match — using the same thresholds as `query_dependencies` (low < 5, medium 5–20, high > 20).
- Aggregate `call_sites` by file: `{file, site_count, lines, first_snippet}` per file rather than a flat list with one row per occurrence.
- Pre-compute the dependent lookup in SQLite — full-scan populate + incremental maintenance via watcher.
- Maintain existing matching semantics (exact > prefix > contains; pub-and-private; functions + types + exports).
- Keep the change pre-1.0-compatible: drop the old flat `call_sites` shape without a deprecation shim.

## User Stories

### US-001: Schema + migration for `symbol_definitions` and `symbol_imports` tables (with backfill)
**Description:** As a Seshat maintainer, I need new SQLite tables that index symbol definitions and import sites so that per-symbol lookups become O(log N), AND existing DBs must be retrofitted in the same migration so first-call after upgrade is not empty.

**Acceptance Criteria:**
- [ ] New migration `V13__symbol_index.sql` creates two tables:
  - `symbol_definitions(branch_id TEXT, symbol_name TEXT, file_path TEXT, line INTEGER, end_line INTEGER, kind TEXT, is_public INTEGER, snippet TEXT)` — `kind` is one of `function|type|export`. `snippet` holds the truncated definition snippet so query path needs no JOIN with `files_ir`.
  - `symbol_imports(branch_id TEXT, imported_name TEXT, importer_file TEXT)` — one row per concrete-named import per file.
- [ ] Indexes on `(branch_id, symbol_name)` for `symbol_definitions` and `(branch_id, imported_name)` for `symbol_imports`.
- [ ] **Backfill step inside the migration**: read every row in `files_ir`, deserialize `ir_data`, extract Function/TypeDef/Export rows into `symbol_definitions` and concrete-named Import rows into `symbol_imports`. Migration must produce identical row counts to what a fresh scan of the same project would.
- [ ] Backfill is idempotent: running V13 a second time (impossible in practice, but defensive) does not duplicate rows.
- [ ] Migration runs cleanly on (a) empty DB, (b) existing `seshat.db`, (c) very small DB with one file, (d) DB whose `ir_data` blobs span all four languages (Rust/TS/JS/Python).
- [ ] `cargo test -p seshat-storage` passes; new migration test asserts backfill row counts for a fixture DB.

### US-002: Populate symbol index during full scan
**Description:** As a developer running `seshat scan`, I need symbol-index tables to fill up after a clean scan so subsequent `query_code_pattern` calls have data to read.

**Acceptance Criteria:**
- [ ] After scanner persists `files_ir` for a file, also persist its symbol-definition rows (one per Function/TypeDef/Export from the IR) and symbol-import rows (one per concrete-named Import).
- [ ] **Skip wildcard imports**: `use foo::*` (Rust), `from foo import *` (Python), `import * as foo from '…'` (TS) — produce ZERO `symbol_imports` rows. They contribute no per-symbol dependency signal.
- [ ] **Aliased imports**: `use foo::Bar as Baz` (Rust), `import { Bar as Baz } from '…'` (TS), `from foo import Bar as Baz` (Python) — store `Bar` (the defining name) in `imported_name`, not `Baz`. Lookups happen from the defining side; if we stored the local alias, dependents would be invisible from a `query_code_pattern("Bar")` call.
- [ ] Persisting is transactional with `files_ir` upsert — either both succeed or both roll back.
- [ ] Re-scanning the same project yields identical row counts (idempotent on full scan).
- [ ] `cargo test -p seshat-scanner` passes; new integration test asserts (a) row counts match an expected fixture, (b) a fixture with both wildcard and aliased imports produces the expected rows (alias stored as defining name; wildcard absent).
- [ ] Manual: `seshat scan` over the seshat repo populates non-empty `symbol_definitions` and `symbol_imports`.

### US-003: Maintain symbol index incrementally via watcher hot tier
**Description:** As an agent connected to a long-running `seshat serve`, I need symbol-index rows to stay in sync as files change — without restarting the server.

**Acceptance Criteria:**
- [ ] When the hot tier upserts a file's IR (file modified/added), it first deletes existing `symbol_definitions` / `symbol_imports` rows for that file in the current branch, then inserts the new set.
- [ ] When the hot tier deletes a file (file removed from disk), it deletes the corresponding rows from both tables.
- [ ] Branch-switch sync (`incremental_sync_blocking`) also keeps the index in sync — rebuild for the new branch's HEAD.
- [ ] Integration test in `crates/seshat-watcher`: modify a fixture file with a `pub fn foo()` → assert row exists for `foo` after the watcher processes the event; remove the function → assert row is gone.

### US-004: Return `dependent_files` per symbol match in `query_code_pattern`
**Description:** As an agent asking about `BranchId`, I want to see which files import it so I can predict the blast of a rename.

**Acceptance Criteria:**
- [ ] Each `pattern` entry in the response gains a `dependent_files: Vec<String>` field.
- [ ] The list is computed via `SELECT DISTINCT importer_file FROM symbol_imports WHERE branch_id=? AND imported_name=?`.
- [ ] Excludes the defining file itself (a file doesn't depend on its own definitions).
- [ ] Re-exports are NOT chased — only direct `use … ::Name` counts (per agreed scoping; see Non-Goals).
- [ ] Empty list for private symbols whose name never appears in any import — verified by a test.
- [ ] Unit test in `crates/seshat-graph` for a fixture with two importers of `BranchId`.

### US-005: Return `blast_radius` per symbol match
**Description:** As an agent, I want a single low/medium/high signal for "how risky is touching this" without counting list entries myself.

**Acceptance Criteria:**
- [ ] Each `pattern` entry gains `blast_radius: String` with values `"low" | "medium" | "high"`.
- [ ] Thresholds: `low` < 5 dependent_files, `medium` 5–20, `high` > 20 — same as `query_dependencies` for files (extract shared helper if not already factored out).
- [ ] Test covering each threshold boundary (4, 5, 19, 20, 21).

### US-006: Aggregate `call_sites` by file in `query_code_pattern` response
**Description:** As an agent, I want call-site data grouped by file so I can scan-read the result without manually counting entries per file.

**Acceptance Criteria:**
- [ ] `call_sites` in the response changes shape from `Vec<{file, line, end_line, snippet}>` to `Vec<{file, site_count, lines: Vec<u32>, first_snippet}>` — one entry per file, sorted by `site_count` descending.
- [ ] `first_snippet` is the snippet of the lowest-line occurrence in that file (truncated per existing `truncate_snippet`).
- [ ] New top-level field `total_call_sites: usize` preserves the prior `call_site_count` semantics.
- [ ] Pre-1.0: the old flat-list shape is removed entirely — no shim, no fallback (per project policy `feedback_no_backward_compat`).
- [ ] Unit test covers a symbol used 4× in one file and 1× in another → two entries, sorted, with correct counts.

### US-007: Update tool description + envelope metadata
**Description:** As an agent reading MCP tool descriptions, I need the new fields documented so I know what to expect.

**Acceptance Criteria:**
- [ ] `ProjectCodePatternRequest` schema docstring in `crates/seshat-mcp/src/tools/query_code_pattern.rs` mentions `dependent_files`, `blast_radius`, and the aggregated `call_sites` shape.
- [ ] `next_steps` in the response metadata stays useful — if `blast_radius == "high"`, suggest reviewing `dependent_files` before any change.
- [ ] No changes to the request shape (input parameters stay backward compatible).
- [ ] Existing handler test in `crates/seshat-mcp/src/tools/query_code_pattern.rs` updated to assert the new shape.

### US-009: Route symbol matching through `symbol_definitions` index
**Description:** As a Seshat maintainer, I want `query_code_pattern`'s matching itself to read from the new index, not load every IR blob in the branch — otherwise we have two parallel data sources for the same question and they will drift. Architecturally clean before fast.

**Acceptance Criteria:**
- [ ] `query_code_pattern` symbol lookup runs against `symbol_definitions` via SQL, not via in-memory iteration over deserialized `files_ir.ir_data`.
- [ ] Matching semantics preserved exactly:
  - Exact match (`name == query`) ranks above prefix match (`name LIKE 'query%'`) ranks above contains match (`name LIKE '%query%'`).
  - Same scoring values returned (1.0 / 0.7 / 0.4 — or whatever the current code uses).
  - `kind` filter (`function`/`type`/`export`/`all`) applied as a SQL `WHERE` clause, not post-filter.
- [ ] `is_public` filter (currently implicit) preserved.
- [ ] Embedding-similarity fallback (opt-in feature `builtin-embeddings`) — out of scope for this story: it can continue to load IR if it needs full text, OR be re-pointed at `symbol_definitions.snippet`. Document the chosen path in the implementation comment.
- [ ] The full-IR-scan code path used today for definition lookup is REMOVED, not left dormant. Call-site collection in `code_pattern.rs` (which still needs IR) stays.
- [ ] Bench (one-off, manual): run a `query_code_pattern("BranchId")` before and after. The post-change query must hit < 50ms on the seshat DB (~2000 files) — verify with `tracing` instrumentation or `criterion`-style micro-bench in tests.
- [ ] Existing unit and integration tests for `query_code_pattern` continue to pass without semantic adjustment (the tests already assert response shape; they should not care HOW the match was computed).
- [ ] New test: insert a fixture with 1000 definitions; assert lookup time is bounded (sanity guard, not a perf budget).

### US-008: Adversarial code review pass + cleanup
**Description:** As a Seshat maintainer, I want the changes on this branch put through `KSD-CodeReview` (Blind Hunter + Edge Case Hunter + Acceptance Auditor) in the context of this PRD and with Rust idiomatic conventions in mind, then everything it surfaces fixed — so the merge into `main` is genuinely clean, not just green CI.

**Acceptance Criteria:**
- [ ] Invoke the `KSD-CodeReview` skill against the full diff of `feat/code-pattern-symbol-index` vs `main`.
- [ ] Pass this PRD (`.ralph/tasks/prd-code-pattern-symbol-index.md`) into the review as acceptance context — the Auditor layer must compare the diff to ACs of US-001…US-007 and flag any AC that is not actually met by code or tests.
- [ ] Reviewer focus areas explicitly include: SQL injection safety on the new repo functions, transactional correctness of the `files_ir` + symbol-index upsert pair, idempotency of incremental updates in `hot_tier`, Rust idiom (clippy pedantic-level: avoid `unwrap`, prefer `?`, no needless clones, lifetimes over `Arc` where possible, no `&String`/`&Vec<T>` parameters), and proper migration backfill behavior on existing DBs.
- [ ] Triage every finding into one of: **must-fix**, **should-fix**, **nit**, or **deferred** (with rationale + link to a follow-up issue/PRD section).
- [ ] All **must-fix** and **should-fix** findings resolved by additional commits on the same branch.
- [ ] **nit** findings either addressed inline or explicitly listed in the PR description as accepted nits.
- [ ] **deferred** findings each have a tracked follow-up — either a new section in this PRD's Open Questions or a separate task file.
- [ ] After fixes: re-run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` — all clean.
- [ ] Final review pass shows zero new must-fix/should-fix findings (a second KSD-CodeReview run is recommended if the first round produced large structural fixes).
- [ ] Summary of the review (findings + resolutions) attached to the PR description so the human reviewer can scan it before approving.

## Functional Requirements

- FR-1: Add `symbol_definitions` and `symbol_imports` tables via a new migration; index by `(branch_id, name)`.
- FR-2: Scanner persists rows for every Function, TypeDef, Export, and Import in every parsed file.
- FR-3: Watcher hot tier maintains those rows on every file modify/add/delete event.
- FR-4: `query_code_pattern` reads `dependent_files` directly from the `symbol_imports` index (one SQL query per match).
- FR-5: `query_code_pattern` returns `blast_radius` derived from `dependent_files.len()` using the same thresholds as `query_dependencies`.
- FR-6: `query_code_pattern` aggregates call_sites by file (`{file, site_count, lines, first_snippet}`) and adds `total_call_sites`.
- FR-7: Call-site collection itself stays in its current code path (IR scan in `code_pattern.rs`) — only the *output shape* changes.
- FR-8: The defining file is excluded from `dependent_files` even if it self-imports a re-export.
- FR-9: Re-export chains are NOT traversed — only direct `use foo::Bar` matches count for dependents.
- FR-10: Branch-switch / incremental-sync (`incremental_sync_blocking` in `serve.rs`) rebuilds the symbol index for the new branch's HEAD.
- FR-11: Wildcard imports (`use foo::*`, `from foo import *`, `import * as foo`) are skipped at population time — they generate zero `symbol_imports` rows.
- FR-12: Aliased imports store the defining (rightmost) symbol name in `imported_name`, NOT the local alias.
- FR-13: Symbol-definition matching in `query_code_pattern` runs against `symbol_definitions` (SQL), not against deserialized IR blobs. The old IR-scan code path for definition lookup is removed.
- FR-14: Migration V13 backfills both tables from existing `files_ir` blobs as part of its `up()` so first-call after upgrade returns non-empty data without requiring a manual rescan.

## Non-Goals (Out of Scope)

- **External packages** (e.g. `redis`, `tokio`) are NOT unified into the symbol index. Their current `dependency_usage` detector + convention-node path stays. A separate PRD will address unification.
- **Re-export resolution.** If `crate_a` does `pub use crate_b::Bar` and `crate_c` imports `crate_a::Bar`, `crate_c` is a dependent of `crate_a::Bar`, NOT `crate_b::Bar`. Same-name disambiguation across files is out of scope.
- **Matching scoring semantics.** The exact > prefix > contains ranking and `kind` filter stay identical in behavior — only the data source (SQL vs IR scan) changes per US-009.
- **`related_conventions` cleanup.** The field stays present and populated as today; a future PRD will split code/convention surfaces (per the broader brainstorm in `~/.claude/plans/bmad-help-shimmering-iverson.md`).
- **Pre-computing call-sites.** Call sites still come from on-load IR scan in `code_pattern.rs`. Only the *response shape* is aggregated; the upstream computation is unchanged.
- **Performance optimizations beyond what the index naturally gives.** No new caching, no embedding indexes (beyond what already exists), no batched queries.
- **Embedding-similarity rework.** The opt-in `builtin-embeddings` path can keep its IR scan OR be moved to read `symbol_definitions.snippet`; either is acceptable. Out of scope to redesign embedding semantics.
- **Walt-* DBs, other languages.** Indexer must support Rust, TypeScript, JavaScript, Python as the existing parsers already do — no new language support.

## Technical Considerations

- **Files touched (Rust workspace):**
  - `crates/seshat-storage/migrations/V13__symbol_index.sql` — new
  - `crates/seshat-storage/src/repository/symbols.rs` — new (repo for the two tables)
  - `crates/seshat-storage/src/lib.rs` — wire new repo into the module surface
  - `crates/seshat-scanner/src/orchestrator.rs` — persist symbol rows after IR upsert
  - `crates/seshat-watcher/src/hot_tier.rs` — maintain on modify/add/delete
  - `crates/seshat-cli/src/serve.rs::incremental_sync_blocking` — rebuild index on branch switch
  - `crates/seshat-graph/src/code_pattern.rs` — read from index, compute blast_radius, aggregate call_sites
  - `crates/seshat-mcp/src/tools/query_code_pattern.rs` — schema docstring + handler tests
- **Reusing existing infra:**
  - Blast radius thresholds: extract from `crates/seshat-graph/src/dependencies.rs` if not factored out — common helper `fn classify_blast_radius(count: usize) -> &'static str`.
  - Snippet truncation: `truncate_snippet` from `seshat-core`.
- **Transactional safety:** Symbol-index upserts and `files_ir` upsert MUST share the same SQLite transaction. If the symbol-index write fails the IR write also rolls back — never partial state.
- **Empty input handling:** `imports` rows where the imported name is wildcard (`use foo::*`) — skip them; they don't contribute to per-symbol dependents.
- **Worktree:** Set up `../seshat-code-pattern-quick-win` worktree on branch `feat/code-pattern-symbol-index` before any code change, per `feedback_worktree_for_parallel_work`.

## Design Considerations

Response shape after this PRD (illustrative, single match):

```json
{
  "name": "BranchId",
  "kind": "type",
  "file_path": "crates/seshat-core/src/ids.rs",
  "line": 14,
  "end_line": 14,
  "is_public": true,
  "snippet": {"content": "pub struct BranchId(...)", "truncated": false},
  "score": 1.0,
  "dependent_files": [
    "crates/seshat-cli/src/decisions.rs",
    "crates/seshat-cli/src/tui/app.rs",
    "crates/seshat-graph/src/decisions.rs"
  ],
  "blast_radius": "medium",
  "call_sites": [
    {
      "file": "crates/seshat-cli/src/decisions.rs",
      "site_count": 4,
      "lines": [930, 940, 950, 960],
      "first_snippet": "BranchId(branch.to_owned())"
    },
    {
      "file": "crates/seshat-cli/src/tui/app.rs",
      "site_count": 1,
      "lines": [631],
      "first_snippet": "BranchId(branch_id.to_owned())"
    }
  ],
  "total_call_sites": 5
}
```

## Success Metrics

- A `query_code_pattern("BranchId")` against the seshat repo returns `dependent_files.len() ≥ 3` and a `blast_radius` of `medium` or `high` — verified live after merge.
- Response payload for the same probe stays under 30 KB (aggregated call_sites pull the byte count down even for symbols with many usages).
- New tests added; full workspace `cargo test` passes; pre-commit (clippy + fmt) clean.
- After merge + rebuild, agents using the seshat MCP get the new shape on the next call — no DB rebuild required for existing scans (they get an empty `symbol_imports` until next watcher event re-populates the file, OR a one-shot rescan; document this trade-off).

## Decisions (resolved before kickoff)

1. **Migration backfill** — YES. V13 reads existing `files_ir` blobs and populates `symbol_definitions` + `symbol_imports` in the same `up()`. First call after upgrade returns non-empty data without manual rescan. Captured as US-001 AC.
2. **Wildcard imports** — SKIP at population time. They generate zero `symbol_imports` rows. Resolving `*` to concrete names would require cross-file analysis we don't do today. Captured as FR-11 and US-002 AC.
3. **Aliased imports** — store **defining (rightmost) name**, NOT the local alias. `use foo::Bar as Baz` stores `Bar` in `imported_name`. Because the lookup origin is the defining side: a caller asking `query_code_pattern("Bar")` must find every file that pulls `Bar` into scope, regardless of what each one renames it to locally. Captured as FR-12 and US-002 AC.
4. **TS namespaced imports** (`import * as foo from './bar'`) — treated identically to wildcard. SKIP.
5. **Definition lookup through the index** — YES, do it now. Rationale (user, verbatim): «У нас нет заданий прямо сейчас за 5 минут все починить. Есть задача сделать хорошо и правильно. Если это правильно архитектурно, то давай сделаем». Captured as US-009 and FR-13. The old full-IR-scan code path for definition lookup is removed in this PRD's scope — no parallel data paths to drift later.

## Open Questions

- _(All five pre-kickoff decisions resolved in the Decisions section above.)_

### Deferred follow-ups from US-008 KSD-CodeReview pass

The KSD-CodeReview pass (Blind Hunter + Edge Case Hunter + Acceptance Auditor +
Comment Quality) surfaced the items below.  Each one is real but either
intentional, out of scope for this PRD, or low-risk enough to defer.  Logged
here so a future iteration can pick them up.

- **D1 — UNIQUE constraints on `symbol_definitions` / `symbol_imports`.**  V13
  intentionally has no UNIQUE/PRIMARY KEY constraints; idempotency relies on
  the `replace_file` DELETE-then-INSERT discipline inside a single transaction.
  A UNIQUE compound on `(branch_id, file_path, symbol_name, kind)` /
  `(branch_id, importer_file, imported_name)` would harden against
  out-of-band writers and accidental duplicate inserts but requires a
  schema-evolution PRD because the migration would need to dedupe any
  existing duplicates first.  Follow-up PRD if this ever bites.
- **D2 — Unicode case-folding mismatch.**  `symbol_definitions` is queried via
  `LOWER(symbol_name) LIKE ?`; SQLite's default `LOWER()` is ASCII-only while
  Rust's `to_lowercase` is Unicode-aware.  Non-ASCII identifiers (allowed in
  Python and TS) may miss; the keyword path defensively skips score-0 rows.
  Mitigation paths: install `NOCASE` collation on the column, or normalise
  both sides via the same Rust-side `normalize_name`.  Out of scope here —
  ASCII covers >99% of Rust/Python/TS identifiers in this codebase.
- **D3 — N+1 SQL queries in `enrich_with_dependent_files`.**  Single-pattern
  probes against the `(branch_id, imported_name)` index; bookkeeping for a
  batched `IN (...)` query exceeded the savings for typical query result
  sizes (single-digit to low tens of patterns).  Revisit if this ever shows
  up in a flame graph.
- **D4 — 200 ms wall-clock test budget.**  `lookup_time_bounded_with_1000_definitions`
  uses a 200 ms upper bound that may be flaky on contended CI runners.
  Replacing with a criterion micro-bench (gated behind `#[cfg(feature =
  "perf-tests")]`) would be cleaner but is overkill for the current AC.
- **D5 — `MAX_CALL_SITE_FILES_PER_PATTERN = 5` cap.**  The PRD's response-shape
  example does not mention a cap; the implementation truncates the top-N
  file aggregates with `total_call_sites` preserving the uncapped total.
  Intentional for response-size bounding.  A future PRD may want to expose
  this as a tool input parameter.
- **D6 — `SqliteSymbolIndexRepository::delete_branch` not transactional.**
  Test-only call site; the production branch-wipe path runs through
  `BranchRepository::delete_branch` which IS transactional and drops the
  symbol-index rows alongside `nodes`/`edges`/`files_ir`.  Hardening the
  symbol-index repo's own `delete_branch` to use a tx would be tidy but
  doesn't fix a real bug.
- **D7 — `create_snapshot` to an existing branch.**  Bulk-`INSERT INTO …
  SELECT …` assumes the target branch has zero rows.  If the target already
  has data, the copy duplicates rows (no UNIQUE; see D1).  Real-world
  callers always create-then-snapshot or delete-then-snapshot, but a
  defensive `DELETE FROM <table> WHERE branch_id = ?new` pre-step would be
  worth adding alongside D1.
- **D8 — Cross-platform path separators.**  IR stores paths via
  `path.to_string_lossy()`; on Windows this uses backslashes consistently
  across `symbol_definitions.file_path`, `symbol_imports.importer_file`,
  and `files_ir.file_path`, so the `importer_file != file_path` filter in
  `enrich_with_dependent_files` works.  Mixed-separator paths would
  silently break that filter — defensive normalisation could come from
  a shared helper if we ever ship Windows builds.
- **D9 — `incremental_sync_blocking` integration test gap.**  Auditor noted
  the wiring change (delete-with-symbol-index, upsert-with-symbol-index) is
  not exercised by an integration test — only unit tests at the storage and
  watcher layers cover the underlying code paths.  Adding an integration
  test that drives `incremental_sync_blocking` against a two-tree fixture
  and asserts symbol-index reflects new HEAD would close this.
- **D10 — Manual perf bench for "<50ms on ~2000 files".**  PRD explicitly
  marks this as "one-off, manual"; the companion AC for a bounded 1000-def
  unit test is satisfied (see D4).  A criterion micro-bench is the right
  long-term home if perf regresses.
- **D11 — Backfill error context.**  `backfill_symbol_index` errors don't
  identify which `(branch_id, file_path)` triggered the failure.  Wrap each
  per-row write in an attributed error message if a real failure surfaces.

### Accepted spec deviations

- **A1 — Backfill location.**  US-001 AC literally says "inside the
  migration", but refinery 0.9 SQL files cannot deserialize postcard blobs.
  The Rust-side backfill in `Database::open` (gated on "definitions empty")
  runs immediately after refinery applies V13 and is functionally
  equivalent for any code path that opens the DB.  Documented in US-001
  notes.

- **A2 — Language-aware definition snippets.**  Mid-stream addition during
  human review: the synthetic snippets produced by
  `seshat_core::symbol_snippet` previously always borrowed Rust syntax
  (`pub fn …`, `pub struct …`, `export …` regardless of source language),
  which read poorly on non-Rust projects and could mislead LLM agents
  scanning result lists.  Folded into this branch:
  - `function_definition_snippet` / `type_definition_snippet` /
    `export_definition_snippet` now take a `Language` argument and render
    per-language: `pub fn` for Rust, `export function` for TS/JS, `def`
    for Python.  Type-kind keywords also renamed (`typealias` → `type`)
    to use real source syntax.
  - Keyword helpers (`Language::visibility_keyword`,
    `Language::function_keyword`, `TypeDefKind::keyword`) live next to
    the enum definitions in `seshat-core::ir` so reader and writer share
    one source of truth.
  - `extract_definitions` (write path) and `build_ir_lookup` (vector
    fallback) both pass `file.language` through.
  - **DB-row migration:** existing rows in `symbol_definitions.snippet`
    keep their Rust-flavoured strings until the next watcher event or full
    re-scan re-writes them.  Pre-1.0 we accept this transient
    inconsistency rather than force a one-shot rewrite — matches the same
    trade-off documented for `symbol_imports` in Success Metrics.

## Verification Plan (end-to-end)

After all stories are landed:

1. From the worktree branch, `cargo install --path crates/seshat-bin --locked` to install the new binary.
2. Restart MCP serve against the seshat repo.
3. From a separate Claude session:
   - `query_code_pattern("BranchId")` → expect `dependent_files ≥ 3`, `blast_radius = medium`, aggregated `call_sites` ≤ 5 entries.
   - `query_code_pattern("show_summary")` → private fn → expect `dependent_files = []`, `blast_radius = low`.
   - `query_code_pattern("record_decision")` → expect `dependent_files` includes test files + handler.
4. Edit a file that imports `BranchId`, save, wait ~2s for watcher; re-run `query_code_pattern("BranchId")` → `dependent_files` reflects the edit.
5. Delete a file that imports `BranchId`, re-run → `dependent_files` shrinks by 1.
