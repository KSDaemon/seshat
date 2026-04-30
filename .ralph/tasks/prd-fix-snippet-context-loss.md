# PRD: Fix Convention Evidence Snippets — Context Loss and Overwrite Bug

## Introduction

**Type:** Fix

Convention evidence snippets displayed in the TUI "Seshat Convention Review" are often uninformative: they start mid-statement, show bare import lines instead of actual usage, or contain code unrelated to the convention being described. This defeats the callsite-cross-reference feature, which was designed to replace import-line evidence with actual call-site evidence. This fix restores meaningful, contextual code snippets for all convention evidence.

## Goals

- Preserve pre-populated call-site snippets with context (2 lines before + call + 4 lines after)
- Add leading context to empty snippets (macros, import-line evidence)
- Introduce `snippet_start_line` field for correct TUI line numbering
- Ensure backward compatibility with existing JSON artifacts
- Fix `file_structure` detector to show meaningful snippets

## User Stories

### US-001: Stop overwriting pre-populated snippets in detect_with_source
**Description:** As a developer reviewing conventions, I want call-site snippets with context to be preserved so the evidence is meaningful.

**Acceptance Criteria:**
- [ ] `detect_with_source` only extracts snippet from source when `evidence.snippet` is empty
- [ ] Add `&& evidence.snippet.is_empty()` guard to condition in `trait_def.rs`
- [ ] Existing snippets from `find_usage_evidence_for_file_scoped` are preserved
- [ ] Tests pass: `detect_with_source_preserves_pre_populated_snippet`
- [ ] Tests pass: `detect_with_source_fills_empty_snippet_from_source`
- [ ] Tests pass: `detect_with_source_mixed_pre_populated_and_empty_evidence`
- [ ] Typecheck passes (cargo check + cargo clippy)

### US-002: Add snippet_start_line field to CodeEvidence
**Description:** As a developer, I need a `snippet_start_line` field so snippets can include leading context with correct TUI line numbering.

**Acceptance Criteria:**
- [ ] Add `snippet_start_line: usize` field to `CodeEvidence` in `detector_result.rs`
- [ ] `#[serde(default)]` on `snippet_start_line` for backward compatibility
- [ ] Old JSON without `snippet_start_line` deserializes with default value 0
- [ ] Test passes: `snippet_start_line_backward_compat_deserialization`
- [ ] Typecheck passes

### US-003: Include leading context when filling empty snippets
**Description:** As a developer reviewing macro calls or import-line evidence, I want 2 lines of leading context so the snippet is meaningful.

**Acceptance Criteria:**
- [ ] `detect_with_source` extracts snippet starting `EVIDENCE_CONTEXT_BEFORE` (2 lines) before `evidence.line`
- [ ] `snippet_start_line` is set to the actual start line when context is added
- [ ] Test passes: `detect_with_source_includes_context_before_for_empty_snippets`
- [ ] Typecheck passes

### US-004: Update TUI rendering to use snippet_start_line
**Description:** As a TUI user, I want correct line numbers when snippets include leading context lines.

**Acceptance Criteria:**
- [ ] `widgets.rs` computes `snippet_start` from `snippet_start_line` (falls back to `evidence.line` if 0)
- [ ] Line numbers in rendered snippet start from `snippet_start`
- [ ] Context lines (before `evidence.line`) render as yellow, highlight lines (at/after `evidence.line`) render as green
- [ ] `CodeExample` struct in `app.rs` includes `snippet_start_line: u32`
- [ ] `parse_evidence()` reads `snippet_start_line` from JSON
- [ ] Test passes: `code_example_uses_snippet_start_line_for_line_numbers`
- [ ] Typecheck passes

### US-005: Update all intermediate structs and construction sites
**Description:** As a developer, I need all intermediate structs and CodeEvidence construction sites updated to include `snippet_start_line`.

**Acceptance Criteria:**
- [ ] `EvidenceExample` in `conventions.rs` has `snippet_start_line: usize`
- [ ] `convention_to_node()` in `detection.rs` serializes `snippet_start_line`
- [ ] All `CodeEvidence { ... }` construction sites across all crates set `snippet_start_line: 0`
- [ ] Typecheck passes

### US-006: Populate macro call snippets in usage_evidence
**Description:** As a developer, I want macro calls like `info!()` or `assert!()` to have snippets populated, not left empty.

**Acceptance Criteria:**
- [ ] `MacroCall` → `FunctionCall` conversion sets `snippet` from source lines instead of `String::new()`
- [ ] Macro call snippets include 2 lines of leading context
- [ ] Integration test passes: `logging_detector_produces_call_site_snippets_with_context`
- [ ] Typecheck passes

### US-007: Integration verification with real detectors
**Description:** As a tester, I want end-to-end tests with real detectors to verify the fix works in practice.

**Acceptance Criteria:**
- [ ] Integration test passes: `dependency_usage_detector_produces_call_site_snippets_with_context`
- [ ] Integration test passes: `test_patterns_detector_shows_test_annotation_in_snippet`
- [ ] Manual TUI verification: rusqlite evidence shows function signatures + call
- [ ] Manual TUI verification: tracing_subscriber shows full chain
- [ ] Manual TUI verification: serde shows derive + struct body
- [ ] Manual TUI verification: line numbers are correct
- [ ] `cargo test` passes all tests
- [ ] `cargo clippy` passes with no new warnings

## Functional Requirements

- FR-1: `detect_with_source` must check `evidence.snippet.is_empty()` before overwriting
- FR-2: `CodeEvidence` must have `snippet_start_line: usize` with `#[serde(default)]`
- FR-3: Empty snippets must be filled from source with 2 lines of leading context
- FR-4: TUI rendering must use `snippet_start_line` for line number calculation
- FR-5: All intermediate structs (`CodeExample`, `EvidenceExample`) must carry `snippet_start_line`
- FR-6: All `CodeEvidence` construction sites must set `snippet_start_line: 0`
- FR-7: `MacroCall` → `FunctionCall` conversion must include non-empty snippets with context

## Non-Goals

- Fix 3 (file_structure detector snippets) is deferred — requires design discussion before implementation
- No changes to the scanner's `build_call_snippet_from_lines` logic (2 before / 4 after stays as-is)
- No changes to how `find_usage_evidence_for_file_scoped` builds snippets

## Technical Considerations

- **Backward compatibility:** `#[serde(default)]` ensures old JSON without `snippet_start_line` still deserializes
- **Files affected:** `trait_def.rs`, `detector_result.rs`, `widgets.rs`, `app.rs`, `conventions.rs`, `detection.rs`, `usage_evidence.rs`, and all `CodeEvidence` construction sites
- **Blast radius:** `CodeEvidence` is used across multiple crates; all construction sites need updating
- **Risk (Fix 1):** Low — one-line guard addition
- **Risk (Fix 2):** Medium — schema change across 5-6 files with downstream struct updates

## Success Metrics

- rusqlite evidence shows full function signature + `db.query_row(...)` call, not just `params![branch_id],`
- tracing_subscriber evidence shows `fmt().with_env_filter(...).init()` chain
- serde evidence shows `#[derive(Serialize, Deserialize)]` with struct body
- Logging evidence shows `info!("...")` with function context
- Test evidence shows `#[test]` + `fn test_foo()` + `assert!`
- Line numbers in TUI are correct for all snippets with leading context
- ~70% of reported issues resolved by Fix 1 alone
- All `cargo test` and `cargo clippy` checks pass

## Open Questions

- What should `file_structure` detector snippets show for "By-feature directory organization"? (deferred to Fix 3)
  - Options: representative file's first N lines, directory tree structure, or specific well-named file example
