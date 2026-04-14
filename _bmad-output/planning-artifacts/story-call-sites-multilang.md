# Story: Call-Site Snippets — Multi-Language Support (TypeScript, JavaScript, Python)

**Status:** Ready for implementation
**Priority:** High
**Branch:** `feat/call-sites` (continue from existing branch — Rust already done)
**Depends on:** `story-query-code-pattern-call-sites.md` (Rust phase — COMPLETE, commit `84ff359`)

---

## Context

Rust call-site support is complete and live (IR v6, `feat/call-sites` branch).
This story extends the same feature to the three remaining supported languages:
TypeScript, JavaScript, and Python.

### What already exists (do NOT re-implement)

- `FunctionCall { callee, line, end_line, snippet }` struct — `seshat-core::ir` ✅
- `FunctionCall` exported from `seshat-core::lib.rs` ✅
- `build_call_snippet(source, line, end_line) -> String` — currently private in `rust_parser.rs`, must be moved to `parser/mod.rs`
- `collect_calls_bfs` pattern — currently private in `rust_parser.rs` as `collect_call_expressions_recursive`, must be extracted to shared helper
- `enrich_with_call_sites` in `code_pattern.rs` — exists, currently only handles `LanguageIR::Rust`
- `callee_matches_name(callee, name)` — exists in `code_pattern.rs`, works unchanged for all languages
- `PatternResult.call_sites` + `PatternResult.call_site_count` — already in MCP response ✅

---

## Architecture Decisions (party mode discussion, 2026-04-14)

| Question | Decision | Rationale |
|----------|----------|-----------|
| Cross-language call-sites (TS fn called from JS file) | **DEFER** | Requires import graph resolution through type system; out of scope |
| Optional chaining in TypeScript (`foo?.()`) | **CAPTURE callee** | Skipping silently drops real call-sites; extract callee from inner expression |
| `require('module')` in JavaScript `function_calls` | **FILTER OUT** | Already in `JavaScriptIR.require_calls`; duplicating into `function_calls` is noise for agents |
| Shared helpers location | **`parser/mod.rs`** | Prevents per-language drift in snippet window sizes and BFS logic |
| TS/JS extractor sharing | **Duplicate** (small function) | Avoids coupling between two parser files; `extract_ts_js_call` appears in both |
| Python call node name | **`"call"`** | Python tree-sitter grammar uses `"call"`, not `"call_expression"` |

---

## IR Schema Changes

### `IR_SCHEMA_VERSION`: v6 → v7

Version history:
- v1: initial schema
- v2: `Function::parameters`, `ScanConfig::exclude_patterns`
- v3: `file_doc`, `Function::doc_comment`, `TypeDef::doc_comment`
- v4: `dependencies_used` populated for all four language parsers
- v5: `RustIR::mod_declarations` → `Vec<ModDeclaration>`; `RustIR::macro_calls` added
- v6: `RustIR::function_calls: Vec<FunctionCall>` added
- **v7: `TypeScriptIR::function_calls`, `JavaScriptIR::function_calls`, `PythonIR::function_calls` added**

### New fields (`#[serde(default)]` for backward compat with v6 blobs)

```rust
// seshat-core/src/ir.rs

pub struct TypeScriptIR {
    pub has_barrel_exports: bool,
    pub type_only_imports: Vec<String>,
    pub decorators: Vec<String>,
    pub default_export: bool,
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,   // NEW in v7
}

pub struct JavaScriptIR {
    pub module_system: ModuleSystem,
    pub has_module_exports: bool,
    pub require_calls: Vec<String>,
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,   // NEW in v7
}

pub struct PythonIR {
    pub has_all_export: bool,
    pub is_init_file: bool,
    pub type_hints_used: bool,
    pub decorators: Vec<String>,
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,   // NEW in v7
}
```

---

## Shared Helpers: `parser/mod.rs`

Move from `rust_parser.rs` (private) → `parser/mod.rs` (pub/pub(crate)).

### Constants to move

```rust
pub(crate) const MAX_FUNCTION_CALLS_PER_FILE: usize = 500;
pub(crate) const CALL_SNIPPET_LINES_BEFORE: usize = 2;
pub(crate) const CALL_SNIPPET_LINES_AFTER: usize = 4;
pub(crate) const CALL_SNIPPET_MAX_LINES: usize = 30;
```

### `build_call_snippet` (move verbatim, make `pub`)

```rust
/// Build a context snippet around a call-site.
///
/// Layout: [BEFORE lines] + [full call expression body] + [AFTER lines]
/// Hard cap: CALL_SNIPPET_MAX_LINES total.
pub fn build_call_snippet(source: &str, line: usize, end_line: usize) -> String { ... }
```

### `collect_calls_bfs` (new name for extracted helper)

```rust
/// Walk the entire syntax tree (BFS) collecting function call nodes.
///
/// `call_kind`: tree-sitter node kind to match.
///   - `"call_expression"` for Rust, TypeScript, JavaScript
///   - `"call"` for Python
///
/// `extract_fn`: language-specific closure that extracts a FunctionCall from a
/// matched node. Returns None for nodes that should be skipped (e.g. anonymous
/// closures, tagged templates).
///
/// Deduplicates by callee name (first occurrence wins).
/// Enforces MAX_FUNCTION_CALLS_PER_FILE hard limit.
pub fn collect_calls_bfs<F>(
    root: &tree_sitter::Node,
    source: &str,
    call_kind: &str,
    extract_fn: F,
    out: &mut Vec<FunctionCall>,
)
where
    F: Fn(&tree_sitter::Node, &str) -> Option<FunctionCall>,
{ ... }
```

---

## `rust_parser.rs` Refactor

Remove:
- `collect_call_expressions_recursive` (replaced by `collect_calls_bfs` in mod.rs)
- `build_call_snippet` (moved to mod.rs)
- The four constants (`MAX_FUNCTION_CALLS_PER_FILE`, etc.)

Replace call site in `parse()`:

```rust
// Before:
collect_call_expressions_recursive(&root, source, &mut function_calls);

// After:
super::collect_calls_bfs(&root, source, "call_expression", extract_function_call, &mut function_calls);
```

`extract_function_call` stays private in `rust_parser.rs` (Rust-specific grammar).
References to `build_call_snippet` inside `extract_function_call` become `super::build_call_snippet(...)`.

**Verify:** all existing rust_parser tests pass after refactor — no behavior change.

---

## TypeScript Parser (`typescript_parser.rs`)

### Import addition
```rust
use seshat_core::{
    Export, Function, FunctionCall, Import, Language, LanguageIR, ProjectFile,
    TypeDef, TypeDefKind, TypeScriptIR,
};
```

### In `parse()`
```rust
let mut function_calls: Vec<FunctionCall> = Vec::new();
// ... (after main loop, alongside collect_macro_calls_recursive pattern)
super::collect_calls_bfs(&root, source, "call_expression", extract_ts_js_call, &mut function_calls);
// ... in return value:
language_ir: LanguageIR::TypeScript(TypeScriptIR {
    has_barrel_exports,
    type_only_imports,
    decorators,
    default_export,
    function_calls,
}),
```

### `extract_ts_js_call(node: &Node, source: &str) -> Option<FunctionCall>`

tree-sitter TS/JS grammar: `call_expression { function: <expr>, arguments: <argument_list> }`

Callee extraction by `function` child kind:

| Child kind | Callee extraction | Example |
|---|---|---|
| `"identifier"` | `node_text` directly | `foo()` → `"foo"` |
| `"member_expression"` | `"{object}.{property}"` | `obj.method()` → `"obj.method"` |
| `"optional_chain"` | unwrap to `member_expression` or `identifier` inside | `foo?.()` → `"foo"`, `obj?.method()` → `"obj.method"` |
| `"generic_function"` | get `"function"` child inside | `foo<T>()` → `"foo"` |
| tagged template (no `argument_list` child) | `return None` | `` foo`bar` `` → skip |
| anything else | `return None` | anonymous/complex → skip |

**member_expression extraction:**
```
member_expression { object: <expr>, property: identifier }
→ object_text = first 40 chars of node_text(object)  (avoid huge receiver strings)
→ property_text = node_text(property)
→ callee = "{object_text}.{property_text}"
```

**optional_chain extraction:**
Navigate into the `optional_chain` node to find the innermost `member_expression`
or `identifier`. If neither found, return `None`.

---

## JavaScript Parser (`javascript_parser.rs`)

Identical implementation to TypeScript — same tree-sitter grammar for `call_expression`.

### `require` filter
```rust
fn extract_ts_js_call(node: &Node, source: &str) -> Option<FunctionCall> {
    // ... same extraction logic as TypeScript ...
    if callee == "require" {
        return None;  // already captured in require_calls; skip duplication
    }
    Some(FunctionCall { callee, line, end_line, snippet })
}
```

`require_calls: Vec<String>` continues to be populated as before — no change there.

---

## Python Parser (`python_parser.rs`)

### Import addition
```rust
use seshat_core::{
    Export, Function, FunctionCall, Import, Language, LanguageIR, ProjectFile,
    PythonIR, TypeDef, TypeDefKind,
};
```

### In `parse()`
```rust
let mut function_calls: Vec<FunctionCall> = Vec::new();
// ... (after main loop)
super::collect_calls_bfs(&root, source, "call", extract_python_call, &mut function_calls);
// ... in return value:
language_ir: LanguageIR::Python(PythonIR {
    has_all_export,
    is_init_file,
    type_hints_used,
    decorators: all_decorators,
    function_calls,
}),
```

### `extract_python_call(node: &Node, source: &str) -> Option<FunctionCall>`

tree-sitter Python grammar: `call { function: <expr>, arguments: <argument_list> }`

Note: Python uses field name `"function"` (same as Rust), accessed via
`node.child_by_field_name("function")`.

Callee extraction by `function` child kind:

| Child kind | Callee extraction | Example |
|---|---|---|
| `"identifier"` | `node_text` directly | `foo(x)` → `"foo"` |
| `"attribute"` | `"{value}.{attribute}"` | `obj.method(x)` → `"obj.method"` |
| `"call"` (nested) | full `node_text` of function child, trimmed, max 60 chars | `super().__init__()` → `"super().__init__"` |
| anything else | `return None` | complex expressions → skip |

**attribute extraction:**
```
attribute { value: <expr>, attribute: identifier }
→ value_text = first 40 chars of node_text(value)
→ attr_text = node_text(attribute)
→ callee = "{value_text}.{attr_text}"
```

**Chained calls like `super().__init__()`:**
The outer `call` has:
`function: attribute { value: call { function: "super" }, attribute: "__init__" }`

Handled by the `"attribute"` branch: `value_text = node_text(inner_call)` = `"super()"`,
`attr_text = "__init__"` → callee = `"super().__init__"`.
This is readable and unambiguous for the agent.

---

## Graph Layer: `code_pattern.rs`

### `enrich_with_call_sites` — add three new language branches

Current (Rust only):
```rust
if let LanguageIR::Rust(ref ir) = file.language_ir {
    for fc in &ir.function_calls { ... }
}
```

New (all four languages):
```rust
let calls: &[FunctionCall] = match file.language_ir {
    LanguageIR::Rust(ref ir)       => &ir.function_calls,
    LanguageIR::TypeScript(ref ir) => &ir.function_calls,
    LanguageIR::JavaScript(ref ir) => &ir.function_calls,
    LanguageIR::Python(ref ir)     => &ir.function_calls,
    _                              => &[],
};
for fc in calls {
    if callee_matches_name(&fc.callee, name) {
        total_count += 1;
        if sites.len() < MAX_CALL_SITES_PER_PATTERN {
            sites.push(CallSiteResult { ... });
        }
    }
}
```

`callee_matches_name` already handles exact, `::` qualified, and `.` method forms —
no changes needed there.

---

## Files to Change

| File | Change |
|------|--------|
| `crates/seshat-core/src/ir.rs` | Add `#[serde(default)] function_calls: Vec<FunctionCall>` to TypeScriptIR, JavaScriptIR, PythonIR |
| `crates/seshat-storage/src/ir_serialization.rs` | Bump `IR_SCHEMA_VERSION` v6→v7; update history comment; add `function_calls` to TS/JS/Python roundtrip test fixtures if they exist |
| `crates/seshat-scanner/src/parser/mod.rs` | Add `build_call_snippet` (pub), `collect_calls_bfs` (pub), 4 constants (pub(crate)) |
| `crates/seshat-scanner/src/parser/rust_parser.rs` | Remove local helpers + constants; use `super::collect_calls_bfs` and `super::build_call_snippet` |
| `crates/seshat-scanner/src/parser/typescript_parser.rs` | Add `FunctionCall` import; add `extract_ts_js_call`; call `collect_calls_bfs`; populate `TypeScriptIR.function_calls` |
| `crates/seshat-scanner/src/parser/javascript_parser.rs` | Same as TS + filter `"require"` callee |
| `crates/seshat-scanner/src/parser/python_parser.rs` | Add `FunctionCall` import; add `extract_python_call`; call `collect_calls_bfs`; populate `PythonIR.function_calls` |
| `crates/seshat-graph/src/code_pattern.rs` | Add TS, JS, Python branches in `enrich_with_call_sites` |

**No changes needed:**
- `seshat-mcp` — `PatternResult` serialization unchanged (`call_sites` field already exists)
- `seshat-cli` — no manual construction of TS/JS/Python IR in test fixtures (uses `Default`)

---

## Tests

### `typescript_parser.rs` — 4 new tests

```rust
#[test]
fn extracts_simple_ts_call()
// source: "function main() { foo(1, 2); }"
// assert: ir.function_calls contains entry with callee == "foo", line == 1

#[test]
fn extracts_member_call_ts()
// source: "function main() { obj.method(arg); }"
// assert: ir.function_calls contains entry with callee == "obj.method"

#[test]
fn extracts_optional_chain_call_ts()
// source: "function main() { foo?.(); }"
// assert: ir.function_calls contains entry with callee == "foo"

#[test]
fn deduplicates_ts_calls()
// source: calls foo() three times across the file
// assert: ir.function_calls has exactly 1 entry where callee == "foo"
```

### `javascript_parser.rs` — 4 new tests

```rust
#[test]
fn extracts_simple_js_call()
// source: "function main() { foo(1, 2); }"
// assert: ir.function_calls contains callee "foo"

#[test]
fn extracts_member_call_js()
// source: "function main() { obj.method(arg); }"
// assert: ir.function_calls contains callee "obj.method"

#[test]
fn require_filtered_from_function_calls()
// source: "const fs = require('fs');"
// assert: ir.function_calls does NOT contain any entry with callee == "require"
// assert: ir.require_calls DOES contain "fs"

#[test]
fn deduplicates_js_calls()
// source: calls foo() three times
// assert: exactly 1 entry for "foo" in function_calls
```

### `python_parser.rs` — 4 new tests

```rust
#[test]
fn extracts_simple_python_call()
// source: "def main():\n    foo(1, 2)\n"
// assert: ir.function_calls contains entry with callee == "foo"

#[test]
fn extracts_attribute_call_python()
// source: "def main():\n    obj.method(arg)\n"
// assert: ir.function_calls contains entry with callee == "obj.method"

#[test]
fn extracts_chained_call_python()
// source: "def main():\n    super().__init__()\n"
// assert: ir.function_calls contains entry whose callee contains "__init__"

#[test]
fn deduplicates_python_calls()
// source: "def main():\n    foo()\n    foo()\n    foo()\n"
// assert: ir.function_calls has exactly 1 entry with callee == "foo"
```

### `code_pattern.rs` — 1 new integration test

```rust
#[test]
fn call_sites_populated_from_typescript_ir()
// Setup: insert ProjectFile with Language::TypeScript, TypeScriptIR.function_calls =
//   [FunctionCall { callee: "useEffect", line: 10, end_line: 10,
//                   snippet: "  useEffect(fn, [dep]);" }]
// Also insert that same file as a "function" result (name: "useEffect")
// Query: query_code_pattern(conn, "main", "useEffect")
// Assert: patterns is non-empty
// Assert: patterns[0].call_sites.len() > 0
// Assert: patterns[0].call_site_count > 0
// Assert: patterns[0].call_sites[0].snippet contains "useEffect"
```

### `rust_parser.rs` — verify refactor (no new tests needed)

After moving helpers to `mod.rs`, run the existing 11 call-site tests to confirm
zero behavior change. If any fail — the refactor introduced a regression.

---

## Real-World Verification

After `cargo test --workspace`, run live verification on real repositories.
Save every MCP tool response to log files for review.

### Log file structure

```
/Users/kostik/Projects/seshat/test-logs/
  call-sites-multilang/
    01-seshat-rust-scan_project.json
    02-seshat-rust-run_detection_cycle.json
    03-walt-backend-python-<function>.json
    04-walt-backend-submodule-ts-<function>.json
    05-walt-portal-ts-useQuery.json
    06-walt-portal-ts-useState.json
    07-summary.md
```

Each log file format:

```json
{
  "meta": {
    "repo": "<repo-name>",
    "language": "<Rust|TypeScript|JavaScript|Python>",
    "query": "<function-name>",
    "binary_version": "seshat 0.1.0 (<commit>)",
    "timestamp": "2026-..."
  },
  "response": { /* full MCP JSON response */ }
}
```

### Repository 1: seshat (`/Users/kostik/Projects/seshat`) — Rust

```bash
~/Projects/seshat/target/debug/seshat scan ~/Projects/seshat --quiet
```

MCP `query_code_pattern` queries:
- `"scan_project"` — expect 5+ call_sites, multiline snippet for `scan_project_with_progress`
- `"run_detection_cycle"` — expect call_sites from `warm_tier.rs` and `scan.rs`
- `"deserialize_ir"` — expect call_sites from storage layer

What to verify:
- `call_site_count > 0`
- Snippets are multi-line (2 context lines before + call body + 4 after)
- Multiline calls: at least one result has `end_line > line`
- No duplicate callee names within a single file

### Repository 2: walt-chat-backend (`/Users/kostik/Projects/Walt/walt-chat-backend`) — Python + TS/JS submodule

```bash
~/Projects/seshat/target/debug/seshat scan ~/Projects/Walt/walt-chat-backend --quiet
```

**Python queries** — discover real function names after scan, then query 2-3:
- Try framework patterns: `"get"`, `"post"`, `"create"`, `"update"`, `"delete"`
- Try application-specific names visible in scan report

What to verify (Python):
- `call_sites` come from `.py` files
- Attribute calls (`service.method(...)`) → callee is `"service.method"`
- `call_site_count` is non-zero and reasonable (not 500)
- Snippets contain real Python code with original indentation

**TypeScript/JS submodule queries** — after submodule scans:
- Try: `"fetch"`, `"axios"`, or real component/hook names from submodule
- Verify `require` does NOT appear as a call_site callee in JS files

What to verify (TS/JS):
- call_sites from `.ts`/`.tsx`/`.js` files
- Optional chain calls captured
- `require` absent from `function_calls`

### Repository 3: walt-portal (`/Users/kostik/Projects/Walt/walt-portal`) — TypeScript/JavaScript

```bash
~/Projects/seshat/target/debug/seshat scan ~/Projects/Walt/walt-portal --quiet
```

MCP queries — discover real function names after scan, then query 2-3:
- Try React patterns: `"useState"`, `"useEffect"`, `"useQuery"`
- Try API client functions visible in scan report
- Try a real component name from the codebase

What to verify:
- TypeScript member calls (`api.get(...)`) → callee `"api.get"`
- Generic calls (`useQuery<User>(...)`) → callee `"useQuery"` (generics stripped)
- Tagged template literals → NOT in call_sites
- `call_sites[*].file` are real `.ts`/`.tsx` paths

### `summary.md` — final report table

| Repo | Language | Query | call_site_count | snippet_ok | multiline_ok | issues |
|------|----------|-------|-----------------|------------|--------------|--------|
| seshat | Rust | scan_project | ? | ? | ? | |
| seshat | Rust | run_detection_cycle | ? | ? | ? | |
| walt-backend | Python | TBD | ? | ? | ? | |
| walt-backend | TS(submodule) | TBD | ? | ? | ? | |
| walt-portal | TypeScript | TBD | ? | ? | ? | |
| walt-portal | TypeScript | TBD | ? | ? | ? | |

`snippet_ok` = snippet is non-empty and contains real source code
`multiline_ok` = at least one call_site has `end_line > line`

### Live verification pass/fail criteria

- [ ] All three repos scan successfully (exit code 0, no parse errors)
- [ ] Python: at least one query returns `call_site_count > 0`
- [ ] TypeScript: at least one query returns `call_site_count > 0`
- [ ] JavaScript: at least one query returns `call_site_count > 0` (if JS files present)
- [ ] No response contains `"callee": "require"` in any JS file's call_sites
- [ ] All snippets are multi-line (not just the single call line)
- [ ] `summary.md` has no unresolved red flags
- [ ] All log files saved under `test-logs/call-sites-multilang/`

---

## Implementation Order

1. `parser/mod.rs` — move shared helpers: `build_call_snippet`, `collect_calls_bfs`, 4 constants
2. `rust_parser.rs` — refactor to use `super::` helpers; run existing 11 call-site tests to verify zero regression
3. `ir.rs` — add `function_calls` field to TypeScriptIR, JavaScriptIR, PythonIR
4. `ir_serialization.rs` — bump v6→v7; update version history comment; fix any test fixtures
5. `typescript_parser.rs` — `extract_ts_js_call` + `collect_calls_bfs` call + 4 tests
6. `javascript_parser.rs` — same as TS + `require` filter + 4 tests
7. `python_parser.rs` — `extract_python_call` + `collect_calls_bfs` call + 4 tests
8. `code_pattern.rs` — 3 new branches in `enrich_with_call_sites` + 1 integration test
9. `cargo fmt --all` + `cargo clippy --workspace -- -D warnings`
10. `cargo test --workspace` — all existing tests pass + 13 new tests pass
11. `cargo build` (debug binary for scanning)
12. Scan all three real repos + MCP live queries + save logs to `test-logs/call-sites-multilang/`
13. Fill in `summary.md`; verify all pass/fail criteria met

---

## Acceptance Criteria

1. `cargo build --workspace` — no errors, no warnings
2. `cargo clippy --workspace -- -D warnings` — clean
3. `cargo fmt --all` — no diff
4. `cargo test --workspace` — all pre-existing tests pass + 13 new tests pass
5. After scanning a TypeScript/JS/Python project, `query_code_pattern` returns `call_sites` from those language files
6. `call_site_count > 0` for real functions with usages in all three languages
7. Snippets are multi-line: 2 lines context before + full call expression body + 4 lines after, capped at 30
8. `"require"` does NOT appear as callee in any JS file's `function_calls`
9. No duplicate callee names within a single file's `function_calls` for any language
10. IR size increase from v6→v7 baseline < 30% for a typical TS-heavy project
11. All log files present in `test-logs/call-sites-multilang/` with non-empty call_sites
12. `summary.md` complete with no unresolved issues

---

## Known Limitations (documented, not fixed in this story)

- **Cross-language call-sites** (TS function called from JS file) — deferred; requires import graph resolution
- **Bare `require` aliases** (`const load = require; load('fs')`) — not filtered, low priority
- **Optional chaining on member expressions** (`obj?.method()`) — captured as `"obj.method"` (best effort)
- **Deeply chained calls** (`a.b().c()`) — captured via `node_text` of function child, may be verbose
- **Python type annotations with calls** (`x: List[int] = foo()`) — `foo` captured correctly via BFS

---

## Session Notes

- **Party mode discussion:** 2026-04-14, agents Winston (Architect), Amelia (Dev), Murat (QA), John (PM)
- **Rust phase commit:** `84ff359` on `feat/call-sites`
- **Branch to continue:** `feat/call-sites`
- **MCP live test confirmed working for Rust:** `scan_project` → 5 call-sites; `run_detection_cycle` multiline call captured correctly across 7 lines with 4-line post-call context
- **Real-world repos for verification:**
  - `/Users/kostik/Projects/seshat` (Rust)
  - `/Users/kostik/Projects/Walt/walt-chat-backend` (Python + TS/JS submodule)
  - `/Users/kostik/Projects/Walt/walt-portal` (TypeScript/JavaScript)
