# Story: query_code_pattern — Real Call-Site Evidence

**Status:** Ready for implementation  
**Priority:** High  
**Branch:** implement from `fix/convention-snippets-regression` or new feature branch  

---

## Problem Statement

`query_code_pattern` currently returns only symbol *definitions* — where a function/type is declared. For an AI agent this is marginally useful: it knows the function exists and its signature. What's missing is *where and how it's called* — the actual usage patterns.

Import-based "usages" (showing `use seshat_scanner::scan_project;`) are **not useful** — every language has a standard import syntax that tells the agent nothing about how the symbol is actually invoked in practice.

The agent needs to see:
```
scan_project is called here:
  scan.rs:387  →  let scan_result = scan_project(root, &config, &db)?;
  integration_test.rs:45  →  scan_project(&temp_dir, &ScanConfig::default(), &db)
```

---

## Architecture Decision (from party mode discussion, 2026-04-13)

### What to store in IR

New struct in `RustIR` (and eventually TS/JS/Python):

```rust
pub struct FunctionCall {
    pub callee: String,   // full name as written: "scan_project", "db.execute", "Arc::new"
    pub line: usize,      // 1-indexed call-site line
    pub snippet: String,  // the single line of source at call site (trimmed)
}
```

Key design decision: **store snippet in IR at scan time** (not read from disk at query time).  
Rationale: MCP handler has no access to source_map — only IR from DB. Reading files at query time adds disk I/O per MCP call, which is unacceptable.

### Deduplication rule (critical)

**Store unique callee names only — one example per unique callee per file.**

Rationale: A file may call `unwrap()` 200 times. Storing 200 entries wastes IR space and pollutes call-site results. An agent needs one example to understand the pattern.

Implementation: Before inserting into `function_calls`, check if a `FunctionCall` with the same `callee` already exists for this file. If yes, skip. This gives at most one example per unique function name.

Hard limit: **500 unique callees per file** (safeguard for very large files).

### Parser implementation (Rust)

New function `collect_call_expressions_recursive` in `rust_parser.rs`, modelled on the existing `collect_macro_calls_recursive` (lines 612–644):

```rust
fn collect_call_expressions_recursive(root: &Node, source: &str, out: &mut Vec<FunctionCall>) {
    // Iterative BFS, skip token_tree bodies (same as macro walker)
    let mut stack: Vec<(Node, usize)> = ...;
    const MAX_DEPTH: usize = 60;

    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_DEPTH { continue; }

        if node.kind() == "call_expression" {
            // Extract callee: function child (identifier or scoped_identifier or field_expression)
            if let Some(call) = extract_function_call(&node, source.as_bytes()) {
                // Dedup: only insert if callee not yet seen
                if !out.iter().any(|c| c.callee == call.callee) && out.len() < 500 {
                    out.push(call);
                }
            }
            // Still recurse into call_expression children (nested calls)
        }
        for child in node.children(...) {
            stack.push((child, depth + 1));
        }
    }
}
```

tree-sitter Rust grammar: `call_expression { function: <identifier|scoped_identifier|field_expression>, arguments: <arguments> }`

Snippet extraction: `source.lines().nth(line - 1).unwrap_or("").trim().to_string()` — one trimmed line.

### IR schema

`IR_SCHEMA_VERSION: v5 → v6`

```rust
// seshat-core/src/ir.rs
pub struct RustIR {
    pub mod_declarations: Vec<ModDeclaration>,  // already exists
    pub derive_macros: Vec<DeriveUsage>,        // already exists
    pub trait_implementations: Vec<TraitImpl>,  // already exists
    pub error_types: Vec<String>,               // already exists
    pub macro_calls: Vec<MacroCall>,            // added in v5
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,      // NEW in v6
}
```

`#[serde(default)]` ensures existing v5 blobs deserialize without error (empty vec).

---

## query_code_pattern Response Changes

### Current response (PatternResult)

```json
{
  "name": "scan_project",
  "kind": "function",
  "file": "crates/seshat-scanner/src/orchestrator.rs",
  "line": 151,
  "end_line": 157,
  "signature": "pub fn scan_project(root, config, db)",
  "score": 1.0
}
```

### New response

```json
{
  "name": "scan_project",
  "kind": "function",
  "file": "crates/seshat-scanner/src/orchestrator.rs",
  "line": 151,
  "end_line": 157,
  "signature": "pub fn scan_project(root, config, db)",
  "score": 1.0,
  "call_sites": [
    {
      "file": "crates/seshat-cli/src/scan.rs",
      "line": 387,
      "snippet": "let scan_result = scan_project(root, &config, &db)?;"
    },
    {
      "file": "crates/seshat-bin/tests/scan_integration.rs",
      "line": 45,
      "snippet": "scan_project(&temp_dir.path(), &ScanConfig::default(), &db)"
    }
  ],
  "call_site_count": 2
}
```

### Matching logic for call-sites

When building call_sites for a pattern result named `"scan_project"`:
1. Iterate all files in IR
2. For each file, check `language_ir.Rust.function_calls`
3. Match if: `callee == name` OR `callee.ends_with("::<name>")` OR `callee.ends_with("::<name>")` — boundary-aware suffix match
4. Collect up to 5 call-site results, ordered by file path (deterministic)
5. `call_site_count` = total matched across all files (not capped at 5)

---

## Multi-language Plan

**Phase 1 (this story):** Rust only.

**Phase 2 (follow-up stories, one per language):**

| Language | tree-sitter node | Notes |
|---|---|---|
| TypeScript | `call_expression` | Same structure as Rust |
| JavaScript | `call_expression` | Same |
| Python | `call` | `func` child instead of `function` |

Each language gets its own IR field:
- `TypeScriptIR.function_calls: Vec<FunctionCall>`
- `JavaScriptIR.function_calls: Vec<FunctionCall>`  
- `PythonIR.function_calls: Vec<FunctionCall>`

Dedup rule and 500-limit apply to all languages identically.

---

## Files to Change

| File | Change |
|---|---|
| `crates/seshat-core/src/ir.rs` | Add `FunctionCall` struct, add `function_calls` to `RustIR` |
| `crates/seshat-storage/src/ir_serialization.rs` | Bump `IR_SCHEMA_VERSION` v5→v6, update roundtrip test |
| `crates/seshat-scanner/src/parser/rust_parser.rs` | Add `collect_call_expressions_recursive`, `extract_function_call`, call from main parse loop |
| `crates/seshat-graph/src/code_pattern.rs` | Add `call_sites: Vec<CallSiteResult>` and `call_site_count` to `PatternResult`, populate from IR |
| `crates/seshat-mcp/src/` | Update JSON serialization of `PatternResult` |

---

## Acceptance Criteria

1. `cargo run -- scan .` on seshat repo produces `function_calls` in IR for Rust files
2. `query_code_pattern("scan_project")` returns `call_sites` with at least 2 entries (scan.rs + integration test)
3. Each call_site has non-empty `snippet` containing the actual call expression
4. No duplicate callee names within a single file's `function_calls`
5. IR size increase < 20% vs v5 baseline (measure with `stat` on seshat.db)
6. All existing tests pass
7. New tests:
   - Parser: `extract_call_expressions_deduplicates_same_callee` — same callee called 5 times → 1 entry in IR
   - Parser: `extract_call_expressions_captures_scoped_calls` — `Arc::new(...)` → callee = "Arc::new"
   - Parser: `extract_call_expressions_respects_500_limit`
   - Integration: `query_code_pattern_returns_call_sites_for_known_function`
   - Integration: `call_site_snippet_is_nonempty_and_contains_callee_name`

---

## Out of Scope

- Method calls on `self` (e.g. `self.run()`) — captured but no special handling
- Closure calls — captured as anonymous, may have noisy callee names
- Macro call-sites — already handled by `macro_calls` field, not duplicated here
- Vector/semantic search integration for call-sites — separate story
- `related_conventions` populated from call-sites — separate story
