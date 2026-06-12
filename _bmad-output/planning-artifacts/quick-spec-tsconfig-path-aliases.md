---
title: "Quick Spec — JS/TS tsconfig.json Path Aliases"
status: IMPLEMENTED
created: 2026-06-12
scope: full-resolution
roadmap-tag: "#jsts-path-aliases"
branch: feat/tsconfig-path-aliases
---

# Quick Spec — tsconfig.json Path Aliases

## Problem

TypeScript/JavaScript projects routinely remap import specifiers via
`tsconfig.json` `compilerOptions.paths` (+ `baseUrl`):

```jsonc
{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@app/*": ["src/*"],
      "@lib/*": ["packages/lib/src/*"],
      "@config": ["src/config/index.ts"]
    }
  }
}
```

Seshat has **no `tsconfig.json` parsing today**. Consequently an
`import { x } from '@app/utils'` is treated as an **external** package in
`query_dependencies`: it never resolves to the real file (`src/utils.ts`),
produces no internal dependency edge, no reverse-dependent edge, and can be
mis-reported as an external dependency. This is the last open item in the
"JS/TS Ecosystem Improvements" roadmap bucket alongside the (deferred)
monorepo-tool detectors.

## Goal

Resolve aliased imports to real project files so the dependency graph is
correct for alias-using TS/JS codebases:

- `@app/utils` → `src/utils.ts` (wildcard `*` substitution)
- `@config` → `src/config/index.ts` (exact, non-wildcard mapping)
- Alias imports classified **internal** (not external) → correct
  forward deps, reverse dependents, and dead-dependency accounting.

## Non-Goals (defer)

- `extends` chains that reach **outside** the project root (e.g. a published
  base config in `node_modules`). In-repo relative `extends` IS handled.
- Multiple `paths` targets fallback probing beyond first-match-wins per the
  resolution rules below (we DO try each target in order, but we do not model
  TS's full module-resolution algorithm).
- `tsconfig.json` discovery in nested subdirectories — same root-only scope as
  every other manifest today (tracked separately by `#fw3-nested-manifest`).
- Monorepo-tool configs (`turbo.json`, `nx.json`) — separate roadmap item.

## Design

Three crates, mirroring the existing `pnpm-workspace.yaml` → `workspace_crates`
pipeline (no new migration — `branch_metadata` is a generic per-branch KV
table introduced by FW-5).

### 1. Scanner — parse tsconfig (`crates/seshat-scanner/src/manifest.rs`)

- New `PathAlias { pattern: String, targets: Vec<String> }` value type and a
  `parse_tsconfig(path, content) -> Vec<PathAlias>` function.
- Parse as **JSONC**: tolerate `//` / `/* */` comments and trailing commas.
  (Reuse the existing tolerant-JSON approach used by `extract_js_package_names`
  for BOM/`json` quirks; strip comments before `serde_json`.)
- Read `compilerOptions.baseUrl` (default `.`) and `compilerOptions.paths`.
  Each target is joined to `baseUrl` and normalised (drop `./`, fold `\` → `/`),
  stored **verbatim** otherwise (per the JS/TS verbatim-names decision).
- `extends`: resolve a single string or an array of relative paths **within the
  project root**; shallow-merge (child `paths` win). Out-of-root `extends`
  silently skipped (logged via `tracing::warn!`).
- Wire into `analyze_manifests`: when a `tsconfig.json` sits beside the
  `package.json`, attach its aliases to the `ManifestAnalysis` (new
  `path_aliases: Vec<PathAlias>` field) — exactly where the pnpm merge happens.

### 2. Storage — persist per branch

- Scanner serialises the alias list to JSON and writes it to `branch_metadata`
  under a new key `tsconfig_path_aliases`, keyed by `branch_id`, written in the
  same full-scan path that writes `workspace_crates`.
- `BranchRepository::create_snapshot` already copies all `branch_metadata` rows,
  so forked branches inherit aliases until the next full scan — no extra work.

### 3. Graph — resolve at query time (`crates/seshat-graph/src/dependencies.rs`)

- `load_path_aliases(conn, branch_id) -> Vec<PathAlias>` next to
  `load_internal_names`; both loaded once in `query_dependencies` /
  `query_dependencies_batch` and threaded through.
- New alias arm in `resolve_import` (`resolve_alias_import`), tried **before**
  the external fallback. Pattern matching lives in `seshat-core`
  (`resolve_path_alias`) and is **most-specific-wins** (longest literal prefix,
  independent of declaration order — *not* first-declared):
  - Wildcard pattern (`@app/*`): if `module` starts with the prefix before `*`
    and ends with the suffix after it, the captured substring is substituted
    into each target's `*`; each candidate is resolved via
    `SuffixIndex::resolve_path` (a literal-path-fragment lookup that, unlike
    `resolve`, does not fold `.`→`/`). First candidate that resolves wins.
  - Exact pattern (`@config`): if `module == pattern`, resolve each target.
  - Returns the resolved real path (so `resolved: true`, real edge).
- `is_likely_internal` learns aliases too (so an alias import that fails to
  resolve to a concrete file is still recorded as an *unresolved internal*
  import, not silently dropped).
- Reverse view: `import_resolves_to_target` calls `resolve_alias_import` forward
  and compares the resolved path to the target, so `build_dependents` (depth-1)
  and `build_reverse_adjacency` (depth>1) agree with the forward edge exactly —
  no parallel suffix heuristic that could diverge on multi-target aliases.

## Test Plan (TDD)

Scanner unit tests (in `manifest.rs` `#[cfg(test)]`):
- wildcard alias, exact alias, multiple targets, `baseUrl` join, JSONC comments
  + trailing comma, in-root `extends` merge, out-of-root `extends` skipped,
  missing `compilerOptions`/`paths` degrades to empty (no panic).

Graph tests (`crates/seshat-graph/tests/` or inline):
- `@app/utils` resolves to `src/utils.ts` as a forward dependency.
- reverse: `src/utils.ts` lists the aliased importer as a dependent.
- exact `@config` → `src/config/index.ts`.
- unresolved alias import recorded as unresolved-internal, not external.
- no-alias project unchanged (regression).

End-to-end (optional, `seshat-cli` integration): a fixture project with a
`tsconfig.json` + aliased import resolves correctly through a real scan.

## Release

`feat:` commit, no schema change (`branch_metadata` key reuse only), no breaking
marker required. CHANGELOG `[Unreleased]` → Added. **Note:** release-plz in this
repo bumps `feat:` to a **MINOR** version even pre-1.0 (e.g. v0.5.1 → v0.6.0),
not a patch — see `ref_releaseplz_bump_behavior`. A plain dependency/internal
change would be the patch case; this user-facing feature is a minor.

## Files Touched

| Crate | File | Change |
|---|---|---|
| core | `dependency.rs` | `PathAlias` type + `resolve_path_alias` (pattern matching) |
| scanner | `manifest.rs` | `parse_tsconfig`, JSONC strip, `analyze_manifests` wiring, `path_aliases` on `ManifestAnalysis` |
| scanner | `orchestrator.rs` | Step 8c — write `tsconfig_path_aliases` to `branch_metadata` |
| graph | `dependencies.rs` | `load_path_aliases`, `SuffixIndex::resolve_path`, `resolve_alias_import` arm, reverse via `import_resolves_to_target`, `is_likely_internal` |
| (tests) | core + scanner + graph | per Test Plan |
