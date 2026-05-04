---
date: 2026-05-04
scope: Replace hardcoded WORKSPACE_CRATES with runtime manifest parsing for Rust + Python
languages: Rust, Python
approach: Scan-time manifest parsing → repo_metadata → graph reads from DB
merge_with: local_packages (union: auto-detected + manual override for edge cases)
---

# PRD: Runtime Manifest Parsing — Internal Crate/Package Detection (Rust + Python)

**Author:** Kostik
**Date:** 2026-05-04

---

## Part I: Problem Statement

### Current State

`crates/seshat-graph/src/dependencies.rs` contains a hardcoded constant:

```rust
const WORKSPACE_CRATES: &[&str] = &[
    "seshat_core", "seshat_scanner", ...
    "seshat_bin",
];
```

This constant drives five functions (`is_workspace_crate`, `is_likely_internal`,
`resolve_workspace_crate_import`, `module_to_path_suffix`, `first_module_segment`)
that determine whether a Rust import like `use seshat_graph::validate_approach` is
an internal dependency or an external one.

### What's Broken

For **any Rust project other than Seshat itself**, all `use my_crate::module` imports
are classified as external and excluded from the dependency graph. The dependency
graph is silently incomplete.

For **Python projects**, internal package imports (`from my_package.utils import foo`)
are similarly invisible — `is_likely_internal` has no awareness of project-local
package names. (Python relative imports `.models`, `..utils` accidentally work
because they pass through filesystem-relative resolution, but absolute internal
imports fail.)

### Root Cause

The import parser works with source text. `my_crate::module` is syntactically
indistinguishable from `serde::Serialize` — both are `<Name>::<rest>`. Without
knowing which names belong to the project itself, the parser cannot distinguish
internal from external.

---

## Part II: Design

### Architecture: Scan-Time Only

| Phase | What Happens |
|-------|-------------|
| **Scan** | Orchestrator parses `Cargo.toml` / `pyproject.toml`, extracts crate/package names, normalizes `-` → `_`, stores as JSON array in `repo_metadata` under key `workspace_crates` |
| **Re-scan (incremental/full)** | If manifest changed → re-parse → overwrite `workspace_crates` in `repo_metadata` |
| **Query time** | `query_dependencies` reads `workspace_crates` from DB, passes list through the resolution chain |
| **No manifest / non-Rust/Python project** | `workspace_crates` not written → empty list → all `::` imports = external (correct fallback) |

### Interaction with `local_packages` Config

`ScanConfig::local_packages` is a user-specified list of local package names.
It was originally a workaround for this exact problem. Now we **merge**:

- **Auto-detected** names come from manifest parsing (primary source)
- **Manual** names from `local_packages` are added as a union (edge case override)
- Result: final list = `auto ∪ manual`

Existing code in `orchestrator.rs:306-309` that filters `dependencies_used`
by `local_packages` will also use the merged list instead.

### What Gets Parsed From Manifests

| Manifest | Source Field | Example | Normalized |
|----------|-------------|---------|------------|
| `Cargo.toml` | `[package] name` | `"seshat-core"` | `"seshat_core"` |
| `Cargo.toml` | `[workspace.members]` paths | `"crates/seshat-graph"` | `"seshat_graph"` |
| `pyproject.toml` | `[project] name` (PEP 621) | `"my-package"` | `"my_package"` |
| `pyproject.toml` | `[tool.poetry] name` (fallback) | `"my-package"` | `"my_package"` |

For workspace members, the last path component is taken as the crate name
(e.g. `crates/seshat-graph` → `seshat_graph`).

### Changes to `dependencies.rs`

1. **Remove `const WORKSPACE_CRATES`** — dead code
2. **Remove all functions that only served the hardcoded list:**
   - `first_module_segment()` — keep (needed for general module parsing)
   - `is_workspace_crate()` — **delete**, replace with `is_known_internal(name, &[String])`
   - `resolve_workspace_crate_import()` — **rename** to `resolve_internal_crate_import(module, internal_names, suffix_index)`, takes dynamic list
3. **Signature changes:**
   - `is_likely_internal(module, internal_names: &[String])`
   - `resolve_import(module, importing_dir, known_paths, suffix_index, internal_names: &[String])`
   - `build_dependencies(..., internal_names: &[String])`
   - `module_to_path_suffix(module)` — unchanged signature; internal prefix stripping moved to callers via the shared `strip_first_segment()` helper
4. **New function:** `load_internal_names(conn, branch_id) -> Vec<String>` — reads `repo_metadata.workspace_crates`, deserializes JSON array
5. **`query_dependencies()`** calls `load_internal_names()` once, passes result down the chain
6. **Python import handling:** `is_likely_internal` already captures `.`-prefixed imports. The key addition: if the first segment of a non-`.` import matches a known internal name (`my_package`), treat as internal and resolve via suffix index after stripping the prefix.

### Data Flow (Updated)

```
query_dependencies(conn, branch_id, target_path)
  │
  ├─ load_internal_names(conn, branch_id) → Vec<String>
  │
  ├─ load_branch_ir(conn, branch_id) → LoadedIR
  │
  ├─ SuffixIndex::build(&known_paths)
  │
  └─ build_dependencies(target_file, known_paths, suffix_index, &internal_names)
      └─ resolve_import(module, ..., &internal_names)
          ├─ starts_with('.') → resolve_relative_import()  [Python/JS]
          ├─ is_in_internal_names(first_seg) → strip prefix, resolve_by_suffix()
          ├─ crate/super/self → resolve_by_suffix()        [Rust special]
          ├─ src/ or src. → resolve_by_suffix()            [Python flat src]
          └─ else → None (external)
```

### Error Handling

| Condition | Behavior |
|-----------|----------|
| `Cargo.toml` parse error (invalid TOML) | `tracing::warn!`, empty list, no crash |
| `pyproject.toml` parse error | `tracing::warn!`, empty list, no crash |
| Manifest file missing (not a Rust/Python project) | `workspace_crates` not written, `load_internal_names` returns `Vec::new()` |
| `workspace_crates` key missing in `repo_metadata` (old DB from before this change) | `load_internal_names` returns `Vec::new()`. The union with `local_packages` happens at scan time in the orchestrator; if no scan has run yet, only relative/dot imports resolve as internal until the first scan completes |
| `[package] name` absent but `[workspace.members]` present | Only workspace members used |
| Both absent, `local_packages` configured | Only `local_packages` names used |

---

## Part III: Implementation Plan

### Files to Change

| File | Change |
|------|--------|
| `crates/seshat-scanner/src/manifest.rs` | `parse_manifest()`: extract `[package] name` from Cargo.toml, `[project] name` / `[tool.poetry] name` from pyproject.toml. Return in `ManifestAnalysis`. |
| `crates/seshat-scanner/src/orchestrator.rs` | Step 8: after manifest analysis, compute `internal_names` = auto-detected ∪ `config.local_packages`. Write to `repo_metadata` as JSON array. |
| `crates/seshat-graph/src/dependencies.rs` | Remove `WORKSPACE_CRATES` constant. Delete `is_workspace_crate()`. Add `load_internal_names()`. Update all function signatures. |
| `crates/seshat-graph/src/validate_approach.rs` | `enrich_used_by()` calls `query_dependencies` — no signature change needed (internal names loaded inside `query_dependencies`). |
| `crates/seshat-mcp/src/tools/query_dependencies.rs` | No changes — calls `seshat_graph::query_dependencies` unchanged. |
| `crates/seshat-core/src/config.rs` | No changes. `local_packages` field remains. |

### PR Structure

Single PR with atomic commits:

1. **manifest.rs** — extract package names from Cargo.toml and pyproject.toml
2. **orchestrator.rs** — persist internal names to `repo_metadata`
3. **dependencies.rs** — replace constant with DB-driven list, update chain
4. **tests** — Rust workspace tests, Python package tests, fallback tests
5. **verification** — cargo test, clippy, fmt

### Tests (from Quinn's list, extended for Python)

**Manifest parsing tests:**
| # | Input | Expected `internal_names` |
|---|-------|--------------------------|
| 1 | Single crate: `[package] name = "my-app"` | `["my_app"]` |
| 2 | Workspace: `members = ["crates/core", "crates/api"]` | `["core", "api"]` |
| 3 | Workspace + root package | `["my_app", "core", "api"]` |
| 4 | Hyphens: `"my-crate"` → `"my_crate"` | `["my_crate"]` |
| 5 | Empty members: `members = []` | `[]` (or root package if present) |
| 6 | Invalid TOML syntax | `[]` + warn log |
| 7 | PEP 621: `[project] name = "my-package"` | `["my_package"]` |
| 8 | Poetry fallback: `[tool.poetry] name = "my-package"` | `["my_package"]` |
| 9 | Both PEP 621 and Poetry (PEP 621 wins) | `["my_package"]` |
| 10 | `local_packages` union with auto-detected | `auto ∪ manual` |

**Graph resolution tests:**
| # | Input | Expected |
|---|-------|----------|
| 11 | `use seshat_graph::foo` with `["seshat_graph"]` in DB | resolved → `crates/seshat-graph/src/foo.rs` |
| 12 | `use serde::Serialize` with any internal names | unresolved (external) |
| 13 | Internal names empty → all `::` imports = external | no resolved deps |
| 14 | `from my_package.utils import foo` with `["my_package"]` | resolved → `my_package/utils.py` |
| 15 | `from django.db import models` with `["my_package"]` | external |
| 16 | `query_dependencies` end-to-end with workspace crate | DependencyEntry.resolved = true |

---

## Part IV: Acceptance Criteria

- [ ] `cargo test --all-targets` — all tests pass (existing + new)
- [ ] `cargo clippy --all-targets -- -D warnings` — no warnings
- [ ] `cargo fmt --check` — formatting consistent
- [ ] `const WORKSPACE_CRATES` полностью удалена из `dependencies.rs`
- [ ] Rust-проекты с workspace и single-crate резолвят внутренние импорты корректно
- [ ] Python-проекты с `pyproject.toml` резолвят внутренние пакетные импорты
- [ ] `local_packages` из конфига объединяется (union) с авто-определёнными именами
- [ ] Не-Rust/не-Python проекты не ломаются (пустой список → все внешние)
- [ ] Инкрементальный рескан обновляет список при изменении манифеста

---

## Part V: Future Work (Deferred)

### Workspace Member Name Resolution

For `[workspace.members]` entries, the current scanner infers crate names
from the **last path component** of literal paths (e.g. `"crates/my-crate"` →
`"my_crate"`), then reads the inner `Cargo.toml`'s `[package].name` as the
authoritative name.  Glob patterns (`"crates/*"`) are skipped at parse time;
the scan orchestrator handles glob expansion separately.

**Remaining gap:** Workspace members with glob patterns are not resolved to
crate names during the scan itself.  A future enhancement could expand globs
via the existing `ignore`/WalkBuilder infrastructure at scan time and inject
the discovered crate names.

### Legacy Python Manifests (`setup.cfg`, `setup.py`)

Only `pyproject.toml` is parsed for Python package names.  Projects using
`setup.cfg` or `setup.py` (still widely used) get no `internal_names`.
These files are already known to the scanner (`file_structure.rs` lists
`setup.cfg`) but are never parsed for name extraction.

### Nested Manifest Discovery

`discover_manifests()` only looks in the project root directory.  For
monorepos with manifests in subdirectories (e.g. `crates/seshat-core/Cargo.toml`),
those inner manifests are never discovered for name extraction.  Only the
top-level manifest contributes workspace member names.

### Non-Poetry Build Backends (PDM, Hatch, Flit, Maturin)

Only PEP 621 and Poetry are handled for Python package name extraction.
Modern tools like PDM (`[tool.pdm]`), Hatchling, and Flit are not yet supported.

### Per-Branch `workspace_crates` Scoping

`load_internal_names()` accepts `branch_id` but ignores it — metadata is
stored globally, not per-branch.  If two branches have different workspace
structures, they share the same `workspace_crates` list.
