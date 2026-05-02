# PRD: TUI Search/Filter & Precision Diagnostic

## 1. Introduction/Overview

**Type:** Feature

A follow-up to Epic 12 (Interactive Convention Review TUI). Core `seshat review` functionality already works: all review actions (y/n/p/s), navigation, scrolling through examples, post-review summary. Two gaps from Story 12.2 remain:

1. **Search/Filter** — the current implementation has no way to find a convention by keyword. A developer is forced to scroll through 50+ conventions with arrow keys to find "error handling" among them all.
2. **Precision Diagnostic** — the summary already shows `Session precision: XX%`, but there is no interpretation — whether Seshat is calibrated or its detectors need improvement.

---

## 2. Goals

- Add real-time search with fuzzy matching over convention descriptions
- Implement `/` → filter input → `Enter` lock / `Esc` clear
- Show an empty state when there are no matches
- Add a diagnostic message after the summary: `>=70% calibrated` / `<70% low precision`

---

## 3. User Stories

### US-001: Search/filter with fuzzy matching

**Description:** As a developer, I want to type a keyword to find conventions instantly, with tolerance for typos, so I can quickly locate specific conventions among dozens.

**Acceptance Criteria:**

- [ ] `/` enters search mode — bottom row shows filter input bar
- [ ] Typing filters conventions in real time by `description` field
- [ ] **Fuzzy matching**: filter matches even with minor typos (e.g., "err" matches "error handling", "loging" matches "logging") using Levenshtein distance ≤ 2
- [ ] Case-insensitive: "ERROR" matches "error handling"
- [ ] `Backspace` removes last character; when filter becomes empty, exits search mode
- [ ] `Enter` locks filter (exits search mode, keeps filter active, hides input bar)
- [ ] `Esc` in search mode cancels search (clears filter, exits, restores full list)
- [ ] `Esc` when filter is locked unlocks the filter; `Esc` in normal mode quits the app
- [ ] `y/n/p/s/q` keys in search mode are appended as filter characters, not executed as actions
- [ ] `/` in search mode is ignored; `/` when filter is locked resets the filter and re-enters search mode
- [ ] Navigation (`j/k/↑/↓`) walks only through filtered subset
- [ ] `current/total` counter reflects filtered count
- [ ] **Empty state**: when no conventions match filter, TUI shows "No matching conventions" (gray text)
- [ ] When filter locked (not empty + not in search mode), header shows `[filter: "keyword"]` indicator
- [ ] `[/] Search` shown in key bindings help row
- [ ] Typecheck passes: `cargo check -p seshat-cli`

### US-002: Precision Diagnostic Message

**Description:** As a developer, I want to know whether Seshat is well-calibrated after my review, so I can decide if AI agents should trust the detected conventions.

**Acceptance Criteria:**

- [ ] After `q` → summary, right after `Session precision: XX%`, a diagnostic line prints:
  - `>= 70%` → `Precision diagnostic:  calibrated — detected conventions are well-aligned`
  - `< 70%` → `Precision diagnostic:  low precision — consider re-reviewing flagged conventions`
- [ ] 70% is inclusive threshold (69% → warning, 70% → calibrated)
- [ ] Works correctly for edge cases: 0 decisions (all skipped), all confirmed, all rejected
- [ ] Typecheck passes

---

## 4. Functional Requirements

| FR | Description |
|----|-------------|
| FR-1 | `/` enters search mode in TUI |
| FR-2 | In search mode, key presses are accumulated in `filter_query` |
| FR-3 | Filter is case-insensitive substring match + fuzzy fallback |
| FR-4 | `Backspace` removes the last character of the filter |
| FR-5 | Empty filter = exit search mode, show all conventions |
| FR-6 | `Enter` exits search mode, keeping the filter active |
| FR-7 | `Esc` exits search mode, clearing the filter |
| FR-8 | `y/n/p/s/q` in search mode are appended to filter query; review actions blocked when filter is locked |
| FR-9 | Navigation, counter, and `advance_to_next_unreviewed` respect the filter |
| FR-10 | When 0 matches, empty state is displayed |
| FR-11 | When filter is active (locked), the header shows an indicator |
| FR-12 | Summary after `q` includes a diagnostic message based on precision |
| FR-13 | Fuzzy matching: Levenshtein distance ≤ 2 for substrings up to 10 chars; pure substring match for longer |

---

## 5. Non-Goals (Out of Scope)

- Cross-session filter persistence
- Keeping active filter when exiting via Ctrl+C
- Regex search (`/error\d+/`)
- Search by file paths or snippet content (only `description`)
- CLI arguments for `seshat review` (launching with a pre-set filter)
- Search history / recent searches
- Highlighting matching text in results (highlighting keyword in description)

---

## 6. Design Considerations

**Search bar layout:**
```
Filter: █ err_handler  (Esc clear, Enter lock)
```
- Rendered in place of the bottom controls row
- Color: cyan, bold
- The `█` character is a static cursor (non-blinking, to avoid complexity)

**Active filter indicator (header):**
```
1/12: logging is done via tracing  [filter: "log"]
```
- Suffix in brackets, cyan color

**Empty state:**
- Centered in the convention area, gray text: `No matching conventions`

**Precision diagnostic (in summary after q, stdout):**
```
Session precision:    78%
Precision diagnostic:  calibrated — detected conventions are well-aligned
Overall coverage:     92%
```
- When `< 70%`: `Precision diagnostic:  low precision — consider re-reviewing flagged conventions`
- Diagnostic prints immediately after `Session precision` on the same indentation level

---

## 7. Technical Considerations

**Fuzzy matching:**

Implementation: built-in Levenshtein. Function `fuzzy_match(needle: &str, haystack: &str) -> bool` with a distance constraint ≤ 2. The Levenshtein algorithm is ~20 lines of code. No new dependencies.

**Filtering algorithm:**
```
1. substring (case-insensitive) → if match, return true (fast path)
2. needle word >= 4 chars → compute levenshtein ≤ 2 against windows of haystack of the same length
3. otherwise → substring only
```

**No DB changes, no new files. Only `app.rs`, `review_wizard.rs`, `widgets.rs`.**

---

## 8. Success Metrics

- Finding a convention by keyword takes ≤ 5 key presses (instead of scrolling through 50+ with arrow keys)
- Fuzzy matching finds a convention even with 1-2 typos
- 42 unit tests cover all key scenarios, including 10 edge cases
- Precision diagnostic unambiguously shows calibration quality
- No regressions in existing tests (`cargo test` 100% green)

---

## 9. Open Questions

_None._ All questions were resolved during discussion.
