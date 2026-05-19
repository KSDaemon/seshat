# Seshat Roadmap

> Consolidated list of future features and improvements.
> Last updated 2026-05-18. Sources: `epics.md`, `.ralph/tasks/*.md`, codebase analysis.

## Status as of 2026-05-18

All 14 epics (1–12 including 3.5 and 6.5, plus Epic 14) — **COMPLETED**. Fully functional product: scanning, convention detection, MCP server with 9 tools, TUI review wizard, file watcher, branch-aware knowledge graph, auto-scan, init/update/uninstall, project-wide merge-aware decisions with git-state freshness checks.

**Latest delivery — FW-5: Per-Branch Workspace Crates** (branch `feat/per-branch-workspace-crates`, 2026-05-18). `workspace_crates` moved from project-wide `repo_metadata` to a new per-branch `branch_metadata` table (V14 migration). Eliminates cross-branch contamination of internal-name resolution in `query_dependencies` when two branches declare different `[workspace] members`. See `.ralph/prd.json` on `feat/per-branch-workspace-crates` and ADR `_bmad-output/planning-artifacts/15-1-branch-metadata.md`.

**Previous delivery — Epic 14: Merge-aware Decisions and DB Freshness** (branch `feat/merge-aware-decisions`). User decisions migrated from branch-scoped `nodes.ext_data` to a project-wide `decisions` table (V11/V12 migrations, no data migration — pre-1.0 wipe). `seshat serve` startup detects same-branch HEAD movement; `seshat review` performs a blocking incremental sync before opening the TUI. New `seshat decisions <list|forget|export|import>` CLI subcommand. Git-optional fallback locked behind regression tests. See `.ralph/tasks/prd-merge-aware-decisions.md` and ADR `_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md`.

**Also landed (off-epic):**

- **Call-site evidence multi-language** — `query_code_pattern` returns real call-site snippets across all four supported languages. Rust phase merged as commit `84ff359` (IR v6); TypeScript/JavaScript/Python extension merged as commit `85bf081` (IR v7). Shared `collect_calls_bfs` helper lives in `crates/seshat-scanner/src/parser/mod.rs`, called by all four parsers; `enrich_with_call_sites` in `crates/seshat-graph/src/code_pattern.rs` is wired into the pipeline. See `story-query-code-pattern-call-sites.md` and `story-call-sites-multilang.md`.
- **Post-Epic-14 bug-fix sprint** (latest 3 commits on `main`): Bug #1 unify project resolver so worktrees share one DB (`37b271a`), Bug #2 propagate `source_map` through incremental detection (`ac36f94`), Bug #3 store `files_ir` paths relative to `project_root` (`0ac9a49`).

---

## Near-Term (M1-M2)

These features have the highest priority — closing clear gaps in the current product.

### Daemon Mode [#daemon]

Multi-project mode: `seshat serve --daemon` with HTTP/SSE transports, serving multiple projects simultaneously.

- **Blocks:** SSE/HTTP transport (currently stdio only)
- **Source:** Epic 6 non-goal, `prd-submodule-support-scoped-queries.md`

### ~~Shell Completions~~ [#shell-completions] — ✅ IMPLEMENTED 2026-05-09

`seshat completions [SHELL]` subcommand generates bash/zsh/fish/powershell/elvish scripts via `clap_complete`. Without an explicit `<shell>` argument, the target is auto-detected from `$SHELL` (basename → `Shell` enum, with `.exe` suffix stripped for Windows paths); on Windows fallback is PowerShell, otherwise a friendly error lists the supported shells.

- Added `clap_complete = "4"` to workspace; `seshat-cli` consumes it.
- Implementation: `crates/seshat-cli/src/completions.rs` (~85 LOC + tests).
- 11 integration tests in `crates/seshat-bin/tests/completions_integration.rs` covering all five shells, env autodetect, Windows path with `.exe`, unknown shell error, missing `$SHELL` error, and explicit-overrides-detect.
- `seshat completions` skips the background update notice (clean stdout for `eval`-pipes).
- Release pipeline: new `generate-completions` job in `release.yml` builds the binary once on Ubuntu, generates all five scripts, uploads them as artifacts. Each per-platform `build-binaries` job downloads them and bundles into the release archive's `completions/` subfolder. Standalone `seshat-completions.tar.gz` is also published as a release asset.

### ~~Homebrew Formula~~ [#homebrew] — ✅ COMPLETE (verified end-to-end on v0.3.2, 2026-05-19)

Self-rendering tap pipeline, live and shipping:

- `homebrew/seshat.rb` — formula template with per-arch URLs and SHA256 placeholders. Uses Homebrew's `bash_completion` / `zsh_completion` / `fish_completion` helpers to install the bundled scripts into the right shell paths.
- `.github/workflows/homebrew-bump.yml` — fires on `release: published` (or manual `workflow_dispatch` with a `tag` input). Downloads the three Unix tarballs (`aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`) from the release, computes SHA256s, renders the formula, checks out `KSDaemon/homebrew-seshat` via `actions/checkout@v4` (Basic-auth — GitHub's git-over-HTTPS endpoint rejects Bearer), commits `Formula/seshat.rb`, pushes. Intel-Mac (`x86_64-apple-darwin`) is intentionally absent — the `ort` crate (ONNX Runtime via fastembed) no longer ships prebuilt binaries for that target.
- `homebrew/README.md` — bootstrap instructions retained for posterity. Workflow self-skips when `HOMEBREW_TAP_TOKEN` is missing, so the rest of the release pipeline is unaffected.
- End-user install: `brew tap KSDaemon/seshat && brew install seshat` — confirmed working on macOS arm64.

Bootstrap (completed 2026-05-19):

1. ✅ `KSDaemon/homebrew-seshat` repo created (public)
2. ✅ Fine-grained PAT with `Contents: Read and write` scoped to the tap repo
3. ✅ `HOMEBREW_TAP_TOKEN` secret registered in `KSDaemon/seshat`
4. ✅ Tap repo seeded with an initial README commit (`actions/checkout@v4` cannot operate on an unborn HEAD)

Post-launch fixes shipped in #33 (release asset naming alignment — archives now embed the tag suffix) and #34 (auth scheme — `actions/checkout@v4` for canonical Basic-auth header).

### Windows Self-Update [#win-update] — 🚧 IN PROGRESS

Self-update on Windows. Brings `seshat update` and `seshat update --check` to parity with macOS/Linux: `.zip` extraction in `extract_binary`, `self_replace`-based atomic binary replacement, Windows target detection in `current_target`/`find_binary_asset`/`fetch_checksum_for_asset`, `.exe.old` cleanup at startup, and a `windows-latest` entry in the CI test matrix.

- **In scope (this milestone):** `x86_64-pc-windows-msvc` self-update on direct (curl/zip) installs.
- **Out of scope (follow-up below):** Scoop / Chocolatey / winget package-manager detection.
- **Source PRD:** `prd-seshat-windows-self-update.md` (supersedes the original Windows non-goal in `prd-seshat-self-update.md`)

### Windows Package-Manager Detection [#win-pkg-mgr]

Detect Scoop / Chocolatey / winget installs on Windows and route the user to the package manager's update flow instead of running self-update (mirroring the existing macOS Homebrew arm of `detect_install_method`).

- **Depends on:** `#win-update` (lands first; this entry handles only the install-method-detection follow-up)
- **Source:** `prd-seshat-windows-self-update.md` follow-up

### ~~Code Review Deferred Items (Tech Debt)~~ [#tech-debt] — ✅ COMPLETED

From `prd-tech-debt-cleanup-2026-05-02.md` — all 14 active items shipped (verified against `main` 2026-05-09):

| ID | What | Where landed |
|---|---|---|
| D5 | `STOP_WORDS` filter in `extract_keywords` | `seshat-graph/src/validate_approach.rs:35,391` |
| D6 | `find_decisions`/`find_observations` reuse FTS5 via `query_convention` | `seshat-graph/src/validate_approach.rs:571,603` |
| D7 | f64 accumulators in `cosine_similarity` | `seshat-graph/src/code_pattern.rs:274–278` |
| D8 | `SuffixIndex` HashMap (O(N×D) build, O(1) resolve) + 14 unit tests | `seshat-graph/src/dependencies.rs:116–138, 1258+` |
| D9 | Workspace-crate detection — dynamic, loaded from `repo_metadata.workspace_crates` (better than the hardcoded plan) | `seshat-graph/src/dependencies.rs:357–366,479–483` |
| D10 | `call_logger_keys.rs` shared constants + `tracing::debug!` on missing keys | `seshat-mcp/src/call_logger_keys.rs`, `lib.rs:19`, `call_logger.rs:17,57…` |
| D12 | MCP `validate_approach` no longer trims (graph layer trims once) | `seshat-mcp/src/tools/validate_approach.rs` |
| D13 | Drop redundant `idx_code_embeddings_branch` | migration `V9__drop_redundant_embedding_index.sql` |
| D14 | `code_embeddings.updated_at` timestamp | migration `V10__add_embedding_updated_at.sql` |
| D15 | Safe `usize::try_from(count).unwrap_or(0)` cast | `seshat-storage/src/repository/embedding_repository.rs:182` |
| D16 | `LoadedIR { files, truncated }` propagated through `CodePatternData` / `DependenciesData` to JSON envelope | `seshat-graph/src/code_pattern.rs:35–39, 152–195` |
| D17 | `MAX_LIKE_KEYWORDS=5`, sort-by-length-desc + AND join in `build_keyword_like` | `seshat-graph/src/validate_approach.rs:396–408` |
| D18 | Reject `..` path components in `query_dependencies` (component-aware, not substring) | `seshat-mcp/src/tools/query_dependencies.rs:69–81` |
| D19 | `delete_stale(branch_id, &keys)` with batches of 100 + 5 unit tests; pruning wired into scan | `seshat-storage/.../embedding_repository.rs:207`, `seshat-cli/src/scan.rs:852,925` |

Status confirmed by direct code inspection 2026-05-09. The PRD has been archived to `.ralph/tasks/archive/prd-tech-debt-cleanup-2026-05-02.md` (frontmatter: `status: COMPLETED`, `completed: 2026-05-09`). KSD final review pass (PR 5) is technically still on the table but no longer blocking — all CRITICAL items are absent in the shipped code.

3 items remain deferred to M2+ (see Long-Term section):

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

### ~~FW-5: Per-Branch Workspace Crates Scoping~~ [#fw5-branch-crates] — ✅ IMPLEMENTED 2026-05-18

`workspace_crates` moved from the project-wide `repo_metadata` slot to a new per-branch `branch_metadata` table (V14 migration). The scanner now writes the set keyed by the scanned branch's `branch_id`; the graph layer's `load_internal_names` reads keyed by the queried branch's `branch_id`. `BranchRepository::create_snapshot` copies `branch_metadata` rows so a freshly-forked branch inherits its parent's workspace membership until the next full scan refreshes it. Cross-branch contamination of internal-name resolution is locked behind a regression test (`crates/seshat-cli/tests/cross_branch_workspace_crates.rs`). Shipped on branch `feat/per-branch-workspace-crates` across US-001..US-007. See ADR `_bmad-output/planning-artifacts/15-1-branch-metadata.md` and CHANGELOG `[Unreleased]`.

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
