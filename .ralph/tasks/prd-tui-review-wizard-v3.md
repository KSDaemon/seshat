# PRD: TUI Review Wizard — Layout, Dedup, Summary, Stability, Example Navigation

## Introduction

**Type:** Fix

Comprehensive fix for the `seshat review` TUI review wizard covering:

1. **UI Layout** — matches PRD design spec: single outer border, proper dividers, single-line bottom bar
2. **Example Navigation** — left-right navigation between code examples (←→ / A / D)
3. **Convention Dedup** — when user confirms a convention, subsequent scans MUST NOT recreate it. Description hash dedup ensures one convention = one visible node in FTS5
4. **Summary** — meaningful stats: total, already-confirmed, pending, precision, coverage. Printed exactly once
5. **Stability** — no hang on exit, non-blocking event loop, consistent branch ID
6. **Comprehensive Tests** — unit tests for all paths, integration tests with ratatui-testlib for terminal behavior, golden-layout tests for widget rendering

## Goals

- **Fix UI layout** — single outer cyan border, `├───` dividers, single-line bottom bar, expandable example
- **Add example navigation** — left/right navigate between examples in a convention (1/3 → 2/3 → 3/3 → 1/3)
- **Fix convention dedup** — confirm + scan → NO duplicate in FTS5. Description hash links user node to auto-detected node
- **Fix summary** — total/confirmed/rejected/partial/skipped/pending/already-confirmed. Printed once
- **Fix save hang** — match `event::read()` on `Event::Key`, skip non-key events
- **Fix branch mismatch** — `query_conventions_for_review` returns branch_id, same passed to `apply_review_actions`
- **Verify UI visually** — `ratatui-testlib` golden-layout assertions + snapshot tests

## User Stories

### US-001: TUI layout matches PRD design spec

**Description:** As a developer, I want the review TUI to visually match the design spec with single outer border, proper dividers, and a compact bottom bar, so that I can read and interact with conventions clearly.

**Target layout (120x30 terminal):**
```
┌─ Seshat Convention Review ───────────────────────────────────────────────────────────────────────────┐
│    1/53: Import grouping: stdlib → external → internal                                               │
├──────────────────────────────────────────────────────────────────────────────────────────────────────┤
│  Nature: Convention       Confidence: 100%       Weight: Strong                                      │
│  Found in: 4/4 files (100% adoption)                                                                 │
├─ Example (1/3): (…/crates/seshat-cli/src/lib.rs:44) ────────────────────────────────────────────────┤
│   44  pub use args::{Cli, Command};                                                                  │
│   45  pub use db::{find_git_root, get_current_branch};                                               │
│   46  pub use error::CliError;                                                                       │
│                                                                                                      │
├──────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ [y] Confirm  [n] Reject  [p] Partial  [s] Skip  [↑↓/jk] Navigate  [←→] Examples   [q/Esc] Finish     │
└──────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Acceptance Criteria:**
- [ ] Single outer cyan border (`Borders::ALL`), no nested borders
- [ ] Divider lines between sections use `├─── ... ─┤` pattern (`Borders::LEFT | Borders::TOP | Borders::RIGHT`)
- [ ] Info section: Nature/Confidence/Weight on one line, adoption on second line
- [ ] Example section divider uses `├──` with `title` showing `Example (N/M): (file:line)` when M > 1, or `Example: (file:line)` when M == 1
- [ ] Bottom bar is a SINGLE LINE (not 3 rows) with all controls in one row
- [ ] Example code block fills ALL remaining vertical space (not fixed N rows)
- [ ] Code lines truncated to fit within border width (no overflow, no wrapping)
- [ ] When no examples: example section hidden (no "(no examples)" text filling screen)
- [ ] `cargo check -p seshat-cli` compiles with 0 errors, 0 warnings
- [ ] UI rendering verified with `ratatui-testlib` golden-layout tests (see Test Plan section)

### US-002: Navigate examples left-right between examples

**Description:** As a developer, I want to navigate between code examples using left/right arrows when a convention has multiple examples, so that I can see representative snippets and make an informed decision.

**Before (broken):**
```
├── Example: (lib.rs:42) ──────────────────────┐
│   42  fn main() -> Result<()> {              │
│   43      ...                                │
│                                              │
├── Example: (default.rs:5) ───────────────────┤
│   5  impl Default for Config {               │
│   6      ...                                 │
└──────────────────────────────────────────────┘
```
All examples rendered at once with no control — user sees all but cannot focus on one.

**After (correct):**
```
├── Example (2/3): (default.rs:5) ────────────────────┤
│   5  impl Default for Config {                      │
│   6      Config { name: "default".to_owned() }      │
│                                                     │
└─────────────────────────────────────────────────────┘
```
- Example counter appears: `Example (2/3)` — currently showing 2nd of 3 examples
- `[←]` / `[→]` or `[A]` / `[D]` (lowercase) keys cycle example index `0 → 2 → 0`
- When 1 example: no counter, normal title: `├── Example: (lib.rs:42) ─┤`
- Example index is LOCAL to the current convention (resets when next convention is shown)
- Bottom bar shows `[←→] Examples` shortcut when examples > 1, hidden when examples == 1

**Acceptance Criteria:**
- [ ] `App` struct has a way to track current example index per convention: `HashMap<ConventionItem, usize>` — OR use a field `example_index: usize` on `ConventionItem` clone (recommended — simpler)
- [ ] When `examples.len() > 1`, example divider title shows counter: `├── Example (2/3): (file.rs:5) ─┤`
- [ ] When `examples.len() == 1`, title shows normally: `├── Example: (file.rs:5) ─┤` (no counter)
- [ ] `[A]` or `[←]` key cycles example index: `(current - 1) % count`
- [ ] `[D]` or `[→]` key cycles example index: `(current + 1) % count`
- [ ] Example index resets to 0 when `next()`/`previous()` (convention change) is called
- [ ] Example index clamped: `0 ≤ index < examples.len()`
- [ ] Bottom bar conditionally shows `←→` when examples > 1
- [ ] ratatui-testlib golden asserts check example counter text at expected position

### US-003: Convention dedup via description hash

**Description:** As a developer, I want `seshat review` to show only conventions I have NOT yet acted on, so that the list shrinks with each session and I never see duplicates of confirmed conventions after a scan.

**Current bug:** User confirms 6 of 54 → new user node created, original auto-detected untouched → scan deletes old auto-detected + inserts new → now 60 conventions (54 original + 6 confirmed as new auto-detected).

**Root cause:** `record_decision` creates a separate user node. The auto-detected node is NOT modified. On next scan, auto-detected nodes are replaced (DELETE+INSERT), and the confirmed convention gets re-inserted as a new auto-detected node. FTS5 indexes both.

**Acceptance Criteria:**
- [ ] `query_conventions_for_review` excludes auto-detected nodes whose `description_hash` matches any user node's `description_hash` (already confirmed)
- [ ] `confirm_convention` writes `description_hash` to the user node's `description_hash` column
- [ ] `persist_conventions` (scan INSERT phase) skips insertion if a user node with matching `description_hash` exists
- [ ] `rebuild_fts_index` only indexes one node per `description_hash` (user node takes priority over auto-detected)
- [ ] After confirm→scan→review cycle: confirmed convention NOT in review list
- [ ] After confirm→scan: MCP tools return only the user node (not both user + auto-detected)
- [ ] Description hash is stable: normalize (lowercase, trim, collapse whitespace) before hashing

### US-004: Summary shows full picture (total, pending, already-confirmed)

**Description:** As a developer, I want the post-review summary to show total conventions, already-confirmed count, pending count, and meaningful precision — even if I exit without actions.

**Before (broken):**
```
    -- Review Complete -----------------------------------------------
       + Confirmed     6     (← only session count, but 48 conventions were never touched)
       - Rejected       0
       ~ Partial        0
       x Skipped        0
      Precision: 100%  (← 6/6, but 48 were never reviewed)
      Knowledge graph updated.
```

**After (correct):**
```
    -- Review Complete -----------------------------------------------

      Conventions in scope: 53
        + Confirmed this session:    6
        - Rejected this session:     0
        ~ Partial this session:      0
        x Skipped this session:     47
        Already confirmed (DB):    18
        Still pending:              29

      Session precision:   0%   (6 confirmed / 6 decided)
      Overall coverage:   51%   (6 confirmed + 18 already / (53 + 18))

      Knowledge graph updated.
```

**Acceptance Criteria:**
- [ ] `show_summary` accepts `SummaryContext { total_in_scope: usize, already_confirmed: usize }`
- [ ] "Conventions in scope" = total conventions returned by query (excludes already-confirmed, rejected)
- [ ] "Already confirmed" = COUNT of user-sourced convention/observation nodes on current branch, not yet removed
- [ ] "Still pending" = total_in_scope - (confirmed + rejected + partial + skipped)
- [ ] "Session precision" = confirmed / max(confirmed + rejected + partial, 1) * 100
- [ ] "Overall coverage" = (already_confirmed + session_confirmed) / (total_in_scope + already_confirmed) * 100
- [ ] Summary printed exactly ONCE after `ratatui::restore()`
- [ ] "Knowledge graph updated." shown only if there were actual DB actions
- [ ] When no actions (user presses q immediately): all session counts = 0, pending = total_in_scope

### US-005: TUI exits immediately, no hang

**Description:** As a developer, I want the TUI to exit in under 200ms when I press `q`, without any delay or terminal corruption.

**Acceptance Criteria:**
- [ ] `event::read()` result is matched on `Event::Key` — non-key events (resize, mouse) are skipped
- [ ] If `read()` returns `Err` or non-key event, loop continues to next iteration
- [ ] Pressing `q` or `Esc` or `Ctrl+C` exits within 200ms
- [ ] No terminal control characters remain after exit
- [ ] No zombie threads or resource leaks

### US-006: Consistent branch ID across query and apply

**Description:** As a developer on a feature branch, I want my review actions applied to the same branch that was queried.

**Current bug:** `query_conventions_for_review` correctly determines branch from git. But `run_review_tui` uses `get_current_branch(...).unwrap_or_else(|| "main")` — if git branch detection fails mid-session, actions apply to "main" instead of the queried branch.

**Acceptance Criteria:**
- [ ] `query_conventions_for_review` returns `(Vec<ConventionItem>, String)` — both items AND branch_id
- [ ] `run_review_tui` and `run_review_tui_with_conn` use the branch_id returned by the query
- [ ] No `"main"` fallback — if branch cannot be determined, return error before initializing TUI

### US-007: Snapshot hash validates reject concurrency

**Description:** As a developer, I want rejected conventions to be verified with snapshot hash to prevent applying rejects to modified nodes.

**Acceptance Criteria:**
- [ ] `reject_convention` computes `compute_snapshot_hash(&ext_data)` and compares against `expected_hash`
- [ ] If hashes don't match: returns error "convention was modified during review; please retry"
- [ ] `json_extract` returning NULL (no `$.source` key) is handled — defaults to `"auto_detected"`
- [ ] `compute_snapshot_hash` handles `Option::None` consistently

## Functional Requirements

### FR-1: Query returns branch_id
```rust
pub fn query_conventions_for_review(...) -> Result<(Vec<ConventionItem>, String), CliError>
```

### FR-2: Single outer border layout
Single `Block::default().borders(Borders::ALL)` cyan container. Internal dividers are `Block::default().borders(Borders::LEFT | Borders::TOP | Borders::RIGHT)` — renders as `├─── ... ─┤`.

### FR-3: Single-line bottom bar
One row: `y` Confirm `n` Reject `p` Partial `s` Skip `←→` Examples `↑↓/jk` Navigate `[q/Esc]` Finish
Truncated with `...` on narrow terminals.

### FR-4: Expandable example section
`Constraint::Min(3)` — example takes ALL remaining vertical space. `Constraint::Length(0)` when no examples.

### FR-5: Description hash dedup

**Hash function:** `description_hash = sha256(normalize(description)).hex()[0..16]`

**Normalize:** lowercase, trim, collapse internal whitespace, strip leading/trailing punctuation

**Migration:**
```sql
-- V6__add_description_hash.sql
ALTER TABLE nodes ADD COLUMN description_hash TEXT DEFAULT NULL;
CREATE INDEX idx_nodes_description_hash ON nodes(description_hash);
```

**Scan INSERT (persist_conventions):**
```sql
SELECT 1 FROM nodes WHERE description_hash = ? AND json_extract(ext_data, '$.source') = 'user' LIMIT 1
```
If row exists → skip auto-detected insert (user decision is authoritative).

**FTS5 (rebuild_fts_index):**
```sql
DELETE FROM conventions_fts;
INSERT INTO conventions_fts (description, node_id, detector_name)
SELECT n.description, CAST(n.id AS TEXT),
       COALESCE(json_extract(n.ext_data, '$.detector_name'), '')
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

### FR-6: Example navigation data model
```rust
// In app.rs
pub struct ConventionItem {
    // ... existing fields ...
    pub example_index: usize,  // 0 initially
}

// In App struct
pub struct App {
    // ... existing fields ...
    pub conventions: Vec<ConventionItem>, // Clone + example_index = 0 on next()
}

impl App {
    pub fn next_example(&mut self) {
        if let Some(conv) = self.current() {
            let total = conv.examples.len();
            if total > 1 {
                conv.example_index = (conv.example_index + 1) % total;
            }
        }
    }

    pub fn previous_example(&mut self) {
        if let Some(conv) = self.current() {
            let total = conv.examples.len();
            if total > 1 && conv.example_index > 0 {
                conv.example_index -= 1;
            }
        }
    }
}
```

### FR-7: Rich summary with context
```rust
pub struct SummaryContext {
    pub total_in_scope: usize,
    pub already_confirmed: usize,
}

pub fn show_summary(results: &[ReviewAction], context: &SummaryContext)
```

### FR-8: Non-blocking event loop
```rust
if event::poll(50ms)? {
    match event::read() {
        Ok(Event::Key(k)) => ...,
        Ok(_) => {},   // resize/mouse — skip
        Err(_) => {}, // read failed — skip
    }
}
```

## Non-Goals (Out of Scope)

- No scrolling for oversized snippets (future)
- No terminal resize detection (future)
- No color theme customization
- No search/filter in TUI
- No "Show All" mode for viewing previously-confirmed conventions (future)
- No partial description matching (exact hash match only)
- No interactive example preview — only code block + counter

## Design Considerations

### Layout Structure
```
+--- outer Block: Borders::ALL, cyan title ----+
| Row 0: title " Seshat Convention Review  1/N "-|
| Row 1: "   1/N: description..."                 |
| Divider: Block LEFT|TOP|RIGHT (no title)        |
| Info: "Nature: X  Confidence: Y  Weight: Z"      |
| Info: "Found in: A/B files (C% adoption)"         |
| Divider: Block LEFT|TOP|RIGHT with title "Example (2/3): (file:line)"|
| Example: " Example: (file:line) "                |
| ... code lines (fills remaining space)           |
| Divider: Block LEFT|TOP|RIGHT (no title)         |
| Bottom: " [y] Confirm [n] Reject ... [←→] Examples ... "  |
+------------------------------------------------+
```

### Example Cycle Behavior
| Current Example | Action | Next Example |
|----------------|--------|-------------|
| 1/3 (index 0)    | `↓` or `A` | 3/3 (index 2)  |
| 2/3 (index 1)    | `↓` or `A` | 1/3 (index 0)  |
| 3/3 (index 2)    | `↓` or `A` | 2/3 (index 1)  |
| 3/3 (index 2)    | `↑` or `D` | 2/3 (index 1)  |
| 2/3 (index 1)    | `↑` or `D` | 1/3 (index 0)  |
| 1/3 (index 0)    | `↑` or `D` | 3/3 (index 2)  |

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
-- V6__add_description_hash.sql
ALTER TABLE nodes ADD COLUMN description_hash TEXT DEFAULT NULL;
CREATE INDEX idx_nodes_description_hash ON nodes(description_hash);
```

### Backward Compatibility
- Existing nodes without `description_hash` (NULL) are unaffected
- Old confirmed nodes: no hash — scan will re-insert auto-detected. Once re-confirmed, hash is set
- FTS5 query works with NULL description_hash (they just don't get deduped)

### Event Loop Fix (save hang root cause)
The bug was `event::read()` blocking when `poll()` returned true for a non-key event (resize, mouse). `read()` would block waiting for the NEXT event. Fix: `match event::read() { Ok(Event::Key) => ..., Ok(_) => {}, Err(_) => {} }`

### Summary Math
```
total_in_scope = conventions.len()         // from query (excludes already-confirmed, rejected)
session_confirmed = count(Confirm actions)
session_rejected   = count(Reject actions)
session_partial    = count(Partial actions)
session_skipped    = count(Skip actions)
decided = session_confirmed + session_rejected + session_partial
pending = total_in_scope - decided - session_skipped
already_confirmed = DB COUNT of user-sourced convention/observation nodes NOT removed on current branch

session_precision = session_confirmed / max(decided, 1) * 100
overall_coverage   = (already_confirmed + session_confirmed) / (total_in_scope + already_confirmed) * 100
```

### ratatui-testlib for UI Testing

**Dependency:**
```toml
# seshat-cli/Cargo.toml dev-dependencies
ratatui-testlib = { version = "0.1.0", features = ["full", "ratatui-helpers", "snapshot-expect"] }
expect-test = "1.5"
```

**Usage pattern:**
```rust
// tests/tui_widgets.rs
use ratatui_testlib::{TuiTestHarness, ScreenState};

#[test]
fn verify_basic_layout() {
    // Create a mock terminal via ratatui-testlib
    let mut h = TuiTestHarness::new(80, 24);

    // Render widget to buffer
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    let card = ConventionCard { ... };
    card.render(buf.area, buf.buffer_mut());

    // Assert golden layout using ratatui-testlib assertions
    assert_text_at_position(&buf, "Seshat Convention Review", 0, 4);
    assert_text_at_position(&buf, "1/1:", 1, 2);
    assert_text_within_bounds(&buf, "Nature: Convention", 3, 2, 10);
    assert_text_at_position(&buf, "├── Example:", 15, 2);
    assert_text_at_position(&buf, "[y] Confirm", 22, 2);
}

#[test]
fn verify_example_counter() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    let mut conv = ConventionCard { ... };
    conv.example_index = 2; // Show "2/3"
    conv.examples.len = 3;

    card.render(buf.area, buf.buffer_mut());

    // Title should show "(2/3)" at expected position
    let row_16 = buf.get_line(14);
    assert!(row_16.cells.iter().any(|c| c.content.contains("(2/3)")));
}
```

**What ratatui-testlib provides:**
1. `TuiTestHarness::new(w, h)` + `expect_output(name)` — golden file assertions for full TUI output (PTY-based, real process)
2. `ScreenState::feed(bytes)` — feed raw cursor escape sequences, then assertion `text_at(row, col)`, `get_cell(row, col).fg`
3. `snapshot-expect` feature — inline snapshot comments in test files, auto-updated with `UPDATE_EXPECT=1`
4. `insta` feature — golden file snapshots with version control

**Widget-level unit tests (via ratatui TestBackend + ratatui-testlib helpers):**
- `outer_border_covers_full_area` — outer block = entire frame
- `divider_blocks_have_correct_flags` — dividers use LEFT|TOP|RIGHT
- `example_expands_to_fill_space` — Min(3) takes remaining space
- `example_counter_visible_when_n_gt_1` — "(N/M)" appears in divider title
- `example_counter_hidden_when_n_eq_1` — no counter
- `bottom_bar_single_line` — exactly 1 row used
- `code_truncated_to_width` — no overflow past border
- `layout_80x24_minimum` — works on 80x24
- `layout_120x40_standard` — works on 120x40
- `layout_160x60_wide` — works on 160x60
- `no_examples_hides_section` — example section has 0 height

**Terminal integration tests (via ratatui-testlib `TuiTestHarness`):**
- `confirm_then_scan_no_duplicate` — confirm → run scan → FTS5 has 1 result (user only)
- `confirm_then_rereview_excluded` — confirm → new review → confirmed NOT in list
- `reject_then_rereview_excluded` — reject → new review → rejected NOT in list
- `skip_then_rereview_included` — skip → new review → skipped IS in list
- `summary_printed_once` — stdout capture shows one summary block
- `description_hash_stable` — same description → same hash across runs
- `scan_preserves_rejected` — scan doesn't delete user-rejected nodes
- `confirm_then_scan_mcp_returns_one` — MCP tool query returns 1 result (deduped)
- `key_left_cycles_example_before` — `A` cycle: `(N-1) % count`
- `key_right_cycles_example_after` — `D` cycle: `(N+1) % count`
- `reset_example_on_convention_change` — next convention example_index = 0

## Success Metrics

- [ ] `cargo fmt --check` — passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — 0 warnings
- [ ] `cargo build --release` — 0 errors, 0 warnings
- [ ] `cargo test --all-targets --all-features` — all tests pass
- [ ] UI visually matches PRD ASCII art layout (verified via `ratatui-testlib` golden-layout tests)
- [ ] Summary shows total, already-confirmed, pending, precision, coverage
- [ ] Summary printed exactly once
- [ ] TUI exits in < 200ms on `q`
- [ ] Confirm + scan: no duplicate in FTS5
- [ ] Re-review after confirm: confirmed convention NOT in list

## File List

```
crates/seshat-storage/migrations/V6__add_description_hash.sql   ← CREATE: Migration
crates/seshat-graph/src/detection.rs                         ← MODIFY: persist_conventions dedup check
crates/seshat-graph/src/fts.rs                             ← MODIFY: rebuild_fts_index dedup filter
crates/seshat-graph/src/decisions.rs                     ← MODIFY: record_decision writes description_hash
crates/seshat-cli/src/tui/widgets.rs                    ← REWRITE: single border, dividers, single-line bottom bar, example navigation
crates/seshat-cli/src/tui/app.rs                        ← MODIFY: show_summary with context, query returns branch_id, add example_index field
crates/seshat-cli/src/tui/review_wizard.rs               ← MODIFY: non-blocking event loop, remove show_summary call, handle A/D keys
crates/seshat-cli/src/tui/mod.rs                        ← MODIFY: single show_summary, pass SummaryContext, branch_id from query
crates/seshat-cli/Cargo.toml (dev-dependencies)          ← MODIFY: add ratatui-testlib, expect-test
crates/seshat-cli/tests/tui_widgets.rs                 ← CREATE: Widget layout tests with ratatui-testlib
crates/seshat-cli/tests/tui_integration.rs             ← MODIFY: terminal integration tests with TuiTestHarness
```

## Test Plan

### Migration Tests (seshat-storage)
1. `description_hash_column_exists` — V6 migration adds column, index exists
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

### Widget Tests (tui_widgets.rs — via ratatui + ratatui-testlib)
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
23. `example_counter_visible_n_gt_1` — divider shows "(1/3)" for 3 examples
24. `example_counter_hidden_n_eq_1` — no counter for 1 example
25. `example_index_resets_on_next_convention` — next convention example_index = 0
26. `example_index_clamped_to_range` — `(n-1)%n` works correctly for 2 examples
27. `golden_layout_matches_golden_file` — full widget render matches golden.txt snapshot

### App Tests (app.rs)
28. `query_returns_branch_id` — query_conventions_for_review returns branch_id
29. `show_summary_with_context` — total, already_confirmed, pending all shown
30. `show_summary_zero_actions` — meaningful stats with 0 session actions
31. `show_summary_precision_calc` — confirmed/decided * 100
32. `show_summary_coverage_calc` — (already+session) / total * 100
33. `show_summary_status_calibrated` — ≥70% → "calibrated"
34. `show_summary_status_low` — <70% → warning
35. `confirm_sets_user_confirmed_ext` — original auto-detected gets user_confirmed=1
36. `reject_concurrency_check_fails` — hash mismatch → error
37. `reject_concurrency_check_passes` — matching hash → proceeds
38. `reject_null_source_defaults_auto` — NULL source treated as auto_detected
39. `snapshot_hash_consistent` — same ext_data → same hash
40. `snapshot_hash_null_ext` — NULL ext_data → consistent hash

### Review Wizard Tests (review_wizard.rs)
41. `event_loop_skips_resize_event` — resize event doesn't block
42. `event_loop_skips_mouse_event` — mouse event doesn't block
43. `event_loop_handles_read_error` — Err from read() doesn't crash
44. `quit_exits_loop` — q/Esc/Ctrl+C sets quit, loop breaks
45. `no_show_summary_call` — verify no `show_summary()` in file
46. `handle_key_a_cycles_example_backward` — `A` decrements index (wraps)
47. `handle_key_d_cycles_example_forward` — `D` increments index (wraps)
48. `handle_key_left_cycles_example_backward` — `←` same as `A`
49. `handle_key_right_cycles_example_forward` — `→` same as `D`
50. `handle_key_none_effect_on_example_index` — non-example key doesn't change index

### Integration Tests (tui_integration.rs — via ratatui-testlib TuiTestHarness)
51. `confirm_then_scan_no_duplicate` — confirm → run scan → FTS5 has 1 result (user only)
52. `confirm_then_rereview_excluded` — confirm → new review → confirmed NOT in list
53. `reject_then_rereview_excluded` — reject → new review → rejected NOT in list (verify pre-existing)
54. `skip_then_rereview_included` — skip → new review → skipped IS in list
55. `summary_printed_once` — stdout capture shows one summary block
56. `description_hash_stable` — same description → same hash across runs
57. `scan_preserves_rejected` — scan doesn't delete user-rejected nodes
58. `confirm_then_scan_mcp_returns_one` — MCP tool query returns 1 result (deduped)
59. `key_left_example_cycle` — `A` cycles example index backward
60. `key_right_example_cycle` — `D` cycles example index forward
61. `key_quit_returns_correct_actions` — q exits, correct actions returned
62. `ratatui_testlib_assert_text_at_position` — TuiTestHarness validates text at (row, col)
63. `ratatui_testlib_assert_snapshot_matches` — golden layout file matches rendered output

### ratatui-testlib Usage Details

**Unit tests (widget rendering):**
```rust
// crates/seshat-cli/tests/tui_widgets.rs
use crate::tui::app::{App, ConventionItem, ConventionItem, CodeExample};
use crate::tui::widgets::{ConventionCard, Convention};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier},
    widgets::{Block, Borders,},
};
use expect_test::expect;

#[test]
fn verify_basic_layout() {
    let conv = ConventionItem {
        node_id: 1,
        description: "Test convention".to_owned(),
        nature: "convention".to_owned(),
        weight: "strong".to_owned(),
        confidence_pct: 95,
        adoption_count: 10,
        total_count: 10,
        adoption_rate_pct: 100,
        trend: "stable".to_owned(),
        source: "auto_detected".to_owned(),
        examples: vec![CodeExample {
            file: "test.rs".to_owned(),
            line: 42,
            end_line: 45,
            snippet: "fn test() {}".to_owned(),
        }],
        snapshot_hash: 0,
        example_index: 0,
    };

    let mut app = App::new(vec![conv]);
    app.current_index = 0;

    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));

    app.render(buf.area, buf.buffer_mut());

    expect![[r#"
        [0,0] ─┬────── Seshat Convention Review    1/1 ────────────────────────────────────┐
        [1,2]   1/1: Test convention
        [3,2]      Nature: Convention    Confidence: 95%       Weight: Strong
        [4,2]      Found in: 10/10 files (100% adoption)
    "#]]
        .assert_debug_eq(buf);
}
```

**Integration tests (full TUI process, via TuiTestHarness):**
```rust
use ratatui_testlib::TuiTestHarness;

#[test]
fn verify_example_navigation() {
    let mut h = TuiTestHarness::new(80, 24)?;
    h.spawn(CommandBuilder::new("./target/debug/seshat"))?; // Or simulates via mock
    h.wait_for_text("Review Convention")?;
    h.send_text("n")?; // Reject
    h.send_text("↓")?; // Next
    h.send_text("↓")?; // More
    h.send_text("A")?; // Previous example
    h.wait_for_text("Example (2/3)");
    h.send_text("D")?; // Next example
    h.wait_for_text("Example (3/3)");
    h.send_text("q")?; // Quit
    h.wait_for_text("Review Complete");
}
```

## File List (revised with new tests)

```
crates/seshat-storage/migrations/V6__add_description_hash.sql   ← CREATE: Migration
crates/seshat-graph/src/detection.rs                         ← MODIFY: persist_conventions dedup check
crates/seshat-graph/src/fts.rs                             ← MODIFY: rebuild_fts_index dedup filter
crates/seshat-graph/src/decisions.rs                     ← MODIFY: record_decision writes description_hash
crates/seshat-cli/src/tui/widgets.rs                    ← REWRITE: single border, dividers, single-line bottom bar, example navigation
crates/seshat-cli/src/tui/app.rs                        ← MODIFY: show_summary with context, query returns branch_id, add example_index field
crates/seshat-cli/src/tui/review_wizard.rs               ← MODIFY: non-blocking event loop, remove show_summary, A/D key handling
crates/seshat-cli/src/tui/mod.rs                         ← MODIFY: single show_summary, SummaryContext, branch_id from query
crates/seshat-cli/Cargo.toml (dev-dependencies)          ← MODIFY: add ratatui-testlib, expect-test
crates/seshat-cli/tests/tui_widgets.rs                   ← CREATE: Widget layout tests with ratatui-testlib golden assertions
crates/seshat-cli/tests/tui_integration.rs               ← MODIFY: terminal integration tests with TuiTestHarness
```

## References

- Original PRD: `.ralph/tasks/prd-tui-review-wizard-fixes.md`
- Nodes schema: `crates/seshat-storage/migrations/V1__initial_schema.sql`
- FTS5 schema: `crates/seshat-storage/migrations/V4__add_conventions_fts.sql`
- Detection: `crates/seshat-graph/src/detection.rs:222-288`
- Decisions: `crates/seshat-graph/src/decisions.rs:130-228`
- FTS: `crates/seshat-graph/src/fts.rs:26-122`
- Branch: `fix/tui-review-wizard-fixes`
