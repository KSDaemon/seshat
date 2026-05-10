---
title: Transitive Dependents & Content-Level Diff Analysis
type: Feature
status: planned
created: 2026-05-10
author: ksdaemon
roadmap_tags: [transitive-deps, content-diff]
related_prds:
  - .ralph/tasks/prd-advanced-mcp-tools.md  # original query_dependencies (V1, direct only)
  - .ralph/tasks/prd-map-diff-impact.md     # original map_diff_impact (file-level granularity)
---

# PRD: Transitive Dependents & Content-Level Diff Analysis

## 1. Introduction / Overview

**Type:** Feature

Two sequential mid-term roadmap items, delivered together because the second builds on the first:

- **Epic A — Transitive Dependents.** `query_dependencies` today returns only direct (1st-order) dependents of a file. AI agents using Seshat for blast-radius analysis miss the actual ripple — a "low blast radius" file may have hundreds of indirect dependents through a chain of imports. Add transitive BFS with depth=3 default (per `_bmad-output/planning-artifacts/roadmap.md:85` "2nd/3rd order"), cycle-safe, with on-the-fly reverse-adjacency (no schema changes).

- **Epic B — Content-Level Diff Analysis.** `map_diff_impact` today reports affected symbols at file granularity: any line change in `foo.rs` flags ALL public symbols. Add hunk-level parsing via `gix::diff::blob` (already a transitive dep of `gix 0.72`, feature `blob-diff`), intersect hunks with IR symbol line ranges, and report only the symbols whose ranges actually overlap. Concurrently, switch `compute_affected_symbols` to use Epic A's transitive blast radius for richer reporting.

Both epics ship pre-1.0 — no backward-compat shims, IR fields and response structs broken/renamed directly. Each epic delivered on its own git worktree (`feat/transitive-deps`, `feat/content-diff`).

## 2. Goals

- AI agents see realistic blast radius (direct + transitive) when querying dependencies for a file, with explicit depth control and cycle protection.
- AI agents see precise per-symbol impact when calling `map_diff_impact`: only symbols whose line ranges intersect changed hunks are flagged, not the whole file.
- New transitive entries carry a `via` path showing the chain through which they reach the target.
- MCP P95 latency target of 1s is preserved on a 3000-file repo for `query_dependencies` and on a 50-changed-file diff for `map_diff_impact`.
- Zero schema migrations (Epic A); IR version bump only (Epic B), auto-invalidated by existing `StaleIR` detection.

## 3. User Stories

### Epic A — Transitive Dependents

#### US-A1: Reverse-adjacency builder + cycle-safe BFS core

**Description:** As a graph-layer caller, I want a reusable BFS that returns dependents up to depth=N over a one-time reverse-adjacency map, so that all callers (sync, batch, future tools) share the same semantics and perf profile.

**Acceptance Criteria:**
- [ ] New `compute_transitive_dependents(target, reverse, depth) -> TransitiveResult` in `crates/seshat-graph/src/dependencies.rs`.
- [ ] New `build_reverse_adjacency(files, internal_names, suffix_index) -> HashMap<String, Vec<ReverseEdge>>` — single O(N×D) pass.
- [ ] BFS uses `visited: HashSet<String>` keyed by file path; cycles `a→b→a` terminate without re-enqueue.
- [ ] Self-edges skipped at edge-build time.
- [ ] `MAX_DEPENDENTS = 500` cap; on overflow `truncated = true`. Direct entries enumerated first → preserved across capping.
- [ ] Reverse-edge keys go through the same import resolver as forward path (`import_resolves_to_target` at `dependencies.rs:703-792`) — extract shared helper `resolve_import_to_known_path`.
- [ ] Unit tests pass: `transitive_depth_2_includes_2nd_order`, `transitive_depth_3_includes_3rd_order`, `transitive_cycle_a_b_a_terminates`, `transitive_diamond_visits_each_node_once`, `transitive_truncation_caps_at_max_dependents`.
- [ ] `cargo clippy --all-targets -- -D warnings` passes.

#### US-A2: Public `query_dependencies` API accepts depth

**Description:** As a graph-layer caller, I want to opt into transitive results via a `depth` parameter so existing callsites can preserve direct-only semantics by passing `depth=1`.

**Acceptance Criteria:**
- [ ] New `pub struct QueryDependenciesOptions { pub depth: u32 }`; `Default::default()` returns `depth = 1`.
- [ ] `query_dependencies` and `query_dependencies_batch` signatures gain trailing `opts: QueryDependenciesOptions`.
- [ ] `query_dependencies_batch` builds reverse-adjacency once across the whole IR (amortized across all targets).
- [ ] `DependentEntry` extended: `depth: u32`, `via: Vec<String>` (full file paths, intermediate only); `import_names` and `line` populated only when `depth == 1`.
- [ ] `DependencyData` extended: `transitive_dependent_count: usize`, `requested_depth: u32`.
- [ ] `blast_radius` computed from direct count only (semantic stability).
- [ ] Validation: `depth ∉ [1, MAX_TRANSITIVE_DEPTH=10]` → `GraphError::InvalidInput`.
- [ ] Diamond `via` tie-break: lexicographic on joined path string (deterministic across runs).
- [ ] All existing callsites in `validate_approach.rs` and `diff_impact.rs:378` updated to pass `QueryDependenciesOptions { depth: 1 }`.
- [ ] All existing tests pass with no semantic regression.

#### US-A3: MCP request surface

**Description:** As an AI agent, I want to request transitive depth via the MCP tool input so I can see 2nd/3rd-order ripple without a separate tool.

**Acceptance Criteria:**
- [ ] `QueryDependenciesRequest` in `crates/seshat-mcp/src/tools/query_dependencies.rs:17-51` gains `pub depth: Option<u32>` with `#[schemars(description = ...)]`.
- [ ] Handler maps `req.depth.unwrap_or(DEFAULT_TRANSITIVE_DEPTH=3)` → `QueryDependenciesOptions`.
- [ ] Validation: `depth=0` or `depth>10` returns `INVALID_INPUT` with suggestion `"Use depth between 1 and 10"`.
- [ ] Tool description in `crates/seshat-mcp/src/server.rs:545` updated to mention `depth`, default, and max.
- [ ] Response JSON includes `data.transitive_dependent_count`, `data.requested_depth`, and per-entry `depth`/`via`/`import_names`/`line` fields.
- [ ] Integration tests pass: `query_dependencies_tool_default_depth_returns_transitive`, `..._depth_one_returns_direct_only`, `..._depth_zero_returns_invalid_input`, `..._depth_above_max_returns_invalid_input`.

#### US-A4: Call-logger summary

**Description:** As an operator reading call logs, I want the transitive count and effective depth surfaced in the per-call summary so I can debug expensive queries.

**Acceptance Criteria:**
- [ ] `crates/seshat-mcp/src/call_logger_keys.rs::dependencies` adds `DATA_TRANSITIVE_DEPENDENT_COUNT` and `DATA_REQUESTED_DEPTH` constants.
- [ ] `dependencies_result()` in `call_logger.rs` includes both new fields in the summary JSON.
- [ ] Existing call-logger tests updated.

#### US-A5: Performance guard

**Description:** As a maintainer, I want an explicit perf budget test so the 1s P95 target is verifiable.

**Acceptance Criteria:**
- [ ] New `crates/seshat-graph/tests/transitive_perf.rs` (`#[ignore]` by default).
- [ ] 3000 synthetic IR files in a 3-level fan-out tree; `query_dependencies` at depth=3.
- [ ] Asserts wall-clock < 500 ms on dev hardware (2× headroom on 1s MCP P95).
- [ ] Runnable via `cargo test -p seshat-graph --test transitive_perf -- --ignored --nocapture`.

### Epic B — Content-Level Diff Analysis

#### US-B1: IR `end_line` for `Export` & `TypeDef`

**Description:** As an IR consumer, I want every public symbol to carry both `line` and `end_line` so hunk intersection can use the same logic across all symbol kinds.

**Acceptance Criteria:**
- [ ] Add `pub end_line: usize` to `Export` and `TypeDef` in `crates/seshat-core/src/ir.rs:126-172`.
- [ ] `IR_SCHEMA_VERSION` in `crates/seshat-storage/src/ir_serialization.rs:29` bumped from `7` to `8`. Stale-IR detection auto-triggers re-scan; no migration script.
- [ ] All construction sites in `crates/seshat-scanner/src/parser/{rust,typescript,javascript,python}_parser.rs` populate `end_line` via `node.end_position().row + 1` (same pattern as `Function`).
- [ ] For single-line statements (`pub use foo::*;`, `export { Foo };`, `type Alias = X;`), `end_line == line`.
- [ ] Round-trip serialization tests updated; fresh scan produces schema version 8.

#### US-B2: Hunk extraction primitive

**Description:** As a diff consumer, I want a self-contained `Vec<Hunk>` for any pair of file blobs, using the algorithm git uses by default.

**Acceptance Criteria:**
- [ ] New types `LineRange { start, end }` (1-based half-open) and `Hunk { old: LineRange, new: LineRange }` in `crates/seshat-graph/src/diff_impact.rs`.
- [ ] `Hunk::ALL` constant covering an entire file (binary/oversized fallback).
- [ ] Helpers: `is_pure_insertion`, `is_pure_deletion`, `touches_new_line(line)`.
- [ ] `pub fn diff_blobs_to_hunks(old: &[u8], new: &[u8]) -> Vec<Hunk>` using `gix::diff::blob` (Histogram algorithm, git's default since 2.31).
- [ ] 8 unit tests pass: no-change, single-replacement, pure-insertion (top/bottom/middle), pure-deletion, multi-hunk, empty-old, empty-new.

#### US-B3: Blob-aware change enumeration

**Description:** As `compute_affected_symbols`, I want each changed file accompanied by its `base_blob_id` and `index_blob_id` so I can read both sides for diffing.

**Acceptance Criteria:**
- [ ] New struct `ChangedFileWithBlobs { path, status, base_blob_id: Option<gix::ObjectId>, index_blob_id: Option<gix::ObjectId> }`.
- [ ] Refactor `get_changed_files()` (currently `crates/seshat-graph/src/diff_impact.rs:183-348`) to delegate to `enumerate_changes_with_blobs()`; old function becomes a thin wrapper if still needed.
- [ ] New `read_blob_pair(repo, repo_path, rel_path, base_blob_id, index_blob_id, staged_only) -> Result<Option<(Vec<u8>, Vec<u8>)>>`. Returns `None` for binary or oversized → caller falls back to `Hunk::ALL`.
- [ ] `is_binary_blob` heuristic: NUL byte in first 8 KiB.
- [ ] `MAX_DIFF_FILE_SIZE = 5 * 1024 * 1024` cap.
- [ ] Unit tests: staged-Deleted (no `index_blob_id`), Added (no `base_blob_id`), binary returns `None`.

#### US-B4: Hunk-level `compute_affected_symbols`

**Description:** As an AI agent calling `map_diff_impact`, I want only the symbols whose line ranges actually intersect changed hunks — not every public symbol in the file.

**Acceptance Criteria:**
- [ ] `crates/seshat-graph/src/diff_impact.rs:355-451` rewritten: per-changed-file, get hunks via `read_blob_pair` + `diff_blobs_to_hunks`, then `symbol_intersects_hunks(line, end_line, hunks) -> Vec<Hunk>`.
- [ ] `AffectedSymbol` extended (BREAKING — pre-1.0): `changed_lines: Vec<(usize, usize)>` (intersecting hunk ranges), `direct_dependent_count: usize`. `dependent_count` semantically becomes transitive count.
- [ ] `query_dependencies_batch` callsite at line 378 switches to `QueryDependenciesOptions { depth: DEFAULT_TRANSITIVE_DEPTH }` (Epic A).
- [ ] `blast_radius` re-classified using new transitive count.
- [ ] **Modified files**: hunks computed against HEAD blob (or index blob if `staged_only`, or base blob if `base` param given).
- [ ] **Added files**: every symbol affected; `changed_lines = [(line, end_line)]` per symbol.
- [ ] **Deleted files**: file reported in `changed_files`; no symbol granularity (IR removed; old-IR loading is V1 limitation).
- [ ] **Untracked / Conflicted / binary / oversized**: fallback to `Hunk::ALL` (preserves current behavior).
- [ ] Integration tests pass: `single_hunk_in_function_body_flags_only_that_function`, `multi_hunk_flags_each_intersecting_symbol`, `hunk_between_symbols_flags_neither`, `added_file_flags_all_symbols_with_their_ranges`, `deleted_file_reports_no_symbols_only_status`, `binary_modified_file_falls_back_to_hunk_all`, `transitive_dependent_count_uses_depth_3`.

#### US-B5: MCP wiring & call-logger

**Description:** As an AI agent reading the `map_diff_impact` response, I want `next_steps` and call logs to surface the new content-level granularity.

**Acceptance Criteria:**
- [ ] `crates/seshat-mcp/src/tools/diff_impact.rs:97-176` `generate_next_steps` mentions `changed_lines` and shows `direct_dependent_count` separately from transitive (e.g. `"foo touched at lines 42-58 with 12 transitive (4 direct) dependents in ..."`).
- [ ] `crates/seshat-mcp/src/call_logger.rs::diff_impact_result` adds `total_hunks` to the summary.
- [ ] Existing `next_steps_*` MCP-handler tests updated for new field defaults.

#### US-B6: Performance bench

**Description:** As a maintainer, I want a perf bench locking the 1s P95 budget on a representative diff.

**Acceptance Criteria:**
- [ ] New `crates/seshat-graph/benches/diff_impact_bench.rs` (criterion).
- [ ] 50-file synthetic diff, mix of single-hunk and multi-hunk modifications.
- [ ] Asserts (or CI gate) median wall-clock < 1000 ms on dev hardware.

## 4. Functional Requirements

### Epic A

- **FR-A1:** `query_dependencies` accepts a `depth: Option<u32>` parameter; MCP default = 3, graph-layer `Default` = 1, accepted range `[1, 10]`.
- **FR-A2:** Each `DependentEntry` carries its `depth` (1 = direct, 2..=10 = transitive) and `via` (full file paths of intermediate hops, lex-sorted on tie-break).
- **FR-A3:** Direct entries (`depth == 1`) carry `import_names` and `line`; transitive entries leave both empty (`vec![]`, `0`).
- **FR-A4:** BFS is cycle-safe via `visited: HashSet<String>`; each file appears at most once in the result.
- **FR-A5:** Total result is capped at `MAX_DEPENDENTS = 500`; on overflow `DependencyData.truncated = true`. Direct entries are enumerated before transitive → never sacrificed by capping.
- **FR-A6:** `DependencyData.blast_radius` is computed solely from direct count (preserves existing thresholds at `dependencies.rs:21-22, 832-839`).
- **FR-A7:** `DependencyData.transitive_dependent_count` exposes the transitive-only count separately.
- **FR-A8:** `query_dependencies_batch` builds the reverse-adjacency map exactly once across all targets in the batch.
- **FR-A9:** Reverse-adjacency edge keys are produced by the same import resolver used in the forward path — cross-crate guard (`dependencies.rs:770ff`) honored on transitive paths.
- **FR-A10:** `depth=0` or `depth>MAX_TRANSITIVE_DEPTH` returns `INVALID_INPUT` from the MCP layer.

### Epic B

- **FR-B1:** `Export` and `TypeDef` IR structs carry `end_line: usize` populated by all four parsers (`rust_parser.rs`, `typescript_parser.rs`, `javascript_parser.rs`, `python_parser.rs`).
- **FR-B2:** `IR_SCHEMA_VERSION` bumped to 8; existing `StaleIR` detection auto-invalidates older IR on next read.
- **FR-B3:** For each `Modified` file with text content under `MAX_DIFF_FILE_SIZE`, hunks are computed via `gix::diff::blob` (Histogram).
- **FR-B4:** A symbol is included in `affected_symbols` iff its `[line, end_line]` range intersects the `new` range of at least one hunk.
- **FR-B5:** `AffectedSymbol.changed_lines` lists the `(start, end)` of every intersecting hunk.
- **FR-B6:** Binary, oversized, conflicted, and untracked files fall back to `Hunk::ALL` (every public symbol affected — preserves current semantics).
- **FR-B7:** Added files mark every symbol affected; `changed_lines = [(symbol.line, symbol.end_line)]` per symbol.
- **FR-B8:** Deleted files appear in `changed_files` with no associated affected_symbols entries.
- **FR-B9:** `compute_affected_symbols` calls `query_dependencies_batch` with `QueryDependenciesOptions { depth: DEFAULT_TRANSITIVE_DEPTH }`.
- **FR-B10:** `AffectedSymbol.dependent_count` is the transitive count; `direct_dependent_count` exposes the depth-1 count separately. `blast_radius` is re-classified using the transitive count.

## 5. Non-Goals (Out of Scope)

- **No precomputed dependency-edges table.** Both epics compute on-the-fly from IR; no SQLite migrations, no scan-time edge writes. Per `prd-advanced-mcp-tools.md:211`. Revisit if perf budget breaks.
- **No old-IR loading for renamed/deleted symbols within Modified files.** If a function is renamed in the same diff, only the new name appears. Documented limitation; revisit in a separate epic.
- **No convention-risk filtering by hunk overlap.** `compute_convention_risks` (`diff_impact.rs:520-659`) keeps file-level granularity in this PRD; defer to a future epic.
- **No new external diff parser dependency.** Use `gix::diff::blob` only — `imara-diff` is not added as a top-level dep (it's a transitive of gix).
- **No depth-aware blast-radius thresholds.** Keep current 3/10 thresholds applied to direct count (Epic A) or transitive count (Epic B). No new thresholds, no per-depth weighting.
- **No backward-compat shims for renamed struct fields.** Pre-1.0; rename and break (per `feedback_no_backward_compat.md`).
- **No `seshat review` UI changes.** TUI consumes the same data; new fields are additive in JSON shape, breaking only when consumed strictly.
- **No daemon-mode / cross-scope querying.** Out of scope for both epics.

## 6. Design Considerations

### Branching & worktrees

Per `feedback_worktree_for_parallel_work.md`:

- Epic A: branch `feat/transitive-deps` on worktree `../seshat-transitive-deps`.
- Epic B: branch `feat/content-diff` on worktree `../seshat-content-diff`, branched from `main` AFTER Epic A merges.

### `via` representation (resolved decision)

- Full file paths relative to project root (e.g. `crates/seshat-graph/src/diff_impact.rs`). Unambiguous and IDE-friendly.
- Diamond tie-break: lexicographic on the joined `via` string. Deterministic across runs, independent of IR scan order.

### `QueryDependenciesOptions::default()` (resolved decision)

- Returns `depth = 1`. Internal callers (`validate_approach`, `diff_impact` before Epic B is merged) get safe direct-only semantics. MCP layer explicitly overrides to `DEFAULT_TRANSITIVE_DEPTH = 3`.
- After Epic B merges, `diff_impact::compute_affected_symbols` is the only internal caller that intentionally passes `depth: DEFAULT_TRANSITIVE_DEPTH`.

### Hunk algorithm choice

`gix::diff::blob` Histogram is git's default since 2.31 — matches what users see when they run `git diff` themselves, removing surprise.

### Doc hygiene (post-ship)

- After Epic A: in `_bmad-output/planning-artifacts/roadmap.md` move "Transitive Dependents" from Mid-Term to Done; in `.ralph/tasks/prd-advanced-mcp-tools.md:209` strike "No transitive dependency analysis" from Non-Goals.
- After Epic B: same for "Content-Level Diff Analysis"; remove `prd-map-diff-impact.md:179` non-goal and `:317` open question.

## 7. Technical Considerations

### Critical files

| File | Epic | Changes |
|---|---|---|
| `crates/seshat-graph/src/dependencies.rs` | A | new `compute_transitive_dependents`, `build_reverse_adjacency`, `QueryDependenciesOptions`; extend `DependentEntry`/`DependencyData`; refactor `import_resolves_to_target` → `resolve_import_to_known_path` |
| `crates/seshat-graph/src/validate_approach.rs` | A | callsites pass `QueryDependenciesOptions { depth: 1 }` |
| `crates/seshat-graph/src/diff_impact.rs` | A → B | A: `depth: 1` at line 378. B: new types `LineRange`, `Hunk`, `ChangedFileWithBlobs`; rewrite `compute_affected_symbols`; switch to `depth: DEFAULT_TRANSITIVE_DEPTH` |
| `crates/seshat-graph/src/lib.rs` | A, B | re-exports |
| `crates/seshat-mcp/src/tools/query_dependencies.rs` | A | `depth` field, validation |
| `crates/seshat-mcp/src/tools/diff_impact.rs` | B | next_steps wording |
| `crates/seshat-mcp/src/server.rs` | A | tool description, integration tests |
| `crates/seshat-mcp/src/call_logger.rs`, `call_logger_keys.rs` | A, B | summary fields |
| `crates/seshat-core/src/ir.rs:126-172` | B | `end_line` on `Export`, `TypeDef` |
| `crates/seshat-storage/src/ir_serialization.rs:29` | B | `IR_SCHEMA_VERSION = 8` |
| `crates/seshat-scanner/src/parser/{rust,typescript,javascript,python}_parser.rs` | B | populate `end_line` for Export/TypeDef |
| `crates/seshat-graph/tests/transitive_perf.rs` (new) | A | gated perf test |
| `crates/seshat-graph/benches/diff_impact_bench.rs` (new) | B | criterion bench |

### Dependencies

- `gix = "0.72"` already in `Cargo.toml:89` with `blob-diff` feature — Epic B uses existing surface, no new deps.
- No SQLite migrations.
- IR version bump only (Epic B); existing `StaleIR` deserializer (`ir_serialization.rs:55-58`) handles re-scan trigger automatically.

### Performance budget

- `query_dependencies` at depth=3 on 3000 files: < 500 ms wall-clock (gated test in `transitive_perf.rs`).
- `map_diff_impact` on a 50-file diff with hunk parsing: median < 1000 ms (criterion bench in `diff_impact_bench.rs`).

### Test fixture reuse

- Multi-file chain at `dependencies.rs:892-973` (`app → user_service → [user, utils]`) is ideal for transitive depth-2 tests.
- Test setup pattern in `seshat-mcp/src/tools/diff_impact.rs:178-214` (tempdir + `git init`) reused for content-diff integration tests.

## 8. Success Metrics

- All 11 user stories (US-A1..A5, US-B1..B6) ship with ACs satisfied; CI green.
- For Seshat itself: `query_dependencies path="crates/seshat-graph/src/dependencies.rs" depth=3` returns transitive entries reaching at least `seshat-mcp/src/server.rs` (via `diff_impact.rs` or `query_dependencies.rs`).
- For Seshat itself: editing a single function in any multi-symbol file produces an `affected_symbols` list containing only that function (not every public symbol in the file).
- Perf budget tests pass on dev hardware with at least 2× headroom on the 1s MCP P95 target.
- Zero regressions in existing `seshat-graph` and `seshat-mcp` test suites after callsite updates.
- After ship: roadmap entries `#transitive-deps` and `#content-diff` moved from Mid-Term to Done section.

## 9. Open Questions

All design questions resolved before kickoff:

| # | Question | Decision |
|---|---|---|
| 1 | What does `DependentEntry.via` contain? | Full file paths relative to project root |
| 2 | How to break ties between equally-short paths to the same dependent in diamond graphs? | Lexicographic on joined `via` string |
| 3 | What does `QueryDependenciesOptions::default()` return? | `depth = 1` (graph-layer safe default; MCP overrides to 3) |

Items deferred to follow-up PRDs (deliberately out of scope here):

- Old-IR loading for tracking renamed/deleted symbols within a single diff.
- Convention-risk filtering by hunk overlap.
- Pre-computed dependency-edges table (revisit if perf breaks).
