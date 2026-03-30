# PRD: Epic 4 — CLI Scan Report & First Impression (Stories 4.1–4.4)

## Introduction

**Type:** Feature

Implement the `seshat scan <path>` command with a polished, informative analysis report — the developer's first contact with Seshat. This wires the fully-implemented scan engine (`seshat-scanner::scan_project()`) to a real CLI with `clap` argument parsing, `indicatif` progress bars, and `owo-colors` colored output. The scan report shows language breakdown, detected conventions with confidence tiers and trends, submodules, and copy-paste next steps. This is the "wow moment" — what makes a developer say "this understands my project."

**Context:** Seshat is a Rust CLI + MCP server. Epics 1–3.5 are complete: 9-crate workspace, Tree-sitter parsing (4 languages), manifest analysis, documentation ingestion, 8 convention detectors with heuristic fallbacks, confidence scoring, trend detection (Rising/Stable/Declining), package registry metadata, wrapper/facade detection, and knowledge graph persistence. The scan engine (`scan_project()`) is fully tested (1,077 tests, 0 failures). However, the CLI is a skeleton: `seshat-bin/src/main.rs` has only `--version` with raw `std::env::args()`, and `seshat-cli` has only `CliError` + doc comments. No clap, no indicatif, no owo-colors are in the dependency tree yet. The `AppConfig` is fully implemented in `seshat-bin/src/config.rs` but not connected to any command. Detection pipeline runs end-to-end at library level — this epic connects it to the user.

## Goals

- Wire `clap` for CLI argument parsing with `scan`, `serve`, `status`, `review`, `init` subcommands (only `scan` implemented in this epic, others stubbed with "not yet implemented")
- Implement two-phase progress display: discovery count → scanning progress bar with known total (Story 4.1)
- Display project overview: language bars, module count, dependency count with ecosystem breakdown (Story 4.2)
- Display conventions with confidence tier bullets (●/◐/○), trend indicators, top findings, and copy-paste next steps (Story 4.3)
- Establish consistent CLI formatting patterns: section headers with box-drawing, verbosity levels (--quiet/default/--verbose), NO_COLOR support, error/hint output (Story 4.4)

## User Stories

### US-001: Basic `seshat scan` Command & Two-Phase Progress (Story 4.1)

**Description:** As a developer, I want to run `seshat scan <path>` and see scanning progress so that I know Seshat is working and how long it will take.

**Acceptance Criteria:**
- [ ] `clap` added to workspace dependencies and wired in `seshat-bin/src/main.rs` with derive-based subcommand parsing
- [ ] Subcommands defined: `scan` (implemented), `serve`/`status`/`review`/`init` (stubbed with "not yet implemented" message)
- [ ] `seshat scan <path>` validates path exists and is a directory; error with hint if not
- [ ] `seshat --version` / `-V` prints `seshat {version} ({git_hash})` (UX-DR52)
- [ ] Version header displayed at scan start: `seshat v{version}` (UX-DR1)
- [ ] Phase 1 output: `Discovering files... {count} found` — count updates in-place via `indicatif` (UX-DR2)
- [ ] Phase 2 output: `Scanning ████████░░░░ {done}/{total} [{elapsed}]` — progress bar with known total (UX-DR3)
- [ ] `indicatif` added to workspace dependencies for progress bars
- [ ] Scan pipeline executes end-to-end: discovery → parse → detect → aggregate → store
- [ ] Database created in XDG data directory (`dirs::data_dir()/seshat/repos/{project_name}.db`)
- [ ] `AppConfig` loaded from `seshat.toml` if present, otherwise defaults
- [ ] `tracing-subscriber` initialized with appropriate log level
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-002: Scan Report — Project Overview Section (Story 4.2)

**Description:** As a developer, I want the scan report to show project overview so that I immediately see what Seshat learned.

**Acceptance Criteria:**
- [ ] Section header: `── Project Overview ──────────...` using box-drawing characters (UX-DR4)
- [ ] Language breakdown with horizontal bar charts using `▓` (filled) and `░` (empty), percentage, and file count — sorted by percentage descending (UX-DR5):
  ```
  Languages     TypeScript 72% ▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░  1,204 files
                Python     24% ▓▓▓▓▓░░░░░░░░░░░░░░░    412 files
  ```
- [ ] Module count and dependency count with ecosystem breakdown in parentheses (UX-DR6):
  ```
  Modules       34 detected
  Dependencies  127 packages (98 npm, 29 pip)
  ```
- [ ] Submodules section if applicable: `{path}/ → {language} project ({count} files)` (UX-DR9)
- [ ] Data sourced from `ScanResult` + database queries (language counts from `files_ir`, dependencies from `declared_dependencies`, modules from `nodes` where type=module)
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-003: Scan Report — Conventions & Next Steps (Story 4.3)

**Description:** As a developer, I want the scan report to show detected conventions and next steps so that I see value immediately.

**Acceptance Criteria:**
- [ ] Section header: `── Conventions Detected ({count}) ──────...` (UX-DR7)
- [ ] Confidence tier summary with Unicode bullets (UX-DR7):
  ```
  ● 15 high confidence (>85%)
  ◐  6 medium confidence (50-85%)
  ○  2 low confidence (<50%)
  ```
- [ ] Top findings list: tier bullet + description + trend arrow + confidence percentage, sorted by confidence descending (UX-DR8):
  ```
  ● Import grouping: stdlib → external → internal  ↑  93%
  ● Error handling: thiserror with ? propagation    ─  91%
  ◐ Barrel exports from index.ts                   ↓  67%
  ```
  Trend indicators: `↑` Rising, `─` Stable, `↓` Declining, ` ` Unknown
- [ ] Next Steps section with copy-paste commands (UX-DR10):
  ```
  ── Next Steps ──────────────────
    Run  seshat review    to validate detected conventions
    Run  seshat serve     to start MCP server
    Run  seshat init      to generate MCP config
  ```
- [ ] Summary line + database path with human-readable size (UX-DR11):
  ```
  23 conventions detected. Run seshat review to validate.
  Database: ~/.local/share/seshat/repos/my-project.db (12.4 MB)
  ```
- [ ] Conventions queried from knowledge graph (`KnowledgeNode` where nature=Convention/Observation) with confidence, trend from ext_data
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-004: Output Formatting, Verbosity & Error Patterns (Story 4.4)

**Description:** As a developer, I want consistent CLI formatting with verbosity control so that output is readable with detail available on demand.

**Acceptance Criteria:**
- [ ] `owo-colors` added to workspace dependencies for colored output
- [ ] Section headers use box-drawing format: `── {Title} ──────...` padded to ~60 chars (UX-DR88)
- [ ] All messages follow `{level}: {message}` pattern (UX-DR87): `error:`, `warn:`, `info:`, `hint:`
- [ ] Error output: `error: {message}` (red) + blank line + `hint: {suggestion}` lines (UX-DR53)
- [ ] `NO_COLOR` environment variable respected — disables all color output (UX-DR53)
- [ ] `--quiet` flag: errors + final summary line only — no warnings, no findings list (UX-DR57)
- [ ] Default (no flags): errors + warnings + summary + key findings (UX-DR58)
- [ ] `--verbose` flag adds: skipped files section with reasons (UX-DR12), detector details table (UX-DR13), timing breakdown (UX-DR14)
- [ ] Verbose skipped files: `warn: {path} — {reason}` with count in header
- [ ] Verbose detector details: detector name, files analyzed, findings count, execution time (ms) — column-aligned
- [ ] Verbose timing: Discovery, Parsing (with core count), Detection, Storage, Total — right-aligned durations
- [ ] Shared formatting functions extracted to `seshat-cli/src/format.rs` — reusable across all future commands
- [ ] Code/config snippets in bordered boxes with `┌─┐│└─┘` characters (UX-DR89) — for `init` command later, implement the utility now
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-005: Branch Code Review & Quality Gate (Final Step)

**Description:** As a developer, I want a comprehensive code review of all changes in the feature branch before merging so that the code meets professional Rust quality standards.

**Acceptance Criteria:**

**Automated checks — all must pass:**
- [ ] `cargo fmt --all --check` — no formatting violations
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — no lint warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] `cargo doc --no-deps --document-private-items` — documentation builds without warnings
- [ ] No new `unsafe` blocks without justification
- [ ] No `.unwrap()` or `.expect()` in library code (only in tests and `main.rs` where appropriate)

**Manual diff review — full `git diff main...HEAD` analysis:**

_Idiomatic Rust:_
- [ ] Error handling uses `thiserror` for error types, `?` for propagation, `anyhow` only in binary crate if needed
- [ ] CLI argument types leverage Rust type system: `PathBuf` for paths, enums for subcommands, `Option` for optional flags
- [ ] Builder pattern or method chaining for `clap` and `indicatif` configuration where it improves readability
- [ ] `#[must_use]` on functions returning important values
- [ ] Doc comments (`///`) on all public items; module-level `//!` on each new module

_Performance:_
- [ ] No unnecessary allocations in the report rendering hot path — format strings efficiently
- [ ] Progress bar updates not too frequent (throttled to avoid terminal flicker)
- [ ] Database queries for report data batched — not N+1 queries
- [ ] Bar chart rendering uses fixed-size buffer, not repeated string concatenation

_Memory management:_
- [ ] `ScanResult` and report data properly scoped — dropped after display
- [ ] No string formatting into intermediate `String`s when `write!` to stdout suffices
- [ ] Large convention lists: display top N, not unbounded

_Code duplication — CRITICAL:_
- [ ] Shared formatting extracted to `seshat-cli/src/format.rs`: section headers, bar charts, tier bullets, error/hint output, bordered boxes, verbosity filtering
- [ ] No copy-pasted formatting logic between US-002 (overview), US-003 (conventions), US-004 (verbose sections)
- [ ] Color application centralized — one place controls `owo-colors` usage + `NO_COLOR` check
- [ ] Progress bar creation centralized if used in multiple places
- [ ] Number formatting (thousands separator, human-readable file sizes) in shared utility

_Architecture compliance:_
- [ ] `clap` types in `seshat-cli` — not in `seshat-bin` (bin crate stays thin: parse args → delegate to cli crate)
- [ ] Report rendering in `seshat-cli/src/report/` — not mixed into scan logic
- [ ] Scan orchestration stays in `seshat-scanner` — CLI only calls `scan_project()` and renders result
- [ ] `seshat-cli` does NOT depend on `seshat-storage` directly — gets report data via `seshat-graph` queries or receives it from the scan result
- [ ] `#[tracing::instrument]` on the scan command handler
- [ ] Serde not needed for CLI output (direct `write!`/`println!` to stdout)

_Test quality:_
- [ ] Unit tests for formatting functions: `format_section_header()`, `format_bar_chart()`, `format_tier_bullet()`, `format_human_size()`
- [ ] Unit tests for verbosity filtering: quiet/default/verbose produce correct output
- [ ] Test `NO_COLOR` handling
- [ ] Integration test: run `seshat scan` on a temp project directory, verify exit code and key output patterns
- [ ] Test error cases: nonexistent path, empty directory, directory with no parseable files

## Functional Requirements

- FR-1: `clap` derive-based CLI with subcommands: `scan` (implemented), `serve`/`status`/`review`/`init` (stubbed)
- FR-2: `seshat scan <path>` validates input, runs full scan pipeline, displays report
- FR-3: Two-phase progress: discovery count (in-place update) → scanning progress bar with known total and elapsed time
- FR-4: Report "Project Overview": language horizontal bar charts, module count, dependency count with ecosystem breakdown
- FR-5: Report "Conventions Detected": confidence tier summary (●/◐/○), top findings with tier bullet + description + trend + percentage
- FR-6: Report "Submodules": listed if present, with language and file count
- FR-7: Report "Next Steps": copy-paste-ready commands
- FR-8: Summary line with convention count + database path with human-readable size
- FR-9: Verbosity: `--quiet` (summary only), default (summary + findings), `--verbose` (+ skipped files + detector details + timing)
- FR-10: Error output: `error: {message}` + `hint:` lines, red-colored when terminal supports it
- FR-11: `NO_COLOR` environment variable disables all colored output
- FR-12: Section headers with box-drawing characters, consistent ~60 char width
- FR-13: Shared formatting utilities in `seshat-cli/src/format.rs` for reuse across all future commands
- FR-14: `seshat --version` prints `seshat {version} ({git_hash})`
- FR-15: Database created in XDG data directory
- FR-16: `AppConfig` loaded from `seshat.toml` or defaults

## Non-Goals

- No `seshat serve` implementation — that's Epic 5
- No `seshat review` TUI — that's Epic 11
- No `seshat status` implementation — that's Epic 8
- No `seshat init` implementation — that's Epic 8
- No MCP server code — this is purely CLI output
- No incremental scan UX (file watcher progress) — that's Epic 9
- No branch-aware display — that's Epic 10
- No interactive prompts during scan — scan is a batch operation
- No JSON output mode — MCP tools handle structured output (Epic 5)

## Developer Context

This is a **CLI/UX story**, not a core logic story. The scan engine is done. The developer's job is:

1. Add dependencies (`clap`, `indicatif`, `owo-colors`, `dirs`) and wire them
2. Connect `clap` subcommand dispatch to `scan_project()`
3. Render scan results as a beautiful terminal report
4. Establish formatting patterns that will be reused in all future commands

The output should feel like running `cargo build` or `git status` — clean, informative, zero-fluff.

**Factual, not subjective** — no "Great project!" or emoji. Dry facts. Let the data speak.

## Technical Considerations

- **clap v4** with derive macros — `#[derive(Parser)]` for the main struct, `#[derive(Subcommand)]` for commands
- **indicatif** for progress bars — `ProgressBar::new(total)` with `ProgressStyle::with_template()`
- **owo-colors** for colored output — lightweight, supports `NO_COLOR` via `if_supports_color()` or manual check
- **dirs** crate for XDG paths — `dirs::data_dir()` → `~/.local/share/seshat/repos/`
- **`ScanResult`** already returns `files_discovered`, `files_parsed`, `nodes_persisted`, etc. — sufficient for basic report
- **Convention data** requires querying the database after scan: `KnowledgeNode` where `nature=Convention/Observation`, with confidence, adoption, ext_data (trend). Use `seshat-graph` or `seshat-storage` query functions.
- **Progress bar during scan**: Currently `scan_project()` runs as a black box. To show per-file progress, either: (a) pass a callback/channel into the scan function, or (b) refactor scan to yield progress events. Option (a) is simpler — pass `impl Fn(ScanProgress)` callback.
- **`GIT_HASH` at build time**: `seshat-bin/build.rs` should capture git short hash via `git rev-parse --short HEAD` and expose as `env!("GIT_HASH")` — this may already exist.
- **Terminal width**: Use `terminal_size` crate or fixed 60-char width for section headers. Fixed width is simpler and more predictable.

## Success Metrics

- `seshat scan .` on the Seshat project itself produces a complete, correctly-formatted report
- Language bars, convention counts, and trends match reality
- Progress bar shows accurate progress during scanning
- `--verbose` adds skipped files, detector details, and timing sections
- `--quiet` shows only summary
- `NO_COLOR=1 seshat scan .` produces clean uncolored output
- Error on invalid path shows red error + helpful hint
- All formatting utilities are shared — zero duplicated rendering logic
- `cargo test --workspace` passes
- `cargo clippy --all-targets --all-features -- -D warnings` passes
- `cargo fmt --all --check` passes
- Full diff review against `main` passes the quality gate defined in US-005

## Open Questions

- **Progress callback into scan_project()**: The current `scan_project()` is a monolithic function. To report per-file progress, we need a callback mechanism. Options: (a) `Fn(ScanProgress)` parameter, (b) `tokio::sync::mpsc` channel, (c) `Arc<AtomicUsize>` shared counter. Callback (a) is simplest and avoids async. Implementer should choose and document.
- **Convention display limit**: Should the report show all conventions or top N? UX spec says "Top findings" implying a subset. Suggest: default shows top 10, `--verbose` shows all.
- **Database query for report data**: Should the CLI query the DB after `scan_project()` completes, or should `scan_project()` return richer data? Currently `ScanResult` has counts but no convention details. Options: (a) enrich `ScanResult`, (b) CLI queries DB directly. Option (b) keeps scan engine clean.
