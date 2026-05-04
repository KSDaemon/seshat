---
date: 2026-05-04
scope: JS/TS workspace package detection from package.json
languages: JavaScript, TypeScript
approach: Scan-time workspace parsing → repo_metadata → graph reads from DB
merge_with: local_packages (union: auto-detected + manual override)
priority: Enhancement (not blocking — relative imports already work)
---

# PRD: JS/TS Workspace Package Detection — Internal Dependency Resolution

**Author:** Kostik
**Date:** 2026-05-04

---

## Part I: Problem Statement

### What Already Works

JS/TS relative imports are handled correctly:

| Import | Resolution | Status |
|--------|-----------|--------|
| `import { foo } from "./utils"` | Filesystem-relative (current `resolve_relative_import`) | ✅ Works |
| `import { foo } from "../shared"` | Filesystem-relative with parent dir | ✅ Works |
| `import { Button } from "react"` | External (treated as `None`) | ✅ Correct |
| `import { api } from "my-app/shared"` | External (`None`) | ❌ Wrong for monorepo |

All non-relative imports (no `./` or `../` prefix) are classified as external
and excluded from the dependency graph. This is correct for npm dependencies
(`react`, `lodash`) but **wrong for workspace packages in monorepos**.

### The Gap: NPM/Yarn/pnpm Workspaces

In monorepo projects, `package.json` defines workspace packages:

```json
{
  "name": "my-monorepo",
  "workspaces": ["packages/*", "apps/*"]
}
```

Each workspace package has its own `package.json` with a `"name"`:

```json
// packages/shared/package.json
{ "name": "@myorg/shared" }

// apps/web/package.json
{ "name": "my-web-app" }
```

When `apps/web/src/App.ts` does `import { Button } from "@myorg/shared"`, this
should resolve to `packages/shared/src/index.ts` — it's an **internal dependency**,
not an npm package.

Currently, Seshat treats it as external → `None` → silently drops it from the
dependency graph.

### Scope

This PRD covers:
- **NPM/ Yarn / pnpm workspace monorepos**
- **Root `package.json`** with `"workspaces"` field
- **Both scoped (`@org/pkg`) and unscoped (`my-pkg`)** workspace package names
- **Single-package JS/TS projects** where `"name"` in root `package.json` could
  be used as the internal package identifier (though this is rare — single-package
  projects mostly use relative imports)

**Not in scope:** `node_modules` traversal, `tsconfig` path aliases, monorepo
tools (Turborepo, Nx, Lerna) special handling. Those are future enhancements.

---

## Part II: Design

### Architecture: Scan-Time Only

Same pattern as Rust + Python:

| Phase | What Happens |
|-------|-------------|
| **Scan** | Discover `package.json` files via `"workspaces"` field (if present), read each workspace package's `"name"`, normalize, store as JSON array in `repo_metadata` under key `workspace_crates` (shared key — unified storage for all internal names) |
| **Re-scan** | If any `package.json` changed → re-parse → update `workspace_crates` |
| **Query time** | `query_dependencies` loads `workspace_crates` from DB, passes through resolution chain |

### Discovery Algorithm

```
1. Read root/package.json
2. If "workspaces" field exists:
   a. Parse glob patterns (e.g. ["packages/*", "apps/admin"])
   b. Walk matching directories
   c. For each directory containing package.json:
      - Read "name" field
      - Store (normalized: no action needed, but strip @scope/ for lookup? No — keep full name)
   d. If root package.json also has "name", include it
3. If no "workspaces" field, only use root "name" (if single-package use case)
4. Merge with local_packages config
5. Write to repo_metadata as JSON: ["@myorg/shared", "my-web-app", "my-lib"]
```

### How Import Matching Works

For JS/TS imports, the resolver already has two paths in `resolve_import()`:

1. **`./` or `../` prefix** → `resolve_relative_import()` — ✅ Works, unchanged
2. **Everything else** → with the fix, check against internal names list

The matching logic is different from Rust because JS import paths use `/` not `::`:

| Import | Internal name | How to match |
|--------|--------------|-------------|
| `import { x } from "@myorg/shared"` | `"@myorg/shared"` | Exact match on internal name → resolve to package root |
| `import { x } from "@myorg/shared/utils"` | `"@myorg/shared"` | Prefix match → strip `@myorg/shared/`, resolve `utils` via suffix index |
| `import { x } from "my-app/shared"` | `"my-app"` | Prefix match → strip `my-app/`, resolve `shared` via suffix index |
| `import { x } from "react"` | not in list | External → `None` ✅ |

### Changes to `dependencies.rs`

Building on the Rust+Python PRD (which already adds `internal_names: &[String]`
parameter to the chain), for JS/TS we need:

1. **`resolve_import()`**: after the `starts_with('.')` check (relative imports),
   add a branch: check if the module starts with any internal name followed by `/`
   or matches an internal name exactly.

2. **`resolve_js_internal_import(module, internal_names, suffix_index)`**:
   - For each internal name `n`:
     - If `module == n` → resolve `"index"` or package entry point via suffix index
     - If `module.starts_with(&format!("{n}/"))` → strip prefix, resolve rest via suffix index
   - Order matters: check longest names first (e.g. `@myorg/shared` before `@myorg`)

3. **`module_to_path_suffix()`**: No changes needed — JS imports that reach
   suffix resolution already use `/` separators which the function handles.

### Edge Cases (from Quinn)

| Condition | Behavior |
|-----------|----------|
| No `"workspaces"` field | Only use root `"name"` if present, otherwise empty → all imports external (correct) |
| `"workspaces"` with glob patterns | Expand globs using existing `ignore` crate (WalkBuilder), same as file discovery |
| Workspace directory without `package.json` | Skip silently — not a package |
| `package.json` without `"name"` | Skip — can't identify as internal |
| Scoped package `@myorg/shared` imported as `@myorg/shared/subpath` | Prefix match on `@myorg/shared/`, resolve `subpath` |
| Root package imports itself (circular-ish) | Valid — self-referencing import via package name |
| `local_packages` contains names not in auto-detected | Union — both sources contribute |

### JS/TS-Specific Considerations

**Unlike Rust, JS/TS projects rarely have absolute internal imports unless they
use workspaces.** Most internal imports are relative (`./utils`, `../shared`).
The workspace case is the primary beneficiary.

**TypeScript path aliases** (`tsconfig.json` `"paths"`): Not in this PRD.
Aliases like `@app/*` → `src/*` could be parsed at scan time and stored as
internal prefixes. This is a separate enhancement.

**Monorepo tools** (Turborepo, Nx, Lerna): These build on `package.json`
`"workspaces"` — our approach covers them automatically.

---

## Part III: Implementation Plan

### Files to Change

| File | Change |
|------|--------|
| `crates/seshat-scanner/src/manifest.rs` | `parse_package_json()`: extract `"workspaces"` field (array of glob patterns) and root `"name"`. Store in `ManifestAnalysis`. |
| `crates/seshat-scanner/src/orchestrator.rs` | Step 8: expand `"workspaces"` globs, discover workspace `package.json` files, read each `"name"`, merge with root `"name"` and `local_packages`, write to `repo_metadata`. |
| `crates/seshat-graph/src/dependencies.rs` | Add `resolve_js_internal_import()` function. Extend `resolve_import()` to handle JS/TS internal package imports. No new constants — reuses `internal_names` from Rust+Python PRD. |
| `crates/seshat-core/src/config.rs` | No changes. |

### PR Structure

1. **manifest.rs** — extract `"workspaces"` and `"name"` from `package.json`
2. **orchestrator.rs** — discover workspace packages, collect names, write to `repo_metadata`
3. **dependencies.rs** — add JS/TS internal import resolution
4. **tests** — workspace package tests
5. **verification** — cargo test, clippy, fmt

### Tests

| # | Input | Expected |
|---|-------|----------|
| 1 | `package.json` with `"workspaces": ["packages/*"]`, packages `@myorg/shared`, `my-web` | `["@myorg/shared", "my-web"]` |
| 2 | No `"workspaces"`, only `"name": "my-app"` | `["my-app"]` |
| 3 | No `"workspaces"` and no `"name"` | `[]` |
| 4 | Invalid JSON in `package.json` | `[]` + tracing::warn |
| 5 | Empty `"workspaces"` array | `[]` |
| 6 | `import "@myorg/shared"` with `["@myorg/shared"]` in internal names | resolved → package root |
| 7 | `import "@myorg/shared/utils"` with `["@myorg/shared"]` | resolved → `packages/shared/src/utils.ts` (via suffix) |
| 8 | `import "my-web/components"` with `["my-web"]` | resolved → internal dep |
| 9 | `import "react"` with `["@myorg/shared"]` | external (not in list) |
| 10 | Scoped package `@myorg/shared` — longest prefix match wins over `@myorg` | correct resolution |
| 11 | `query_dependencies` end-to-end — monorepo package imports its sibling | DependencyEntry.resolved = true, correct file_path |

---

## Part IV: Acceptance Criteria

- [ ] `cargo test --all-targets` — existing JS/TS tests + new workspace tests pass
- [ ] `cargo clippy --all-targets -- -D warnings` — no warnings
- [ ] `cargo fmt --check` — consistent formatting
- [ ] NPM workspace package names auto-detected from `package.json`
- [ ] Workspace imports resolve to correct internal file paths
- [ ] Non-workspace `import` statements remain as external (no regression)
- [ ] Single-package JS/TS projects: `"name"` used as internal (rare usage, no harm)
- [ ] `local_packages` union with auto-detected
- [ ] No changes to Rust or Python resolution introduced
- [ ] Incremental rescan updates workspace list on `package.json` change

---

## Part V: Priority & Dependencies

**Priority:** Enhancement (not blocking). Relative imports already cover the
majority of JS/TS internal dependency edges. Workspace imports are the remaining gap.

**Depends on:** PRD #1 (Rust + Python manifest parsing). This PRD should be
implemented second, since it builds on the same `internal_names` parameter
chain introduced in PRD #1.

**Can be parallelized:** The Rust+Python PRD #1 is independent. This PRD
can start after PRD #1's function signature changes are merged.
