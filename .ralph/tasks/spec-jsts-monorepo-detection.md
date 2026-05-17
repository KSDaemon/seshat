---
date: 2026-05-17
type: implementation-spec
size: medium (~2-3h)
scope: NPM/Yarn/pnpm workspace package detection for internal-import resolution
parent_prd: prd-js-ts-workspace-detection-2026-05-04.md
roadmap_tag: "#jsts-monorepo"
languages: JavaScript, TypeScript
---

# Spec: JS/TS Monorepo Detection

**Author:** Kostik (drafted by Claude)
**Date:** 2026-05-17
**Status:** Ready for implementation
**Source PRD:** `prd-js-ts-workspace-detection-2026-05-04.md` (read first — this
spec is the implementation-ready distillation, not a replacement)

---

## Problem (one paragraph)

Today `crates/seshat-scanner/src/manifest.rs:343` `parse_package_json()` extracts
DECLARED dependencies but does **not** extract internal workspace package names.
There is no JS/TS analogue of the Rust `extract_crate_names()` /
Python `extract_python_package_names()` flow that the May-04 manifest PRD
introduced. Verified by the test `analyze_manifests_package_json_has_empty_internal_names`
at `manifest.rs:1186`. Result: `import { Button } from "@myorg/shared"` in any
npm/yarn/pnpm workspace is classified as external and silently dropped from
the dependency graph.

## Goal

Extract workspace package names from root `package.json` `"workspaces"` field
(or the pnpm equivalent), persist them into the same `workspace_crates`
key that the Rust/Python pipeline already uses, and teach
`crates/seshat-graph/src/dependencies.rs::resolve_import` to match JS/TS
absolute internal imports against that list with the longest-prefix-wins
rule from the parent PRD.

## Scope (matches parent PRD §I)

In scope:
- Root `package.json` `"workspaces"` field — both forms:
  - `"workspaces": ["packages/*", "apps/*"]` (array)
  - `"workspaces": { "packages": ["packages/*"], "nohoist": [...] }` (object — Yarn classic)
- `pnpm-workspace.yaml` `packages:` field (parallel to `"workspaces"`)
- Glob expansion in workspace patterns (delegates to the same `glob` crate
  pulled in by FW-1; if FW-1 not yet merged, this spec adds it itself)
- Scoped (`@org/pkg`) and unscoped (`my-pkg`) workspace names
- Single-package projects: root `"name"` if present
- Union with `local_packages` config (same merge semantics as Rust/Python)

Out of scope (separate roadmap items):
- `tsconfig.json` `paths` aliases (`#jsts-path-aliases`)
- Special handling for Turborepo/Nx/Lerna metadata files (`#jsts-monorepo-tools`)
- `node_modules` traversal

## Design

### Code locations

| File | Symbol | Change |
|---|---|---|
| `crates/seshat-scanner/src/manifest.rs:343` | `parse_package_json` | extend to extract `workspaces` and root `name` into a new field on `ManifestAnalysis`; alternatively, introduce `extract_js_package_names(path, content) -> Vec<String>` parallel to `extract_crate_names()` for symmetry |
| `crates/seshat-scanner/src/manifest.rs` | new `fn extract_js_package_names(path: &Path, content: &str, manifest_dir: &Path) -> Vec<String>` | parses `"workspaces"` (array OR object form), expands globs via `glob` crate, reads each matched dir's `package.json`, returns normalized names; also handles root `"name"` |
| `crates/seshat-scanner/src/manifest.rs` | new `fn parse_pnpm_workspace_yaml(path: &Path) -> Vec<String>` | parses `pnpm-workspace.yaml` `packages:` field; called from the orchestrator only when root `package.json` lacks `"workspaces"` |
| `crates/seshat-scanner/src/orchestrator.rs:467` | the `workspace_crates` union+persist block | include JS/TS names in the union (alongside Rust/Python ones it already merges) |
| `crates/seshat-graph/src/dependencies.rs::resolve_import` | the resolver chain | after the existing `starts_with('.')` short-circuit, add JS/TS-internal branch: for each internal name (longest first), match exact OR prefix-with-`/` |
| `crates/seshat-graph/src/dependencies.rs` | new `fn resolve_js_internal_import(module: &str, internal_names: &[String], suffix_index: &SuffixIndex) -> Option<PathBuf>` | implements the table from PRD §II.C |

### Resolution rules (recap from parent PRD)

| Import | Internal name | Action |
|---|---|---|
| `@myorg/shared` | `@myorg/shared` | exact match → resolve to package root (`index.{ts,js}` via suffix index) |
| `@myorg/shared/utils` | `@myorg/shared` | prefix match → strip `@myorg/shared/`, resolve `utils` via suffix index |
| `my-app/components` | `my-app` | prefix match → strip `my-app/`, resolve `components` via suffix index |
| `react` | not in list | external (`None`) |

Longest-prefix-wins so that `@myorg/shared` is checked before `@myorg/sh`.
Sort `internal_names` by length-desc once at the top of
`resolve_js_internal_import`.

### Storage

Shared `repo_metadata.workspace_crates` key (see parent PRD §II — explicit
unification with the Rust/Python pipeline). The key name is **kept as
`workspace_crates`** even for JS/TS; renaming is out of scope and would
churn migrations for zero behaviour gain.

> **Note on FW-5:** if `spec-fw5-per-branch-workspace-crates.md` lands
> first, this spec's persist step writes to the per-branch slot instead.
> Otherwise the global slot. Resolver code is agnostic — it reads via
> `load_internal_names(conn, branch_id)`.

## Acceptance Criteria (consolidates parent PRD §IV)

- [ ] `parse_package_json` continues to extract DECLARED dependencies
  exactly as before — no regression in
  `analyze_manifests_end_to_end` (`manifest.rs:1206`).
- [ ] `extract_js_package_names()` returns the expected list for the
  11 test rows in PRD §III. New unit tests inline in `manifest.rs::tests`.
- [ ] `analyze_manifests_package_json_has_empty_internal_names` is
  renamed to `..._has_populated_internal_names` and asserts the workspace
  names are returned.
- [ ] `parse_pnpm_workspace_yaml` handles a typical `packages: ["packages/*"]`
  layout; new unit test with a fixture YAML.
- [ ] Orchestrator union (`orchestrator.rs:481`) adds JS/TS names to the
  existing set; new unit test reads workspace `package.json` files,
  asserts persisted JSON contains all expected names + any
  `local_packages` extras.
- [ ] `resolve_js_internal_import` resolves the four cases in the
  Resolution Rules table; new unit tests in
  `crates/seshat-graph/src/dependencies.rs::tests`.
- [ ] End-to-end: scan a fixture monorepo
  (`crates/seshat-scanner/tests/fixtures/js_monorepo/`) with two workspace
  packages, then `query_dependencies` on a file in package A returns a
  dependent in package B.
- [ ] `cargo test --workspace` passes.
- [ ] `cargo clippy --workspace -- -D warnings` clean.
- [ ] `cargo fmt --check` clean.

## Fixture

Create `crates/seshat-scanner/tests/fixtures/js_monorepo/`:

```
js_monorepo/
├── package.json                  ← "workspaces": ["packages/*"]
├── packages/
│   ├── shared/
│   │   ├── package.json          ← "name": "@myorg/shared"
│   │   └── src/index.ts          ← export const Button = …
│   └── web/
│       ├── package.json          ← "name": "@myorg/web"
│       └── src/App.ts            ← import { Button } from "@myorg/shared"
```

Used by the e2e test in the dependencies test module.

## Risks

- **`pnpm-workspace.yaml` parsing:** adds a YAML dependency
  (`serde_yml` already in tree per the Apr/May dep bumps — confirm).
- **Yarn classic object form:** `"workspaces": { "packages": [...] }`
  vs the simple array form requires `#[serde(untagged)]` enum or a
  manual `Value` walk. The parent PRD glosses over this — be explicit
  about both shapes in the deserializer.
- **Scope sensitivity in matching:** `@myorg/shared` is a single
  "name" token (with a slash inside it). Suffix-index lookups must NOT
  split `@myorg/shared` into two segments. The exact-vs-prefix branch
  takes care of this because the comparison is string-level before any
  module-segment work.

## Suggested Implementation Order

1. Fixture under `tests/fixtures/js_monorepo/`.
2. `extract_js_package_names()` in `manifest.rs` + unit tests (TDD).
3. `parse_pnpm_workspace_yaml()` + unit test.
4. Orchestrator union — wire in, update persistence test.
5. `resolve_js_internal_import()` in `dependencies.rs` + unit tests.
6. End-to-end test via the fixture.
7. Lints, fmt, smoke run against any local npm workspace project.

## Dependencies

- **FW-1 (Glob Workspace Members):** soft dep — both pull in the `glob`
  crate. If FW-1 lands first, this spec just reuses it.
- **FW-5 (Per-Branch workspace_crates):** orthogonal. This spec writes
  to whatever slot `load_internal_names` reads from at the time of
  implementation.
