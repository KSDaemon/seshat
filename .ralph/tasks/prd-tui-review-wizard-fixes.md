# PRD: TUI Review Wizard Fixes

## Introduction

**Type:** Fix

Fix critical issues with the `seshat review` TUI review wizard that prevent users from using the feature effectively. The current implementation has UI layout problems (overlapping text, cramped spacing, nested borders), terminal corruption on exit (control characters remain in shell), application hangs (unresponsive state after confirming/rejecting conventions), and data persistence issues (code snippets disappearing after saving decisions).

This PRD covers a full redesign of the TUI layout, fixes for terminal management and cleanup, data persistence verification, and comprehensive testing strategy to ensure no regressions.

## Goals

- **Redesign TUI layout** — Clean, adaptive interface that respects terminal boundaries
- **Fix terminal corruption** — Proper cleanup on exit, no control characters remaining
- **Fix application hangs** — Graceful shutdown handling, no unresponsive state
- **Fix data persistence** — Ensure code snippets and decisions are saved correctly
- **Comprehensive testing** — Unit tests, integration tests, and real-data validation

## User Stories

### US-001: Review conventions with clean, adaptive TUI layout

**Description:** As a developer, I want to review conventions in a clean, well-organized TUI that adapts to any terminal size, so that I can easily read and evaluate each convention without UI clutter.

**Acceptance Criteria:**
- [ ] TUI layout follows the new design (header + info section + example section + bottom bar)
- [ ] All text respects terminal boundaries (no overlapping, no text going beyond borders)
- [ ] Layout adapts to terminal width (80x24, 120x40, 160x60)
- [ ] Layout adapts to terminal height (small, medium, large)
- [ ] No nested borders — each section has its own clean border
- [ ] Example code block takes remaining vertical space
- [ ] Typecheck passes (no Rust warnings or errors)

### US-002: Exit TUI cleanly without terminal corruption

**Description:** As a developer, I want the TUI to exit cleanly without leaving control characters or corrupting my terminal, so that I can continue using my shell normally.

**Acceptance Criteria:**
- [ ] Pressing `q` exits TUI and restores terminal to normal state
- [ ] Pressing `Ctrl+C` exits TUI and restores terminal to normal state
- [ ] Pressing `Esc` exits TUI and restores terminal to normal state
- [ ] No control characters remain in shell after exit
- [ ] Shell prompt is clean and usable immediately after exit
- [ ] No zombie processes or resource leaks
- [ ] All DB connections are properly closed
- [ ] All mutexes are unlocked

### US-003: TUI never hangs or becomes unresponsive

**Description:** As a developer, I want the TUI to remain responsive at all times, so that I can navigate, confirm, reject, and exit without the application freezing.

**Acceptance Criteria:**
- [ ] TUI remains responsive during navigation (↑↓ keys work immediately)
- [ ] TUI remains responsive during confirm/reject/partial operations
- [ ] TUI remains responsive during batch save on exit
- [ ] No unresponsive state after confirming/rejecting conventions
- [ ] No unresponsive state during batch save
- [ ] Proper error handling for DB operations (no hangs on DB locked)
- [ ] Graceful error messages if operations fail

### US-004: Decisions and code snippets are persisted correctly

**Description:** As a developer, I want my review decisions (confirm/reject/partial) and associated code snippets to be saved correctly, so that I don't lose my work when the TUI exits.

**Acceptance Criteria:**
- [ ] Confirm action creates a new user-decision node with all examples
- [ ] Reject action marks node as user_rejected and removes from FTS5
- [ ] Partial action creates a preference node with description
- [ ] All decisions are batch-applied when pressing `q`
- [ ] Code snippets are persisted correctly (no disappearing snippets)
- [ ] Decisions survive TUI restart (exit → restart → verify decisions exist)
- [ ] Rejected conventions are not recreated on re-scan (Persisted Rejection)
- [ ] FTS5 index is updated after batch save

### US-005: Navigate conventions with keyboard shortcuts

**Description:** As a developer, I want to navigate between conventions using keyboard shortcuts (↑↓, j/k), so that I can review efficiently without reaching for the mouse.

**Acceptance Criteria:**
- [ ] `↑` / `k` navigates to previous convention
- [ ] `↓` / `j` navigates to next convention
- [ ] Navigation is clamped to bounds (no panic on first/last item)
- [ ] Progress counter shows current/total (e.g., "1/23")
- [ ] `y` confirms current convention
- [ ] `n` rejects current convention
- [ ] `p` marks current convention as partial
- [ ] `s` skips current convention
- [ ] `q` / `Esc` finishes review and saves all changes

### US-006: See summary after review completes

**Description:** As a developer, I want to see a summary of my review decisions after exiting the TUI, so that I can verify my work and understand the impact.

**Acceptance Criteria:**
- [ ] Summary shows confirmed/rejected/partial/skipped counts
- [ ] Summary shows precision percentage
- [ ] Summary shows status message (calibrated/low precision)
- [ ] Summary is printed to stdout after TUI exits
- [ ] Batch save indicator shown during save ("Saving...")

## Functional Requirements

### FR-1: Redesign TUI layout with new structure

The TUI must follow this exact layout:

```
┌─ Seshat Convention Review  ─────────────────────────────────────────────────────────────────────┐
│  1/23: Import grouping: stdlib → external → internal                                            │
├─────────────────────────────────────────────────────────────────────────────────────────────────┤
│  Nature: Convention    Confidence: 93%    Weight: Strong                                        │
│  Found in: 1/1 files (100% adoption)                                                            │
│                                                                                                 │
├── Example: (tools/mcp-smoke.py:63) ─────────────────────────────────────────────────────────────┤
│  import { readFile } from 'fs';                                                                 │
│  import axios from 'axios';                                                                     │
│  import { AuthService } from '../services';                                                     │
├─────────────────────────────────────────────────────────────────────────────────────────────────┤
│  [y] Confirm   [n] Reject   [p] Partial   [s] Skip   [↑↓] Navigate   [q] Finish                 │
└─────────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Header**: Title + progress + description on one line
- **Info section**: Metadata (nature, confidence, weight) + adoption stats
- **Example section**: Code block with filename:line in border title
- **Bottom bar**: All controls in one line

### FR-2: Make layout adaptive to terminal size

- Header line wraps or truncates gracefully on narrow terminals (<80 chars)
- Example code block expands to fill remaining vertical space
- All sections respect terminal boundaries
- No text goes beyond borders
- Layout re-renders on terminal resize

### FR-3: Fix terminal cleanup on exit

- Call `crossterm::terminal::disable_raw_mode()` on exit
- Call `crossterm::terminal::leave_alternate_screen()` on exit
- Use `std::panic::catch_unwind` to ensure cleanup even on panic
- Use `Drop` trait to ensure cleanup on any exit path
- Test with `q`, `Ctrl+C`, `Esc` exits

### FR-4: Fix application hangs

- Ensure all DB operations complete within reasonable time (<5s)
- Add timeout for batch save operations
- Show "Saving..." indicator during batch save
- Handle DB locked errors gracefully (show error, don't hang)
- Ensure event loop remains responsive during operations

### FR-5: Fix data persistence

- Verify code snippets are saved correctly in `record_decision`
- Verify examples are converted to `ExampleInput` structs correctly
- Verify FTS5 index is rebuilt after batch save
- Verify rejected conventions are not recreated on re-scan
- Add test for snippet persistence across TUI restarts

### FR-6: Batch save all decisions on exit

- Accumulate all actions in memory during review
- On `q`, apply all actions in a single SQLite transaction
- Show "Saving..." indicator during batch save
- Apply all actions or rollback on error
- Rebuild FTS5 index after batch save
- Show summary after successful save

### FR-7: Add comprehensive tests

- Unit tests for layout rendering
- Unit tests for terminal cleanup
- Unit tests for data persistence
- Integration tests for full review flow
- Tests with real data from codebase

## Non-Goals (Out of Scope)

- No manual save button (decisions happen immediately on key press)
- No search/filter functionality (out of scope for this fix)
- No multi-branch support (only current branch)
- No MCP integration (TUI calls graph functions directly)
- No UI animations or transitions
- No color themes or customization
- No progress bar for batch save (just "Saving..." text)

## Design Considerations

### TUI Layout Design

**Header Section:**
- Title: "Seshat Convention Review"
- Progress: "1/23"
- Description: Convention name + description
- All on one line, separated by spaces
- Wraps or truncates gracefully on narrow terminals

**Info Section:**
- Metadata: Nature, Confidence, Weight on one line
- Adoption stats: "Found in: X/Y files (Z% adoption)" on next line
- Empty line for spacing
- All text respects terminal boundaries

**Example Section:**
- Border title: "Example: (filename:line)"
- Code block with line numbers
- Highlighted lines (line to end_line) in Green+Bold
- Non-highlighted lines in Yellow
- Takes remaining vertical space

**Bottom Bar:**
- All controls in one line
- Bold key names: [y] Confirm, [n] Reject, etc.
- Arrow keys and vim-style navigation
- Takes fixed 3 rows (border + content + border)

### Color Scheme

- **Header**: Cyan borders
- **Example section**: Yellow borders
- **Metadata**: White text
- **Code block**: Green+Bold for highlighted lines, Yellow for non-highlighted
- **Bottom bar**: DarkGray borders, White text

### Adaptive Behavior

- **Narrow terminals (<80 chars)**: Header wraps, code block truncates long lines
- **Short terminals (<24 rows)**: Example section shrinks, may need scroll (future)
- **Wide terminals (>160 chars)**: All sections expand horizontally
- **Tall terminals (>60 rows)**: Example section expands vertically

## Technical Considerations

### Terminal Management

**Current Issues:**
- Terminal not restored on exit (control characters remain)
- No cleanup on panic
- No cleanup on Ctrl+C
- No cleanup on Ctrl+Z

**Fix Approach:**
```rust
// Use RAII pattern for terminal cleanup
struct TerminalGuard {
    terminal: DefaultTerminal,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Restore terminal state
        ratatui::shutdown();
        // Ensure raw mode is disabled
        // Ensure alternate screen is left
    }
}
```

### Data Persistence

**Current Issues:**
- Code snippets may not be saved correctly
- Examples may not be converted to `ExampleInput` correctly
- FTS5 index may not be rebuilt after batch save

**Fix Approach:**
1. Verify `record_decision` receives all examples
2. Verify examples are converted to `ExampleInput` structs
3. Verify `delete_fts_entry` is called for rejected nodes
4. Verify `rebuild_fts_index` is called after batch save
5. Add test for snippet persistence across TUI restarts

### Error Handling

**Current Issues:**
- No timeout for batch save operations
- No graceful handling of DB locked errors
- No indication of saving progress

**Fix Approach:**
1. Add timeout for batch save (<5s)
2. Show "Saving..." indicator during batch save
3. Handle DB locked errors gracefully (show error, don't hang)
4. Rollback transaction on error
5. Show summary even if some operations fail (with warnings)

### Testing Strategy

**Unit Tests:**
- Layout rendering (widgets.rs)
- Terminal cleanup (app.rs)
- Data persistence (app.rs)
- Key handling (review_wizard.rs)
- Navigation (app.rs)
- Batch save (app.rs)

**Integration Tests:**
- Full review flow (run TUI → confirm/reject → exit → verify DB)
- Terminal cleanup (run TUI → exit → verify terminal state)
- Data persistence (run TUI → exit → restart → verify decisions exist)
- Persisted rejection (reject → re-scan → verify not recreated)
- Real data tests (use actual conventions from codebase)

**Edge Case Tests:**
- Empty conventions list
- Single convention
- Very long descriptions
- Very long filenames
- No code examples
- Very large convention sets (100+ items)
- DB locked during save
- Terminal resize during review

## Success Metrics

- **UI**: No overlapping text, no cramped spacing, all sections visible
- **Terminal**: No control characters after exit, shell prompt clean
- **Responsiveness**: No hangs or unresponsive state
- **Data**: All decisions persisted correctly, no snippets disappearing
- **Tests**: 100% coverage for new code, no regressions in existing tests

## Open Questions

1. Should we add a "Save" button/key to manually save during review? (No, batch save on exit is sufficient)
2. What happens if the batch save fails (e.g., DB locked)? (Show error, don't hang, rollback transaction)
3. Should we add a progress indicator for saving decisions? (Just "Saving..." text is sufficient)
4. Should we add a confirmation dialog before saving? (No, pressing `q` implies save)
5. Should we add a "discard changes" option? (No, pressing `q` implies save)

## File List

```
crates/seshat-cli/src/tui/widgets.rs          ← MODIFY: Redesign layout with new structure
crates/seshat-cli/src/tui/app.rs              ← MODIFY: Fix terminal cleanup, data persistence
crates/seshat-cli/src/tui/review_wizard.rs    ← MODIFY: Fix event loop, add timeout
crates/seshat-cli/src/tui/mod.rs              ← MODIFY: Update exports if needed
crates/seshat-cli/tests/tui_integration.rs    ← CREATE: Integration tests for TUI
```

## Test Plan

### Unit Tests (widgets.rs)

1. `render_header_shows_progress_and_description` — Header shows progress (1/23) and description
2. `render_info_section_shows_metadata` — Info section shows nature, confidence, weight, adoption stats
3. `render_example_section_shows_code` — Example section shows code block with filename:line
4. `render_bottom_bar_shows_controls` — Bottom bar shows all controls
5. `layout_adapts_to_narrow_terminal` — Layout wraps/truncates on narrow terminals (<80 chars)
6. `layout_adapts_to_short_terminal` — Layout shrinks on short terminals (<24 rows)
7. `layout_adapts_to_wide_terminal` — Layout expands on wide terminals (>160 chars)
8. `layout_adapts_to_tall_terminal` — Layout expands on tall terminals (>60 rows)

### Unit Tests (app.rs)

9. `terminal_cleanup_on_drop` — TerminalGuard drops and restores terminal state
10. `terminal_cleanup_on_panic` — TerminalGuard drops even on panic
11. `batch_save_applies_all_actions` — All actions applied in single transaction
12. `batch_save_rolls_back_on_error` — Transaction rolled back on error
13. `batch_save_rebuilds_fts_index` — FTS5 index rebuilt after batch save
14. `confirm_action_creates_user_decision` — Confirm creates new user-decision node
15. `reject_action_marks_user_rejected` — Reject marks node as user_rejected
16. `partial_action_creates_preference` — Partial creates preference node
17. `skip_action_does_nothing` — Skip does nothing
18. `snippet_persistence_across_restart` — Code snippets persist across TUI restarts

### Unit Tests (review_wizard.rs)

19. `handle_key_y_confirms` — Press y, verify ReviewAction::Confirm pushed
20. `handle_key_n_rejects` — Press n, verify ReviewAction::Reject pushed
21. `handle_key_p_partial` — Press p, verify ReviewAction::Partial pushed
22. `handle_key_s_skips` — Press s, verify ReviewAction::Skip pushed
23. `handle_key_q_quits` — Press q, verify quit=true
24. `handle_key_esc_quits` — Press Esc, verify quit=true
25. `handle_key_up_down_navigates` — Press ↑↓, verify current_index changes
26. `handle_key_j_k_navigates` — Press j/k, verify current_index changes
27. `navigation_clamped_to_bounds` — Navigation clamped to first/last item
28. `batch_save_timeout_does_not_hang` — Batch save times out after 5s

### Integration Tests

29. `full_review_flow` — Run TUI → confirm/reject → exit → verify DB
30. `terminal_cleanup_on_exit` — Run TUI → exit → verify terminal state
31. `data_persistence_across_restart` — Run TUI → exit → restart → verify decisions exist
32. `persisted_rejection` — Reject → re-scan → verify not recreated
33. `empty_conventions_exits_gracefully` — No conventions, verify clean exit
34. `single_convention_works` — Single convention, verify review works
35. `db_locked_shows_error` — DB locked, verify error shown (no hang)
36. `terminal_resize_re_renders` — Resize terminal, verify re-render
37. `real_data_review` — Use actual conventions from codebase, verify review works

### Edge Case Tests

38. `very_long_description_wraps` — Very long description wraps gracefully
39. `very_long_filename_truncates` — Very long filename truncates gracefully
40. `no_code_examples_hides_section` — No code examples, hides example section
41. `large_convention_set_works` — 100+ conventions, verify review works
42. `multiple_concurrent_tui_sessions` — Multiple TUI sessions don't interfere

### Real-Data Tests (Seshat Repository)

**These tests must run against the actual seshat repository, not synthetic data:**

43. `scan_seshat_and_query_conventions` — Run `seshat scan` on the seshat repository, verify conventions are created
44. `query_conventions_for_review_real_data` — Query conventions for review, verify data is populated correctly
45. `review_tui_with_real_conventions` — Run TUI with real conventions from seshat, verify navigation works
46. `confirm_real_convention` — Confirm a real convention, verify decision is saved correctly
47. `reject_real_convention` — Reject a real convention, verify it's marked as user_rejected
48. `persisted_rejection_real_data` — Reject convention → re-scan → verify not recreated
49. `ft5_index_updated_real_data` — Reject convention → verify FTS5 index is updated
50. `full_review_flow_real_data` — Full review flow with real conventions, verify all actions work

## Quality Gates

**No story is considered complete until ALL quality gates pass:**

### Code Quality Gates

1. **`cargo fmt --check`** — All code must be formatted with rustfmt
2. **`cargo clippy --all-targets --all-features -- -D warnings`** — No clippy warnings or errors
3. **`cargo build --all-targets`** — No compilation errors or warnings
4. **`cargo test --all-targets --all-features`** — All tests pass
5. **`cargo test --all-targets --all-features -- --ignored`** — All ignored tests pass (if any)

### Code Review Gates

6. **KSD-CodeReview** — Thorough code review using KSD-CodeReview skill:
   - Blind Hunter pass: Review code adversarially for bugs, edge cases, security issues
   - Edge Case Hunter pass: Exhaustively enumerate unhandled edge cases
   - Acceptance Auditor pass: Verify all acceptance criteria are met
   - Structured triage into actionable categories (bugs, improvements, suggestions)
7. **Remove duplicates** — No duplicate code, no redundant logic
8. **Remove errors** — No compilation errors, no runtime panics
9. **Remove non-idiomatic Rust** — Follow Rust best practices, use idiomatic patterns
10. **Remove warnings** — No compiler warnings, no clippy warnings

### Integration Gates

11. **Real-data validation** — All tests pass with real data from seshat repository
12. **Terminal cleanup validation** — Terminal is restored correctly after TUI exit
13. **Data persistence validation** — Decisions persist correctly across TUI restarts
14. **Persisted rejection validation** — Rejected conventions are not recreated on re-scan

### Documentation Gates

15. **PRD updated** — All changes documented in PRD
16. **Stories updated** — All user stories have updated acceptance criteria
17. **References updated** — All file references are accurate

## Definition of Done

**A story is only "Done" when ALL of the following are true:**

- [ ] All acceptance criteria are met
- [ ] `cargo fmt --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo build --all-targets` passes with no warnings
- [ ] `cargo test --all-targets --all-features` passes
- [ ] KSD-CodeReview completed with no critical issues
- [ ] All duplicates removed
- [ ] All errors fixed
- [ ] All non-idiomatic Rust removed
- [ ] All warnings fixed
- [ ] Real-data tests pass (seshat repository)
- [ ] Terminal cleanup validated
- [ ] Data persistence validated
- [ ] Persisted rejection validated
- [ ] PRD updated with all changes
- [ ] Stories updated with all acceptance criteria
- [ ] References updated with all file paths
- [ ] No regressions in existing tests
- [ ] Code is idiomatic Rust
- [ ] No dead code, no unused imports
- [ ] All public APIs have documentation
- [ ] All error messages are clear and actionable

## References

- Story: `_bmad-output/implementation-artifacts/story-12-1-tui-review-wizard.md`
- UX Design: `_bmad-output/planning-artifacts/ux-design-specification.md#L139-L217`
- Architecture: `_bmad-output/planning-artifacts/architecture.md#L149`
- Current TUI code: `crates/seshat-cli/src/tui/`
- Decision persistence: `crates/seshat-cli/src/tui/app.rs` (lines 270-456)
- Layout rendering: `crates/seshat-cli/src/tui/widgets.rs` (lines 71-214)
- Event loop: `crates/seshat-cli/src/tui/review_wizard.rs` (lines 12-52)
