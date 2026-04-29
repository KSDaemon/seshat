# PRD: Cross-Reference Import Symbols to Function Call Sites for Evidence Snippets

## Introduction

**Type:** Feature

Currently, convention evidence snippets point to `use`/`import` lines, showing only the import statement (e.g., `use tracing::{info, warn, error};`). This is uninformative — it doesn't show AI agents how the library is actually used. The IR already captures `FunctionCall` entries (callee name, line range, 6-30 line snippet) for all four languages (Rust, TypeScript, JavaScript, Python), but no detector uses them for evidence.

This feature adds a new utility module `usage_evidence.rs` that cross-references imported symbol names from `Import.names` with `FunctionCall.callee` to find actual call-site snippets. A distinct-by-callee dedup ensures diversity (e.g., `info`, `warn`, `error` each shown once, not 10x `info`).

## Goals

- Replace import-line evidence with actual call-site evidence where possible
- Support all 4 languages: Rust, TypeScript, JavaScript, Python
- Deduplicate by callee name to maximize diversity of examples shown
- Maximize informativeness: show `logger.info(...)`, `logger.warn(...)`, `logger.error(...)` rather than 3x `logger.info(...)`
- Fall back to import-line evidence when no call sites are found

## User Stories

### US-001: Create `usage_evidence.rs` utility module
**Description:** As a developer, I need a reusable function that takes imports and function calls, and returns call-site evidence.

**Acceptance Criteria:**
- [ ] New file `crates/seshat-detectors/src/usage_evidence.rs`
- [ ] Public function `find_usage_evidence(imports: &[Import], function_calls: &[FunctionCall], file_path: &Path, max: usize) -> Vec<CodeEvidence>`
- [ ] Matches each `FunctionCall.callee` against `Import.names` from imports that share the same top-level package/module
- [ ] Deduplicates by callee name — only one `CodeEvidence` per unique callee
- [ ] Returns empty vec if no matches found
- [ ] Respects `max` parameter for limiting results
- [ ] Includes `line`, `end_line`, and `snippet` from the `FunctionCall`
- [ ] Typecheck and lint pass

### US-002: Unit tests for `find_usage_evidence` with mocked data
**Description:** As a developer, I need comprehensive unit tests to verify the cross-reference logic works correctly for all edge cases.

**Acceptance Criteria:**
- [ ] Test: basic import-to-call match (e.g., `Import { names: ["info"] }` matches `FunctionCall { callee: "info" }`)
- [ ] Test: no match when callee not in import names
- [ ] Test: dedup by callee — two `info` calls produce one evidence entry
- [ ] Test: diverse callees preserved — `info`, `warn`, `error` each produce separate entries
- [ ] Test: max limit respected — only N results returned
- [ ] Test: empty imports or empty function_calls returns empty vec
- [ ] Test: multiple imports from same module — all names are considered
- [ ] Test: cross-module mismatch — callee from module A doesn't match import from module B
- [ ] Typecheck and lint pass

### US-003: Extend `find_usage_evidence` for all 4 languages
**Description:** As a developer, I need the function to work for Rust, TypeScript, JavaScript, and Python IR types.

**Acceptance Criteria:**
- [ ] Rust: matches against `RustIR::function_calls` (and `macro_calls` for Rust-specific macros like `info!`)
- [ ] TypeScript: matches against `TypeScriptIR::function_calls`
- [ ] JavaScript: matches against `JavaScriptIR::function_calls`
- [ ] Python: matches against `PythonIR::function_calls`
- [ ] Language-agnostic wrapper: `find_usage_evidence_for_file(file: &ProjectFile, max: usize) -> Vec<CodeEvidence>`
- [ ] Unit tests per language with mocked data
- [ ] Typecheck and lint pass

### US-004: Integrate into `logging_observability` detector
**Description:** As a developer, I want logging evidence to show actual `info!(...)`/`warn!(...)`/`logger.info(...)` call sites instead of `use tracing::{...}` import lines.

**Acceptance Criteria:**
- [ ] `detect_rust`: replace `macro_call_evidence` fallback path with `find_usage_evidence_for_file` call
- [ ] `detect_typescript`: use `find_usage_evidence_for_file` instead of `import_evidence`
- [ ] `detect_javascript`: use `find_usage_evidence_for_file` instead of `import_evidence`
- [ ] `detect_python`: use `find_usage_evidence_for_file` instead of `import_evidence`
- [ ] Existing unit tests still pass
- [ ] New unit test: detector produces call-site evidence for a mocked file with tracing imports and info!/warn!/error! macro calls
- [ ] New unit test: TypeScript file with `import winston` shows `logger.info(...)` instead of import line
- [ ] Typecheck and lint pass

### US-005: Integrate into `test_patterns` detector
**Description:** As a developer, I want test pattern evidence to show actual `expect(...).toBe(...)` or `assert!` call sites instead of `use assert!` import lines.

**Acceptance Criteria:**
- [ ] `function_evidence`: use `find_usage_evidence_for_file` to find actual assertion/test function calls
- [ ] `import_evidence` in test_patterns: fall back to call-site evidence when imports of test frameworks detected
- [ ] Existing unit tests still pass
- [ ] New unit test: TS file with Jest imports shows `expect(...).toBe(...)` call sites
- [ ] New unit test: Rust file with `#[test]` functions shows `assert!(...)` call sites
- [ ] Typecheck and lint pass

### US-006: Integrate into `error_handling` detector
**Description:** As a developer, I want error handling evidence to show actual `Err(...)` or `thiserror::Error` derive usage instead of `use thiserror::Error`.

**Acceptance Criteria:**
- [ ] `build_rust_error_evidence`: cross-reference `thiserror`/`anyhow` imports with `function_calls` to find error construction sites
- [ ] TypeScript/JavaScript/Python: find error throwing or error class construction call sites
- [ ] Existing unit tests still pass
- [ ] New unit test: Rust file shows `Err(DatabaseError::...)` call site
- [ ] Typecheck and lint pass

### US-007: Integrate into `dependency_usage` detector
**Description:** As a developer, I want dependency evidence to show actual library API calls instead of `use reqwest::Client`.

**Acceptance Criteria:**
- [ ] `detect` method: replace import-line evidence with call-site evidence via `find_usage_evidence_for_file`
- [ ] Dedup by callee ensures diverse examples per dependency
- [ ] Existing unit tests still pass
- [ ] New unit test: reqwest import shows `.get(url).send().await` call site
- [ ] Typecheck and lint pass

### US-008: Wire into `detect_with_source` in `trait_def.rs`
**Description:** As a developer, I want the `detect_with_source` template method to automatically upgrade evidence line numbers and snippets with call-site data when available.

**Acceptance Criteria:**
- [ ] `detect_with_source` calls `find_usage_evidence_for_file` after `detect()` to attempt call-site upgrade
- [ ] Evidence items that matched a call site get updated `line`, `end_line`, `snippet`
- [ ] Evidence items with no call site match retain original import-line data
- [ ] Line 0 (file-level) evidence is never modified
- [ ] Existing test `pipeline_uses_detect_with_source_when_source_present` still passes
- [ ] New test verifies upgrade from import line to call-site line
- [ ] Typecheck and lint pass

## Functional Requirements

- FR-1: `find_usage_evidence` matches `FunctionCall.callee` against `Import.names` for the same module/package
- FR-2: Dedup by callee — one evidence entry per unique callee name
- FR-3: Result limited to `max` entries (default: `MAX_EVIDENCE = 5`)
- FR-4: Falls back to empty vec if no call sites found (detectors handle fallback to import evidence)
- FR-5: Language-agnostic wrapper handles all 4 IR types
- FR-6: Rust `MacroCall` entries are also considered as call sites (for `info!`, `warn!`, `assert!`, etc.)
- FR-7: Evidence `snippet` is copied from `FunctionCall.snippet` (pre-populated with 6-30 lines of context)
- FR-8: `detect_with_source` integration is opt-in per detector (not automatic override)

## Non-Goals

- No semantic analysis — matching is purely string-based on callee name vs import names
- No disambiguation of bare names across multiple imports (e.g., `info` from both `tracing` and `log`)
- No runtime performance benchmarking
- No changes to the IR parsing or storage layer
- No changes to the UI/CLI display layer — only evidence quality improvement
- No support for wildcard imports (`use foo::*`) — these can't be matched precisely

## Technical Considerations

- `Import.names` already captures individual imported symbols for all 4 languages
- `FunctionCall` already exists in IR with `callee`, `line`, `end_line`, `snippet`
- `MacroCall` (Rust-only) has `name`, `line` — needs to be treated alongside `FunctionCall` for Rust
- `MAX_EVIDENCE` constant is already defined (value: 5)
- Existing test helpers: `make_import()`, `make_function()`, `make_dep()`, `make_*_file_with_ir()`
- Test approach: unit tests with mocked `ProjectFile` + manual IR construction, no actual parsing required
- No new dependencies required

## Success Metrics

- Evidence snippets contain actual usage code (function calls, macro invocations) instead of import statements
- At least 3 distinct callee names appear in evidence when available (vs 1 import line repeated)
- Zero regressions in existing test suite
- All 4 languages produce meaningful call-site evidence in unit tests

## Open Questions

- Should we prioritize rarer callees over common ones (e.g., `error` > `info`)? Currently: first-match-wins.
- Should `MacroCall` in Rust be treated identically to `FunctionCall`, or require separate handling?
- What's the fallback when `Import.names` contains `["*"]` (wildcard import)? Currently: no match attempted.
