# Seshat Roadmap

> Consolidated list of future features and improvements.
> Last updated 2026-05-09. Sources: `epics.md`, `.ralph/tasks/*.md`, codebase analysis.

## Status as of 2026-05-09

All 14 epics (1–12 including 3.5 and 6.5, plus Epic 14) — **COMPLETED**. Fully functional product: scanning, convention detection, MCP server with 9 tools, TUI review wizard, file watcher, branch-aware knowledge graph, auto-scan, init/update/uninstall, project-wide merge-aware decisions with git-state freshness checks.

**Latest delivery — Epic 14: Merge-aware Decisions and DB Freshness** (branch `feat/merge-aware-decisions`). User decisions migrated from branch-scoped `nodes.ext_data` to a project-wide `decisions` table (V11/V12 migrations, no data migration — pre-1.0 wipe). `seshat serve` startup detects same-branch HEAD movement; `seshat review` performs a blocking incremental sync before opening the TUI. New `seshat decisions <list|forget|export|import>` CLI subcommand. Git-optional fallback locked behind regression tests. See `.ralph/tasks/prd-merge-aware-decisions.md` and ADR `_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md`.

---

## Near-Term (M1-M2)

These features have the highest priority — closing clear gaps in the current product.

### Daemon Mode [#daemon]

Multi-project mode: `seshat serve --daemon` with HTTP/SSE transports, serving multiple projects simultaneously.

- **Blocks:** SSE/HTTP transport (currently stdio only)
- **Source:** Epic 6 non-goal, `prd-submodule-support-scoped-queries.md`

### Homebrew Formula [#homebrew]

Create a Homebrew formula/tap for macOS installation.

- **Source:** `prd-seshat-self-update.md` non-goal

### Windows Self-Update [#win-update]

Self-update on Windows (currently shows a graceful "not supported" message).

- **Source:** `prd-seshat-self-update.md` non-goal

### Code Review Deferred Items (Tech Debt) [#tech-debt]

From `prd-tech-debt-cleanup-2026-05-02.md` — 14 open items (planned as 5 PRs):

1. **Quick Wins** — minor fixes (renames, unused imports, cleanup)
2. **Search Quality** — semantic search quality improvements
3. **Infra Robustness** — infrastructure reliability
4. **Graph Performance** — graph query performance
5. **KSD Code Review** — final review of all cleanup

Plus 3 items deferred to M2+ (see Long-Term section):

- **D20**: Inline embedding generation during scan
- **D22**: `sqlite-vec` ANN search
- **D23**: Per-function import usage analysis

---

## Mid-Term (M2-M3)

Features that significantly improve the product but require more engineering effort.

### Transitive Dependents [#transitive-deps]

`query_dependencies` currently only analyzes direct dependencies. Add transitive (2nd and 3rd order).

- **Source:** `prd-advanced-mcp-tools.md` deferred

### Call Graph Extraction [#call-graph]

AST analysis to build a call graph: who calls whom within the project.

- **Depends on:** possibly D23 (per-function import usage)
- **Source:** `prd-advanced-mcp-tools.md` deferred to M2+

### Content-Level Diff Analysis [#content-diff]

`map_diff_impact` currently only analyzes which files changed. Add analysis of exactly which lines/functions changed within a file.

- **Source:** `prd-map-diff-impact.md` future enhancement

### Submodule Recursive Scanning [#deep-submodules]

Currently only first-level submodules are scanned. Add recursive traversal.

- **Source:** `prd-submodule-support-scoped-queries.md` non-goal

### Submodule Convention Inheritance [#submodule-inherit]

Convention inheritance from root project to submodules (and vice versa). Currently each scope is fully independent.

- **Source:** `prd-submodule-support-scoped-queries.md` non-goal

### Cross-Scope Queries [#cross-scope]

Ability to query data across multiple scopes simultaneously (root + submodules).

- **Source:** `prd-submodule-support-scoped-queries.md` deferred to Epic 7

---

## Long-Term (M3+)

Improvements that make the product more complete but are not critical to the core value proposition.

### D20: Inline Embedding Generation during Scan [#d20-inline-emb]

Generate embeddings during scanning (rather than as a separate step). Requires scan pipeline reorganization.

- **Source:** `prd-tech-debt-cleanup-2026-05-02.md`

### D22: sqlite-vec ANN Search [#d22-sqlite-vec]

Use the sqlite-vec extension for ANN (approximate nearest neighbour) search. Depends on a C extension, requires cross-platform builds.

- **Source:** `prd-tech-debt-cleanup-2026-05-02.md`

### D23: Per-Function Import Usage Analysis [#d23-func-imports]

Analyze import usage at the individual function level (rather than file-level). Requires body AST analysis.

- **Source:** `prd-tech-debt-cleanup-2026-05-02.md`

---

## Manifest Parsing Improvements [#manifest]

From Appendix in `epics.md` and `prd-rust-python-manifest-parsing-2026-05-04.md`.

### FW-1: Glob Workspace Members Resolution [#fw1-glob]

Expand glob patterns in `[workspace.members]` (`crates/*` → actual crate names).

- **Affects:** Epic 2 (Scanning), Epic 7 (Dependencies)

### FW-2: Legacy Python Manifests [#fw2-legacy-py]

Parse `setup.cfg` and `setup.py` (currently only `pyproject.toml`).

- **Affects:** Epic 2 (Scanning), Epic 7 (Dependencies)

### FW-3: Nested Manifest Discovery [#fw3-nested-manifest]

Discover manifests not only in the project root but also in subdirectories (for monorepos).

- **Affects:** Epic 2 (Scanning), Epic 7 (Dependencies)

### FW-4: Non-Poetry Build Backends [#fw4-alt-backends]

Support for PDM (`[tool.pdm]`), Hatchling, Flit, Maturin in `pyproject.toml`.

- **Affects:** Epic 2 (Scanning)

### FW-5: Per-Branch Workspace Crates Scoping [#fw5-branch-crates]

`workspace_crates` is currently stored globally, not per-branch. If different branches have different sets of crates, data gets mixed.

- **Affects:** Epic 11 (Branch-Aware), Epic 7 (Dependencies)

---

## JS/TS Ecosystem Improvements [#jsts]

From `prd-js-ts-workspace-detection-2026-05-04.md`.

### JS/TS: Monorepo Detection [#jsts-monorepo]

Traverse `node_modules` to identify internal workspace packages in npm/yarn/pnpm monorepos.

### JS/TS: tsconfig.json Path Aliases [#jsts-path-aliases]

Resolve path aliases like `@app/*` → `src/*` via `tsconfig.json`.

### JS/TS: Monorepo Tools [#jsts-monorepo-tools]

Special handling for Turborepo, Nx, Lerna to detect workspace structure.

- **Depends on:** PRD #1 (Rust+Python manifest parsing)

---

## TUI Improvements [#tui]

Improvements to the TUI Review Wizard (Epic 12).

### TUI: Scrolling Oversized Snippets [#tui-scroll]

Code snippets are currently truncated if they don't fit. Add scrolling (PgUp/PgDn or vim-like navigation).

- **Source:** `prd-tui-review-wizard-v3.md` non-goal (future)

### TUI: Terminal Resize Detection [#tui-resize]

Terminal resize is currently silently ignored (`non-key event silently discarded`). Add layout re-rendering on terminal size changes.

- **Source:** `prd-tui-review-wizard-v3.md` non-goal (future)
- **Status:** ❌ not implemented

### TUI: Color Theme Customization [#tui-theme]

Ability to customize the TUI color scheme.

- **Source:** `prd-tui-review-wizard-v3.md` non-goal

### TUI: "Show All" Mode [#tui-show-all]

View all conventions at once (instead of one at a time).

- **Source:** `prd-tui-review-wizard-v3.md` non-goal

### TUI: Regex Search [#tui-regex]

Search by regular expression (currently fuzzy match + substring).

- **Source:** `prd-tui-search-filter-and-diagnostics.md` non-goal
- **Status:** ❌ not implemented

### TUI: Cross-Session Filter Persistence [#tui-filter-persist]

Persist the filter across `seshat review` sessions.

- **Source:** `prd-tui-search-filter-and-diagnostics.md` non-goal
- **Status:** ❌ not implemented

---

## Non-Goals / Deferred Indefinitely

Items explicitly named as non-goals in PRDs with no estimated timeline:

- Custom domain for curl installer
- Interactive prompts in installer
- GPG verification of binaries
- API response caching
- `seshat update` with `--version v0.2.0` (version pinning)
- Downgrade support
- Automatic background daemon updates
- Post-update restart/respawn
- Pre-release tags for self-update
- Wildcard imports in evidence cross-reference (`use foo::*`)
- Semantic analysis (non string-based match) in evidence cross-reference
- Search history in TUI
- Highlighted matching text in TUI results
- CLI arguments for pre-set filter in TUI
- Cross-scope dependency analysis
- Automatic re-embedding on file changes (embedded once, not updated)
- File watcher for submodules
- Automatic GC of orphaned submodule DBs
- Log rotation for call-log.jsonl
- Dashboard/built-in UI for call-log analysis
- Full response body logging (summary scalars only)
- Caller/sub-agent identification in call log
- Encryption/compression for call log
- Third-party registries beyond crates.io, npm, PyPI (custom registries)
- Type-aware parameter analysis (parameter names only, not types)
- Pre-commit hook for map_diff_impact (optional, advisory)
- Hunks analysis (changed files only in diff_impact)
- Auto-fix violations (reporting only in diff_impact)

---

## Legend

- `#tag` — unique tag for linking and search
- **bold** — priority/blocking item
- ❌ — verified in code, not implemented
- Sources are listed for each item
