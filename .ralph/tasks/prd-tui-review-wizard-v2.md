# PRD: TUI Review Wizard — Layout, Dedup, Summary, Stability

## Introduction

**Type:** Fix

Comprehensive fix for the `seshat review` TUI review wizard covering:

1. **UI Layout** — matches PRD design spec: single outer border, proper dividers, single-line bottom bar, expandable example section
2. **Convention Dedup** — when user confirms a convention, subsequent scans MUST NOT recreate it. Description hash dedup ensures one convention = one visible node in FTS5
3. **Summary** — meaningful stats: total, already-confirmed, pending, precision. Printed exactly once
4. **Stability** — no hang on exit, non-blocking event loop, consistent branch ID
5. **Comprehensive Tests** — unit tests for all paths, integration tests for scan→confirm→rescan cycle

## Goals

- **Fix UI layout** — single outer cyan border, `├───` dividers, single-line bottom bar, expandable example
- **Fix convention dedup** — confirm + scan → NO duplicate in FTS5. Description hash links user node to auto-detected node
- **Fix summary** — total/confirmed/rejected/partial/skipped/pending/already-confirmed. Printed once
- **Fix save hang** — match `event::read()` on `Event::Key`, skip non-key events
- **Fix branch mismatch** — `query_conventions_for_review` returns branch_id, same passed to `apply_review_actions`

## User Stories

### US-001: TUI layout matches PRD design spec

**Description:** As a developer, I want the review TUI to visually match the design spec with single outer border, proper dividers, and compact bottom bar.

**Target layout (120x30 terminal):**
```
┌─ Seshat Convention Review ──────────────────────────────────────────────────────────────────┐
│   1/53: Import grouping: stdlib → external → internal                                       │
├─────────────────────────────────────────────────────────────────────────────────────────────┤
│  Nature: Convention       Confidence: 100%       Weight: Strong                             │
│  Found in: 4/4 files (100% adoption)                                                        │
├── Example: (…/crates/seshat-cli/src/lib.rs:44) ─────────────────────────────────────────────┤
│  44  pub use args::{Cli, Command};                                                          │
│  45  pub use db::{find_git_root, get_current_branch};                                       │
│  46  pub use error::CliError;                                                               │
│                                                                                             │
├─────────────────────────────────────────────────────────────────────────────────────────────┤
│ [y] Confirm    [n] Reject    [p] Partial    [s] Skip    [↑↓/jk] Navigate    [q/Esc] Finish  │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Acceptance Criteria:**
- [ ] Single outer cyan border (`Borders::ALL`), no nested borders
- [ ] Divider lines between sections use `├─── ... ─┤` pattern (`Borders::LEFT | Borders::TOP | Borders::RIGHT`)
- [ ] Info section: Nature/Confidence/Weight on one line, adoption on second line
- [ ] Example section divider uses `├──` (LEFT only + TOP + RIGHT, connects to outer border), with `title` showing filename:line
- [ ] Bottom bar is a SINGLE LINE (not 3 rows): `[y] Confirm    [n] Reject    [p] Partial    [s] Skip    [↑↓/jk] Navigate    [q/Esc] Finish`
- [ ] Example code block fills ALL remaining vertical space (not fixed 5 rows)
- [ ] Code lines truncated to fit within border width (no overflow, no wrapping)
- [ ] When no examples: example section hidden (no "(no examples)" text filling screen)
- [ ] `cargo check -p seshat-cli` compiles with 0 errors, 0 warnings

### US-002: Review only unreviewed conventions (dedup)

**Description:** As a developer, I want `seshat review` to show only conventions I have NOT yet acted on, so that the list shrinks with each session and I never see duplicates of confirmed conventions after a scan.

**Current bug:** User confirms 6 of 54 → new user node created, original auto-detected untouched → scan deletes old auto-detected + inserts new → now 60 conventions (54 original + 6 confirmed as new auto-detected)

**Root cause:** `record_decision` creates a separate user node. The auto-detected node is NOT modified. On next scan, auto-detected nodes are replaced (DELETE+INSERT), and the confirmed convention gets re-inserted as a new auto-detected node. FT5 indexes both.

**Acceptance Criteria:**
- [ ] `query_conventions_for_review` excludes auto-detected nodes whose `description_hash` matches any user node's `description_hash` (already confirmed)
- [ ] `confirm_convention` writes `description_hash` to the user node's `description_hash` column
- [ ] `persist_conventions` (scan INSERT phase) skips insertion if a user node with matching `description_hash` exists
- [ ] `rebuild_fts_index` only indexes one node per `description_hash` (user node takes priority over auto-detected)
- [ ] After confirm→scan→review cycle: confirmed convention NOT in review list
- [ ] After confirm→scan: MCP tools return only the user node (not both user + auto-detected)
- [ ] Description hash is stable: normalize (lowercase, trim, collapse whitespace) before hashing

### US-003: Summary shows full picture (total, pending, already-confirmed)

**Description:** As a developer, I want the post-review summary to show total conventions, already-confirmed count, pending count, and meaningful precision — even if I exit without actions.

**Before (broken):**
```
   -- Review Complete -----------------------------------------------
      + Confirmed     6    (← only session count)
      - Rejected      0
      ~ Partial       0
      x Skipped       0
     Precision: 100%     (← 6/6, but 48 were never touched)
     Knowledge graph updated.
```

**After (correct):**
```
   -- Review Complete -----------------------------------------------

     Conventions in scope: 53
      + Confirmed this session:   6
      - Rejected this session:    0
      ~ Partial this session:     0
      x Skipped this session:    47
      Already confirmed (DB):    18
       Still pending:             29

     Session precision: 100%   (6 confirmed / 6 decided)
     Overall coverage:  19%   (6 confirmed / 53 in scope)

     Knowledge graph updated.
```

**Acceptance Criteria:**
- [ ] `show_summary` accepts `SummaryContext { total_in_scope: usize, already_confirmed: usize }`
- [ ] "Conventions in scope" = total conventions returned by query (excludes already-confirmed)
- [ ] "Already confirmed" = COUNT of user-sourced convention/observation nodes on current branch, not yet removed
- [ ] "Still pending" = total_in_scope - (confirmed + rejected + partial + skipped)
- [ ] "Session precision" = confirmed / max(confirmed + rejected + partial, 1) * 100
- [ ] "Overall coverage" = (already_confirmed + session_confirmed) / (total_in_scope + already_confirmed) * 100
- [ ] Summary printed exactly ONCE after `ratatui::restore()`
- [ ] "Knowledge graph updated." shown only if there were actual DB actions
- [ ] When no actions (user presses q immediately): all session counts = 0, pending = total_in_scope

### US-004: TUI exits immediately, no hang

**Description):** As a developer, I want the TUI to exit in under 200ms when I press `q`, without any delay or terminal corruption.

**Acceptance Criteria:**
- [ ] `event::read()` result is matched on `Event::Key` — non-key events are skipped
- [ ] If `read()` returns `Err` or non-key event, loop continues to next iteration
- [ ] Pressing `q` or `Esc` or `Ctrl+C` exits within 200ms
- [ ] No terminal control characters remain after exit
- [ ] No zombie threads or resource leaks

### US-005: Consistent branch ID across query and apply

**Description:** As a developer on a feature branch, I want my review actions applied to the same branch that was queried.

**Current bug:** `query_conventions_for_review` correctly determines branch from git. But `run_review_tui` uses `get_current_branch(...).unwrap_or_else(|| "main")` — if git branch detection fails mid-session, actions apply to "main" instead of the queried branch.

**Acceptance Criteria:**
- [ ] `query_conventions_for_review` returns `(Vec<ConventionItem>, String)` — both items AND branch_id
- [ ] `run_review_tui` and `run_review_tui_with_conn` use the branch_id returned by the query
- [ ] No `"main"` fallback — if branch cannot be determined, return error before initializing TUI

### US-006: Snapshot hash validates reject concurrency

**Description:** As a developer, I want rejected conventions to be verified with snapshot hash to prevent applying rejects to modified nodes.

**Acceptance Criteria:**
- [ ] `reject_convention` computes `compute_snapshot_hash(&ext_data)` and compares against `expected_hash`
- [ ] If hashes don't match: returns error "convention was modified during review; please retry"
- [ ] `json_extract` returning NULL (no `$.source` key) is handled — defaults to `"auto_detected"`

## Functional Requirements

### FR-1: Single outer border layout

Single `Block::default().borders(Borders::ALL)` cyan container. Internal dividers are `Block::default().borders(Borders::LEFT | Borders::TOP | Borders::RIGHT)` — renders as `├─── ... ─┤`.

### FR-2: Single-line bottom bar

One row: `[y] Confirm    [n] Reject    [p] Partial    [s] Skip    [↑↓/jk] Navigate    [q/Esc] Finish`. Truncated with `...` on narrow terminals.

### FR-3: Expandable example section

`Constraint::Min(3)` — example takes ALL remaining vertical space. `Constraint::Length(0)` when no examples.

### FR-4: Description hash dedup

**Hash function:** `description_hash = sha256(normalize(description)).hex()[0..16]`

**Normalize:** lowercase, trim, collapse internal whitespace, strip leading/trailing punctuation

**Schema migration:** `ALTER TABLE nodes ADD COLUMN description_hash TEXT DEFAULT NULL`

**Scan INSERT (persist_conventions):**
```sql
SELECT 1 FROM nodes WHERE description_hash = ? AND json_extract(ext_data, '$.source') = 'user' LIMIT 1
```
If row exists → skip auto-detected insert (user decision is authoritative)

**Confirm (record_decision):**
```sql
-- Insert user node with description_hash = hash(normalize(description))
INSERT INTO nodes (..., description_hash) VALUES (..., ?7)
```

**FTS5 (rebuild_fts_index):**
```sql
DELETE FROM conventions_fts;
INSERT INTO conventions_fts (description, node_id, detector_name)
SELECT n.description, CAST(n.id AS TEXT), COALESCE(json_extract(n.ext_data, '$.detector_name'), '')
FROM nodes n
WHERE n.description_hash IS NOT NULL
  AND (n.id NOT IN (
      -- Exclude auto-detected nodes that have a user counterpart
      SELECT an.id FROM nodes an
      WHERE json_extract(an.ext_data, '$.source') = 'auto_detected'
        AND an.description_hash IN (
            SELECT un.description_hash FROM nodes un
            WHERE json_extract(un.ext_data, '$.source') = 'user'
        )
  ) OR json_extract(n.ext_data, '$.source') = 'user')
  AND {sql_not_removed}
```

### FR-5: Rich summary with context

`show_summary` signature:
```rust
pub fn show_summary(results: &[ReviewAction], context: &SummaryContext)
```

```rust
pub struct SummaryContext {
    pub total_in_scope: usize,      // len(conventions) from query
    pub already_confirmed: usize,   // COUNT user-sourced on branch
}
```

### FR-6: Non-blocking event loop

```rust
if event::poll(50ms)? {
    match event::read() {
        Ok(Event::Key(k)) => ...,
        Ok(_) => {},  // resize/mouse — skip
        Err(_) => {}, // read failed — skip
    }
}
```

### FR-7: Branch ID from query, not from git at apply time

```rust
// In mod.rs:
let (conventions, branch_id) = app::query_conventions_for_review(db_path, git_root)?;
// branch_id is FROM the query, not re-queried from git
review_wizard::run_app(&mut terminal, conventions, &conn, &branch_id)
```

## Non-Goals (Out of Scope)

- No scrolling for oversized snippets (future)
- No terminal resize detection (future)
- No color theme customization
- No search/filter in TUI
- No "Show All" mode for viewing previously-confirmed conventions (future: `--show-all` flag)
- No partial description matching (exact hash match only)

## Design Considerations

### Layout Structure

```
+--- outer Block: Borders::ALL, cyan title ----+
| Row 0: title " Seshat Convention Review  1/N "-|
| Row 1: "   1/N: description..."                |
| Divider: Block LEFT|TOP|RIGHT (no title)       |
| Info: "Nature: X  Confidence: Y  Weight: Z"     |
| Info: "Found in: A/B files (C% adoption)"        |
| Divider: Block LEFT|TOP|RIGHT with title         |
| Example: " Example: (file:line) "               |
| ... code lines (fills remaining space)          |
| Divider: Block LEFT|TOP|RIGHT (no title)        |
| Bottom: " [y] Confirm [n] Reject ... "         |
+------------------------------------------------+
```

### Description Hash Behavior

| Scenario | What happens |
|----------|-------------|
| User confirms "snake_case for functions" | User node gets `description_hash=abc123...` |
| Scan runs | `persist_conventions` checks: user node with hash `abc123...` exists → **skips** auto-detected insert |
| Re-review | Query excludes auto-detected with hash matching user node → confirmed not in list |
| MCP query | FTS5 only indexes user node (auto-detected excluded) → **one result** |
| Description drift (detector changes text) | Hash changes → new auto-detected IS inserted. Uncommon. Accepted risk. |

### Color Scheme

- **Outer border**: Cyan
- **Metadata**: Nature=Green, Confidence=Yellow, Weight=Magenta
- **Example border**: Yellow title, DarkGray border
- **Code**: Green+Bold for highlighted lines, Yellow for non-highlighted
- **Bottom bar**: DarkGray border

## Technical Considerations

### Migration

```sql
-- V5__add_description_hash.sql
ALTER TABLE nodes ADD COLUMN description_hash TEXT DEFAULT NULL;
CREATE INDEX idx_nodes_description_hash ON nodes(description_hash);
```

### Backward Compatibility

- Existing nodes without `description_hash` (NULL) are unaffected
- Old confirmed nodes: no hash → scan will re-insert auto-detected. Once re-confirmed, hash is set
- FTS5 query works with NULL description_hash (they just don't get deduped)

### Event Loop Fix (save hang root cause)

The bug was `event::read()` blocking when `poll()` returned true for a non-key event (resize, mouse). `read()` would block waiting for the NEXT event. Fix: `match event::read() { Ok(Event::Key) => ..., Ok(_) => {}, Err(_) => {} }`

### Summary Math

```
total_in_scope = conventions.len()        // from query (excludes already-confirmed, rejected)
session_confirmed = count(Confirm actions)
session_rejected  = count(Reject actions)
session_partial   = count(Partial actions)
session_skipped   = count(Skip actions)
decided = session_confirmed + session_rejected + session_partial
pending = total_in_scope - decided - session_skipped
already_confirmed = DB COUNT of user-sourced convention/observation nodes NOT removed on current branch

session_precision = session_confirmed / max(decided, 1) * 100
overall_coverage  = (already_confirmed + session_confirmed) / (total_in_scope + already_confirmed) * 100
```

## Success Metrics

- [ ] `cargo fmt --check` — passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — 0 warnings
- [ ] `cargo build --release` — 0 errors, 0 warnings
- [ ] `cargo test --all-targets --all-features` — all tests pass
- [ ] UI visually matches PRD ASCII art layout (verified in terminal)
- [ ] Summary shows total, already-confirmed, pending, precision, coverage
- [ ] Summary printed exactly once
- [ ] TUI exits in < 200ms on `q`
- [ ] Confirm + scan: no duplicate in FTS5
- [ ] Re-review after confirm: confirmed convention NOT in list

## File List

```
crates/seshat-storage/migrations/V5__add_description_hash.sql  ← CREATE: Migration
crates/seshat-graph/src/detection.rs                          ← MODIFY: persist_conventions dedup check
crates/seshat-graph/src/fts.rs                                ← MODIFY: rebuild_fts_index dedup filter
crates/seshat-graph/src/decisions.rs                          ← MODIFY: record_decision writes description_hash
crates/seshat-cli/src/tui/widgets.rs                         ← REWRITE: single border, dividers, single-line bottom bar
crates/seshat-cli/src/tui/app.rs                             ← MODIFY: show_summary with context, query returns branch_id
crates/seshat-cli/src/tui/review_wizard.rs                   ← MODIFY: non-blocking event loop, remove show_summary
crates/seshat-cli/src/tui/mod.rs                             ← MODIFY: single show_summary, pass SummaryContext, branch_id from query
```

## Test Plan

### Migration Tests (seshat-storage)
1. `description_hash_column_exists` — V5 migration adds column, index exists
2. `description_hash_nullable` — existing rows have NULL hash
3. `backward_compat_no_hash` — FTS5 works with NULL description_hash

### Detection Tests (seshat-graph/detection.rs)
4. `persist_skips_auto_detected_when_user_hash_exists` — user node with hash → auto-detected NOT inserted
5. `persist_inserts_when_no_user_hash` — no user node → auto-detected inserted normally
6. `persist_ignores_user_rejected` — rejected nodes preserved (not deleted by scan)
7. `persist_hash_computation_consistent` — same description → same hash across calls

### FTS5 Tests (seshat-graph/fts.rs)
8. `fts_excludes_auto_with_user_counterpart` — auto-detected same hash as user node → excluded from FTS5
9. `fts_includes_user_nodes` — user nodes always indexed
10. `fts_includes_orphan_auto` — auto-detected with no user counterpart → indexed

### Decisions Tests (seshat-graph/decisions.rs)
11. `record_decision_sets_description_hash` — user node's description_hash is populated
12. `record_decision_hash_is_normalized` — hash uses normalized description

### Widget Tests (widgets.rs)
13. `outer_border_covers_full_area` — outer block = entire frame
14. `divider_blocks_border_flags` — dividers use LEFT|TOP|RIGHT
15. `example_expands_to_fill_space` — Min(3) takes remaining space
16. `bottom_bar_single_line` — exactly 1 row used
17. `code_truncated_to_width` — no overflow past border
18. `layout_80x24_minimum` — works on 80x24
19. `layout_120x40_standard` — works on 120x40
20. `layout_160x60_wide` — works on 160x60
21. `no_examples_hides_section` — example section has 0 height
22. `truncate_str_cjk` — wide chars handled (truncated by char count)

### App Tests (app.rs)
23. `query_returns_branch_id` — query_conventions_for_review returns branch_id
24. `show_summary_with_context` — total, already_confirmed, pending all shown
25. `show_summary_zero_actions` — meaningful stats with 0 session actions
26. `show_summary_precision_calc` — confirmed/decided * 100
27. `show_summary_coverage_calc` — (already+session) / total * 100
28. `show_summary_status_calibrated` — ≥70% → "calibrated"
29. `show_summary_status_low` — <70% → warning
30. `confirm_sets_user_confirmed_ext` — original auto-detected gets user_confirmed=1
31. `reject_concurrency_check_fails` — hash mismatch → error
32. `reject_concurrency_check_passes` — matching hash → proceeds
33. `reject_null_source_defaults_auto` — NULL source treated as auto_detected
34. `snapshot_hash_consistent` — same ext_data → same hash
35. `snapshot_hash_null_ext` — NULL ext_data → consistent hash

### Review Wizard Tests (review_wizard.rs)
36. `event_loop_skips_resize_event` — resize event doesn't block
37. `event_loop_skips_mouse_event` — mouse event doesn't block
38. `event_loop_handles_read_error` — Err from read() doesn't crash
39. `quit_exits_loop` — q/Esc/Ctrl+C sets quit, loop breaks
40. `no_show_summary_call` — verify no `show_summary()` in file

### Integration Tests
41. `confirm_then_scan_no_duplicate` — confirm → run scan → FTS5 has 1 result (user only)
42. `confirm_then_rereview_excluded` — confirm → new review session → confirmed NOT in list
43. `reject_then_rereview_excluded` — reject → new review → rejected NOT in list (pre-existing, verify)
44. `skip_then_rereview_included` — skip → new review → skipped IS in list
45. `summary_printed_once` — stdout capture shows one summary block
46. `description_hash_stable` — same description → same hash across runs
47. `scan_preserves_rejected` — scan doesn't delete user-rejected nodes
48. `confirm_then_scan_mcp_returns_one` — MCP tool query returns 1 result (deduped)

## References

- Original PRD: `.ralph/tasks/prd-tui-review-wizard-fixes.md`
- Nodes schema: `crates/seshat-storage/migrations/V1__initial_schema.sql`
- FTS5 schema: `crates/seshat-storage/migrations/V4__add_conventions_fts.sql`
- Detection: `crates/seshat-graph/src/detection.rs:222-288`
- Decisions: `crates/seshat-graph/src/decisions.rs:130-228`
- FTS: `crates/seshat-graph/src/fts.rs:26-122`
- Branch: `fix/tui-review-wizard-fixes`

User stories are ready for implementation.
