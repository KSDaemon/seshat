# Quick Tech Spec — Manifest Parsing: pnpm wiring + FW-4 (Poetry/PDM)

> Status: in progress · Created 2026-05-25 · Branch `feat/manifest-pnpm-fw4`
> Scope: roadmap items `#jsts-pnpm` (⚠️ PARTIAL → ✅) and `#fw4-alt-backends` (⚠️ MOSTLY COVERED → ✅)
> All changes confined to `crates/seshat-scanner/src/manifest.rs` (blast radius: low, 1 dependent).

## Context

`crates/seshat-scanner/src/manifest.rs` parses dependency manifests and extracts
internal package names for `query_dependencies` internal-vs-external resolution.
Two known gaps remain:

1. **pnpm** — `parse_pnpm_workspace_yaml` (manifest.rs:650) is fully implemented but
   `#[allow(dead_code)]`; never invoked from production. pnpm monorepos declare
   members in `pnpm-workspace.yaml`, not the `package.json` `"workspaces"` field,
   so their internal packages resolve as *external* today.
2. **FW-4** — `parse_pyproject_toml` (manifest.rs:724) only reads PEP 621
   `[project].dependencies` / `[project.optional-dependencies]`. Legacy Poetry and
   PDM declare deps in tool-specific tables that are invisible to cross-referencing.

## Design

### 1. pnpm wiring (`#jsts-pnpm`)

In `analyze_manifests` (manifest.rs:99), the `ManifestType::PackageJson` arm of the
`internal_names` match currently calls only `extract_js_package_names`. Extend it:
after computing the package.json names, look for a **sibling** `pnpm-workspace.yaml`
in the same directory as the manifest; if present, merge
`parse_pnpm_workspace_yaml(&sibling)` results, then `sort` + `dedup`.

- Self-contained in `analyze_manifests` — **no orchestrator change** (discovery is by
  path from the package.json that's already in the manifest list).
- Remove `#[allow(dead_code)]` and the "Not yet called" comment on
  `parse_pnpm_workspace_yaml`.
- Names stay **verbatim** (`@scope/name`, hyphens preserved) — both functions already
  honor the recorded JS/TS verbatim-naming decision; merging two verbatim lists keeps it.
- Scoped to "pnpm-workspace.yaml alongside a root package.json" (the canonical layout).
  A pnpm workspace with no root `package.json` is out of scope (no manifest entry to
  hang off of) — acceptable per roadmap framing.

### 2. FW-4 Poetry/PDM (`#fw4-alt-backends`)

Extend the `pyproject.toml` deserialization with a `[tool]` table and parse the
non-PEP-621 dependency sources in `parse_pyproject_toml`:

| Source | Shape | is_dev |
|---|---|---|
| `[tool.poetry.dependencies]` | `name = "^1.0"` or `name = { version = "^1.0", ... }` | false |
| `[tool.poetry.group.<g>.dependencies]` | same | true if `<g>` ∈ {dev,test,testing} |
| `[tool.poetry.dev-dependencies]` (legacy ≤1.1) | same | true |
| `[tool.pdm.dev-dependencies]` | `<group> = ["pep508-spec", ...]` | true |

- Poetry dep **value** → version: string is the version; table → its `version` key
  (or `"*"` if absent, e.g. git/path deps); anything else → `"*"`.
- Skip the reserved `python` key in Poetry dependency tables (it's the interpreter
  constraint, not a package).
- Normalize Poetry/PDM names with **lowercase + `-`→`_`** (same as
  `parse_pep508_name_version`) so they match `count_files_importing`'s normalized
  module comparison. PDM values are PEP 508 strings → reuse `parse_pep508_name_version`.
- **Dedup against PEP 621**: collect PEP 621 dep names first; skip any Poetry/PDM dep
  whose normalized name is already present. Prevents double-counting on Poetry 2.0 /
  hybrid projects that mirror deps into both `[project]` and `[tool.poetry]`.

New private structs (serde, all `#[serde(default)]`, unknown fields ignored):
`PyprojectTool { poetry, pdm }`, `PoetryTool { dependencies, dev_dependencies, group }`,
`PoetryGroup { dependencies }`, `PdmTool { dev_dependencies }`. Poetry dep tables typed
as `HashMap<String, toml::Value>` to absorb the string-or-table value shapes.

## Files

- `crates/seshat-scanner/src/manifest.rs` — only file changed (impl + tests).

## Tests (in the existing `#[cfg(test)] mod tests`)

- pnpm wiring through `analyze_manifests`: root `package.json` + sibling
  `pnpm-workspace.yaml` + `packages/*` members → `internal_names` contains members.
- pnpm: root package.json WITHOUT a `"workspaces"` field but WITH `pnpm-workspace.yaml`
  (the real pnpm case) still resolves members.
- Poetry: `[tool.poetry.dependencies]` parsed, `python` skipped, table-form dep version
  extracted, `[tool.poetry.group.dev.dependencies]` flagged dev.
- Poetry legacy `[tool.poetry.dev-dependencies]` flagged dev.
- PDM: `[tool.pdm.dev-dependencies]` groups parsed as dev.
- Dedup: same dep in `[project.dependencies]` and `[tool.poetry.dependencies]` counted once.

## Out of scope

- pnpm catalogs / `catalog:` protocol, nested non-root pnpm workspaces.
- Poetry `path`/`git`/`url` source resolution beyond name+version extraction.
- tsconfig path aliases, Turbo/Nx/Lerna (separate roadmap items).

## Risks / notes

- `serde_yml` and `toml` are already dependencies — no `Cargo.toml` change.
- Non-breaking → commits use `feat:` (NOT `feat!:`). The v0.4.0 minor bump comes from
  the already-merged FW-5 V14 breaking change in `[Unreleased]`; verify that commit
  carries the breaking marker before release (release-plz reads commit messages).
