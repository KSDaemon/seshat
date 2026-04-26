# PRD: TUI Review Wizard — Layout, Summary, and Stability Fixes

## Introduction

**Type:** Fix

Fix the `seshat review` TUI so it:
1. Matches the PRD design exactly (single outer border, proper dividers, single-line bottom bar)
2. Shows meaningful summary after exit (total conventions, already-confirmed count, not just 0/0/0/0)
3. Never hangs on exit (no blocking `event::read()`)
4. No duplicate summary output
5. No branch mismatch between query and apply

## Goals

- **Fix UI layout** — Single outer cyan border, `├──` style dividers between sections, single-line bottom bar
- **Fix summary** — Show total conventions, already-confirmed from DB, meaningful precision even when user exits without actions
- **Fix save hang** — Replace blocking `event::read()` with `event::try_read()` pattern
- **Fix duplicate summary** — Remove double `show_summary()` calls
- **Fix branch mismatch** — Same branch_id for query and apply

## User Stories

### US-001: TUI layout matches PRD design spec

**Description:** As a developer, I want the review TUI to visually match the design spec with single outer border, proper dividers, and compact bottom bar, so that the interface is clean and professional.

**Target layout (120x30 terminal):**
```
┌─ Seshat Convention Review ──────────────────────────────────────────────────────────────────┐
│   1/53: Import grouping: stdlib → external → internal                                       │
├─────────────────────────────────────────────────────────────────────────────────────────────┤
│  Nature: Convention   Confidence: 100%   Weight: Strong                                     │
│  Found in: 4/4 files (100% adoption)                                                        │
├── Example: (…/crates/seshat-cli/src/lib.rs:44) ─────────────────────────────────────────────┤
│  44  pub use args::{Cli, Command};                                                          │
│  45  pub use db::{find_git_root, get_current_branch};                                       │
│  46  pub use error::CliError;                                                               │
├─────────────────────────────────────────────────────────────────────────────────────────────┤
│ [y] Confirm   [n] Reject   [p] Partial   [s] Skip   [↑↓/jk] Navigate   [q/Esc] Finish       │
└─────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Acceptance Criteria:**
- [ ] Single outer cyan border (`Borders::ALL`), no nested borders
- [ ] Divider lines between sections use `├─── ... ─┤` pattern (matching `Borders::LEFT | Borders::TOP | Borders::RIGHT`)
- [ ] Info section: Nature/Confidence/Weight on one line, adoption on second line
- [ ] Example section border uses `├──` (LEFT only, connects to outer border)
- [ ] Bottom bar is a SINGLE LINE (not 3 rows) — `[y] Confirm   [n] Reject   [p] Partial   [s] Skip   [↑↓/jk] Navigate   [q/Esc] Finish`
- [ ] Example code block fills remaining vertical space (not fixed 5 rows)
- [ ] Code lines are truncated to fit within the border width (no overflow)
- [ ] No `(no examples)` text filling the screen when has_examples is false
- [ ] `cargo check -p seshat-cli` compiles cleanly

### US-002: Summary shows meaningful stats even with zero actions

**Description:** As a developer, I want the summary after exiting the TUI to show the total number of conventions reviewed, already-confirmed count from DB, and a meaningful precision metric — even if I exit without taking any actions.

**Before (broken):**
```
  -- Review Complete -----------------------------------------------
     + Confirmed    0
     - Rejected     0
     ~ Partial      0
      x Skipped      0
     Precision: 0%
     Knowledge graph updated.
```

**After (correct):**
```
  -- Review Complete -----------------------------------------------

     Total reviewed: 53 conventions
     + Confirmed:    12 (already confirmed: 25, pending: 16)
     - Rejected:      5
     ~ Partial:       3
      x Skipped:     33

     Precision: 70%  (confirmed / decided)
     Status: + Seshat is calibrated and ready to use

     Knowledge graph updated.
```

**Acceptance Criteria:**
- [ ] Summary shows "Total reviewed: X conventions" (total from query)
- [ ] Summary shows "already confirmed" count from DB (not just session actions)
- [ ] Summary shows "pending" count (not yet acted on)
- [ ] Precision calculated as confirmed / (confirmed + rejected + partial), not 0 when no actions
- [ ] Summary is printed ONCE after TUI exits (not twice)
- [ ] "Knowledge graph updated." only shown if there were actual actions

### US-003: TUI never hangs on exit

**Description:** As a developer, I want the TUI to exit immediately when I press `q` without any delay or hang.

**Acceptance Criteria:**
- [ ] Pressing `q` exits within 100ms
- [ ] Pressing `Ctrl+C` exits within 100ms
- [ ] `event::poll()` is never followed by blocking `event::read()` when no event is pending
- [ ] If `poll()` returns true but `read()` fails (non-key event), loop continues without blocking
- [ ] No threading, no channels — everything is synchronous and fast

### US-004: No duplicate summary output

**Description:** As a developer, I want the summary to appear exactly once after the TUI exits, not twice.

**Acceptance Criteria:**
- [ ] `show_summary()` is called exactly once (in `mod.rs`, not in `review_wizard.rs`)
- [ ] Summary appears after `ratatui::restore()` (on normal stdout, not in alternate screen)
- [ ] No summary printed inside the TUI before exit

### US-005: Consistent branch ID across query and apply

**Description:** As a developer on a non-main branch, I want my review actions to be applied to the same branch that was queried, not silently applied to "main".

**Acceptance Criteria:**
- [ ] `query_conventions_for_review` returns the branch_id it used
- [ ] `apply_review_actions` receives the SAME branch_id as the query
- [ ] No `"main"` fallback — error if branch cannot be determined

## Functional Requirements

### FR-1: Single outer border layout

The entire TUI is enclosed in a single `Borders::ALL` cyan border. Internal section dividers use the pattern `├─── ... ─┤` — implemented as `Borders::LEFT | Borders::TOP | Borders::RIGHT` on a Block that spans the width.

**Implementation approach:**
- Use a single `Block::default().borders(Borders::ALL)` as the outer container
- Internal dividers: render a `Block::default().borders(Borders::LEFT | Borders::TOP | Borders::RIGHT)` with no title, just an empty row that creates the `├─── ┤` visual
- Example section border: same pattern but with `title` for the filename:line

### FR-2: Single-line bottom bar

The bottom bar with key bindings is exactly ONE row inside the outer border (not 3 rows). It shows:
`[y] Confirm   [n] Reject   [p] Partial   [s] Skip   [↑↓/jk] Navigate   [q/Esc] Finish`

### FR-3: Expandable example section

The example code section uses `Constraint::Min(3)` so it takes ALL remaining vertical space. Code lines are truncated to fit within the code block width.

### FR-4: Rich summary with total stats

`show_summary` accepts additional context:
- `total_conventions: usize` — total from query (passed in)
- `already_confirmed: usize` — count of existing user-sourced nodes (queried from DB)
- Computed: `pending = total - (confirmed + rejected + partial + skipped)`

### FR-5: Non-blocking event loop

Replace `event::poll()` + `event::read()` with `event::try_read()` to avoid blocking on non-key events. If `try_read()` returns `Err` or a non-key event, the loop continues to the next iteration.

### FR-6: Single summary output

Remove `show_summary()` call from `review_wizard.rs`. Keep only the call in `mod.rs` (after `ratatui::restore()`).

### FR-7: Consistent branch ID

`query_conventions_for_review` returns `(Vec<ConventionItem>, String)` where the `String` is the branch_id. This same branch_id is passed to `apply_review_actions`.

## Non-Goals (Out of Scope)

- No scrolling for oversized snippets (future)
- No terminal resize detection (future)
- No color theme customization
- No search/filter in TUI

## Design Considerations

### Layout Structure

```
+--- outer Block: Borders::ALL, cyan title ----+
| Row 0: title (Seshat Convention Review N/M)   |
| Row 1: "  1/M: description..."                |
| Row N: divider Block (LEFT|TOP|RIGHT)         |
| Row N+1: "Meta: Nature... Confidence..."      |
| Row N+2: "Found in..."                        |
| Row N+3: divider Block with title             |
| Row N+4: " Example: (file:line) "            |
| ... (code lines, fills remaining space)       |
| ...                                            |
| Row last-1: "  [y] Confirm   [n] Reject ...  "|
+------------------------------------------------+
```

### Color Scheme (unchanged)

- **Outer border**: Cyan
- **Metadata**: Nature=Green, Confidence=Yellow, Weight=Magenta
- **Example border**: Yellow title, DarkGray border
- **Code**: Green+Bold for highlighted lines, Yellow for non-highlighted
- **Bottom bar**: DarkGray border

## Technical Considerations

### Event Loop Fix

Current (blocks):
```rust
if event::poll(50ms)? {
     let key = event::read()?; // BLOCKS if last event was resize/mouse
     ...
}
```

Fixed (non-blocking):
```rust
if event::poll(50ms)? {
     match event::read() {
         Ok(Event::Key(k)) => handle_key(k),
         Ok(_) => {}, // resize/mouse — just skip
         Err(_) => {}, // read failed — skip
     }
}
```

### Summary Math

```
total_conventions = conventions.len()
confirmed = actions where Confirm
rejected = actions where Reject
partial = actions where Partial
skipped = actions where Skip
decided = confirmed + rejected + partial
pending = total_conventions - decided - skipped
already_confirmed = COUNT of nodes WHERE nature='convention' AND source='user' AND branch_id=X
precision = (confirmed / max(decided, 1)) * 100
```

## Success Metrics

- [ ] `cargo check -p seshat-cli` — compiles with 0 errors
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — 0 warnings
- [ ] `cargo test -p seshat-cli` — all tests pass
- [ ] UI visually matches PRD ASCII art layout
- [ ] Summary shows total conventions and already-confirmed count
- [ ] TUI exits in < 200ms on `q` key
- [ ] Summary printed exactly once
- [ ] Branch ID is consistent between query and apply

## File List

```
crates/seshat-cli/src/tui/widgets.rs        ← REWRITE: Single outer border, proper dividers, single-line bottom bar
crates/seshat-cli/src/tui/app.rs           ← MODIFY: Enriched show_summary, branch_id in query return
crates/seshat-cli/src/tui/review_wizard.rs ← MODIFY: Non-blocking event loop, remove show_summary call
crates/seshat-cli/src/tui/mod.rs           ← MODIFY: Remove duplicate show_summary, pass total_conventions
```

## Test Plan

### Widget Tests (widgets.rs)
1. `outer_border_covers_full_area` — Verify outer block covers entire frame area
2. `divider_line_has_correct_borders` — Verify divider blocks use LEFT|TOP|RIGHT borders
3. `example_border_has_left_only` — Example section divider uses LEFT border only
4. `bottom_bar_single_line` — Bottom bar occupies exactly 1 row
5. `example_expands_to_fill_space` — Example section takes remaining vertical space
6. `code_truncated_to_width` — Code lines don't overflow the example block width
7. `layout_80x24_minimum` — Layout works on 80x24 terminal
8. `layout_120x40_standard` — Layout works on 120x40 terminal
9. `layout_160x60_wide` — Layout works on 160x60 terminal

### App Tests (app.rs)
10. `show_summary_with_total_context` — Summary includes total_conventions count
11. `show_summary_already_confirmed` — Summary shows already_confirmed from DB
12. `show_summary_pending_count` — Summary calculates pending correctly
13. `show_summary_precision_with_existing` — Precision includes already confirmed
14. `show_summary_no_actions` — Shows meaningful stats even with 0 actions
15. `show_summary_status_calibrated` — Status "calibrated" when precision >= 70%

### Review Wizard Tests (review_wizard.rs)
16. `event_loop_non_blocking` — Event loop doesn't block on non-key events
17. `no_show_summary_call` — verify `show_summary` is NOT called in review_wizard
18. `quit_exits_immediately` — `q` key sets quit=true, loop breaks on next iteration

### Integration Tests
19. `full_render_matches_golden` — Render output matches expected ASCII art layout
20. `summary_output_once` — Summary is printed exactly once after TUI exit
21. `branch_id_consistency` — Same branch_id used for query and apply

## References

- Original PRD: `.ralph/tasks/prd-tui-review-wizard-fixes.md`
- Code: `crates/seshat-cli/src/tui/`
- Branch: `fix/tui-review-wizard-fixes`
