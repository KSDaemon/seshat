# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_No changes yet._

## [0.3.1] - 2026-05-17

### Notes

- **No user-facing changes.** Release-plz re-cycled the workspace and re-tagged
  every crate at `v0.3.1`. The binary is byte-equivalent in scope to `v0.3.0`;
  only the per-crate `Cargo.toml` versions and per-crate `CHANGELOG.md` files
  were updated. No code, schema, or behaviour shifted between `v0.3.0` and
  `v0.3.1`.

## [0.3.0] - 2026-05-17

### Added

- **`query_code_pattern` symbol-index enrichment** (PR #22). Every symbol
  match now carries:
  - `dependent_files` — list of project files that import the symbol's
    defining name (`use`/`import`/`from … import …`).
  - `blast_radius` — `low` / `medium` / `high`, using the same thresholds as
    `query_dependencies` (low < 5, medium 5–20, high > 20).
  - `call_sites` aggregated by file: `{file, site_count, lines, first_snippet}`
    instead of the previous one-row-per-occurrence flat list.

  Lookups are served from a new pre-computed index (`symbol_definitions` and
  `symbol_imports` tables, migration `V13`) populated at scan time and kept
  in sync by the watcher's hot tier. Old DBs are retrofitted in-place by the
  migration's backfill step — no full rescan required. Wildcard imports
  (`use foo::*`, `from foo import *`, `import * as foo from '…'`) contribute
  no rows; aliased imports (`use foo::Bar as Baz`) are stored under the
  defining name so dependents stay reachable from a `Bar` query.
- **Per-language definition snippets** in `query_code_pattern` responses.
  Snippets are rendered with language-aware truncation rules instead of a
  shared default, so Rust attribute-blocks, TypeScript decorators, and
  Python decorators all survive the snippet window.
- **`seshat serve` separates `project_root` from `sync_root`** during
  incremental sync. The sync root walks the actual git tree (so symlinked
  worktrees no longer silently re-sync the wrong directory) while
  `project_root` continues to anchor the scope and DB selection.
- Top-level **README rewrite**: install path, quick-start, full feature
  matrix. The previous CLI reference moved out to `docs/cli.md` so the
  README stays scannable. New supported-platforms and contributing sections.

### Fixed

- **Detectors — cross-file enrichment + bucket consolidation** (May 16–17
  detector polish sprint):
  - Cross-file findings now include the real source snippet around the
    triggering site instead of an empty `evidence.snippet`.
  - Per-file naming-percentage buckets collapse into one language-wide
    bucket; a Rust project no longer surfaces `38% snake_case (in handlers.rs)`
    alongside `42% snake_case (in models.rs)` — both fold into one
    language-level percentage.
  - Non-conforming file-naming variants fold under the dominant convention
    instead of advertising themselves as a separate trend.
  - The wrapper-facade convention and its violators share a single bucket
    so the violator count is comparable to the adoption count.
  - Composite header verbs (`adopted_by_majority`, `mixed`, etc.) reflect
    the bucket's adoption ratio rather than the bucket's first sample.
- **`validate_approach` payload + focus_area semantics hardened.** Payload
  bounded to prevent oversized responses from FTS5 + dependents JOIN paths;
  `focus_area` matching now uses a deterministic ranking instead of relying
  on FTS5 score ties.
- **Windows CI**: tests use a per-test tempdir for `XDG_CONFIG_HOME` so the
  Windows runner stops clobbering its own state across tests.
- **Docs**: dropped intra-doc links to private items so `cargo doc` is
  warning-clean again.

## [0.2.1] - 2026-05-11

### Fixed

- **Release pipeline — darwin and aarch64-linux binary builds.** The
  `v0.2.0` workspace release failed to upload macOS and `aarch64-unknown-linux-gnu`
  artifacts because of a broken cross-toolchain matrix entry. Patched in
  `release.yml` and re-tagged as `v0.2.1` with the missing binaries.

## [0.2.0] - 2026-05-11

### Breaking

- **DB schema redesigned (Merge-aware Decisions). Existing DBs are
  incompatible — delete `~/.local/share/seshat/repos/<project>.db` and
  rescan.** Migrations V11 (new `branches` table) and V12 (new
  `decisions` table) replace the previous "decisions stored as `nodes`
  rows with `ext_data.source = 'user'`" contract. No data migration is
  performed; the wipe-and-rescan path is the only supported upgrade.
  Rationale and trade-offs are documented in
  [ADR 14.1](_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md).
- **MCP `record_decision` / `update_decision` / `remove_decision`
  identifier changed** from a numeric rowid to the
  `description_hash` (16-character hex string). Scripts that captured
  the old `id` from `record_decision` and threaded it back into
  `update_decision` / `remove_decision` must switch to passing the
  hash. The `query_*` envelope shape is unchanged.
- **IR schema bumped to v8.** `Export` and `TypeDef` IR structs gained
  `end_line` so hunk-aware diff impact can resolve symbol ranges. Old
  IR blobs are migrated transparently on first re-parse.

### Added

- **Project-wide decisions table.** User-recorded decisions (TUI
  confirm/reject/partial AND MCP `record_decision`/`update_decision`)
  are stored once per `description_hash`, project-wide. Approving a
  convention on `feature` and merging into `main` no longer re-surfaces
  it in `seshat review` on `main`. Decisions also survive branch
  deletion.
- **Same-branch HEAD-change detection in `seshat serve`.** On startup,
  `seshat serve` compares `branches.last_scanned_commit` against
  `git rev-parse HEAD`. If different (e.g. after a `git pull`), it
  spawns `background_sync` automatically. Logs include
  `old_head=<7-char>, new_head=<7-char>` so the trigger is visible.
- **Blocking incremental sync in `seshat review`.** Stale DBs are
  brought up-to-date BEFORE the TUI opens. Progress is rendered as
  `Files: X / Y` at ≥ 1 Hz on TTY (single-line on piped output).
- **`seshat decisions` CLI subcommand group:**
  - `seshat decisions list [--state STATE] [--branch BRANCH] [--format table|json]`
  - `seshat decisions forget <HASH> [--yes]` (full hash or ≥ 4-char
    unambiguous prefix)
  - `seshat decisions export <FILE>` (writes JSON array)
  - `seshat decisions import <FILE> [--strict]` (UPSERTs; conflicts
    resolved by latest `decided_at`, or `--strict` aborts before any
    write)
- **Git-optional fallback documented and locked behind tests.** When
  `.git` is absent or `git rev-parse HEAD` fails, `detect_branch`
  returns `"main"`, all freshness comparisons skip silently with a
  `debug!` log, and `last_scanned_commit` stays `NULL`. No warnings or
  errors reach stdout/stderr.
- **Transitive dependents in `query_dependencies`.** New `depth`
  parameter (default 1, max 3) drives a cycle-safe BFS over reverse
  imports. `dependents[]` entries gain `transitive_dependent_count`
  and `via` (the shortest import chain). Performance guard test
  prevents regression on large graphs. Call-logger summary surfaces
  transitive count and traversal depth.
- **Content-level granularity in `map_diff_impact`.** `affected_symbols`
  is now computed at hunk-level: only symbols whose line range
  intersects a changed hunk are flagged. Hunk extraction is powered
  by `gix::diff::blob`. Blob-aware change enumeration handles
  added/modified/renamed/deleted files uniformly. Criterion benchmark
  locked the perf budget.
- **Windows self-update parity.** `seshat update` and
  `seshat update --check` now work on Windows:
  - `.zip` archive extraction in `extract_binary` with the same
    path-traversal, symlink-skip, decompressed-size, and case-insensitive
    extension safeguards as the existing `.tar.gz` path.
  - `self_replace`-based atomic binary replacement (handles cross-FS
    moves internally; no bespoke EXDEV fallback).
  - Windows target detection in `current_target`, `find_binary_asset`,
    and `fetch_checksum_for_asset` (`x86_64-pc-windows-msvc`).
  - `.exe.old` cleanup at next startup (best-effort, suppressed for
    `update` and `completions` subcommands).
  - `windows-latest` in the CI test matrix.
  - Permission-denied errors map to "Try running as Administrator."
- **Release archive layout fix (FR-23).** `ARCHIVE_NAME` now embeds the
  version in the directory name
  (`seshat-${TARGET_TRIPLE}-${GITHUB_REF_NAME}`), matching what
  `extract_binary` expects, so self-update works against real release
  tags on every platform.
- **`seshat completions` subcommand** (`bash` / `zsh` / `fish` /
  `powershell` / `elvish`). Without an explicit shell argument the
  target is auto-detected from `$SHELL`; on Windows the fallback is
  PowerShell. `seshat completions` skips the background update notice
  so `eval $(seshat completions ...)` pipes stay clean.
- **Homebrew tap pipeline.** `homebrew/seshat.rb` formula template +
  `.github/workflows/homebrew-bump.yml` that fires on `release: published`,
  downloads the Unix tarballs, computes SHA256s, renders the formula,
  and pushes to `KSDaemon/homebrew-seshat`. Workflow self-skips when
  `HOMEBREW_TAP_TOKEN` is missing, so the rest of the release pipeline
  is unaffected. Bootstrap instructions in `homebrew/README.md`.
- **Top-level `README.md`** with the install path, quick-start, and
  full `seshat decisions` reference.
- **Smoke-test plan** at
  `docs/smoke-tests/merge-aware-decisions.md` mapping every story
  US-001 .. US-016 to manual verification steps.
- **ADR 14.1** at
  `_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md`
  documenting the decision-table-vs-user-node trade-off, the
  no-migration choice, the git-optional fallback semantics, the
  worktree concurrency limitation, and deferred future extensions.

### Fixed

- **`create_snapshot` no longer drops decision metadata.** Decisions
  are project-wide and not copied per branch, so the column-strip bug
  cannot recur for decision rows.
- **Pre-V8 user nodes' missing `description_hash` is no longer a dedup
  hazard.** Decisions live in their own table, fresh-populated, and no
  longer go through the legacy `nodes`/`ext_data` path.
- **`Hunk::ALL` fallback for Conflicted files** (FR-B6). When `gix`
  reports a Conflicted change, `map_diff_impact` now treats the whole
  file as changed instead of producing an empty hunk list.
- **`enumerate_changes_with_blobs` `base = Some(commit)` branch** now
  has explicit test coverage (`base=Some` was previously only
  exercised end-to-end).
- **Dependency-tree diamond `via` tie-break** uses lexicographic ordering
  on the joined chain string so identical-length paths produce stable
  output across runs.
- **`read_disk_file_bytes` distinguishes `NotFound` from
  `PermissionDenied`** and surfaces the right error variant.
- **`format_changed_lines` collapses single-line ranges**
  (`L42-L42` → `L42`).
- **`query_dependencies` truncation flag** propagated through
  `DependencyData.truncated` so callers can tell when the BFS hit
  `MAX_DEPENDENTS`.
- **`#[serde(default)]` dropped on fields newly added in this branch**
  to avoid silently masking IR-schema mismatches.

### Dependencies

- `gix` 0.72 → 0.83
- `fastembed` 4 → 5
- `criterion` 0.5 → 0.8
- `zip` 7 → 8
- `sha1` 0.10 → 0.11
- `rusqlite` 0.37 → 0.38
- `toml` 0.8 → 1
- `which` 7 → 8
- `indicatif` 0.17 → 0.18
- `serde_yml` 0.0.11 → 0.0.12
- Plus semver-compatible lockfile bumps.

### Known Limitations

- **Concurrent `seshat serve` instances on different worktrees of the
  same main repo race on the global `metadata.current_branch` value.**
  Decisions and per-branch scans are unaffected (decisions are
  project-wide; nodes are scoped by `branch_id`); only the
  "current branch" pointer flickers. Documented in ADR 14.1 §D4.
- **Detached-HEAD checkouts each become a unique `branch_id`.** Branch
  GC remains the cleanup mechanism (Story 11.2).

## [0.1.1] - 2026-05-06

### Fixed

- **Closed a 90+ GB memory leak class in `seshat serve` auto-scan / watcher
  startup** (peak observed: 91.8 GB; reproducible: 11.4 GB in 3.5 min). The
  root cause was `notify-debouncer-full` recursively walking the project root
  on init, which previously could happen even when:
  1. The auto-scan failed (e.g. project too large for `auto_scan_limit`), or
  2. `seshat serve` was invoked from a denylisted directory like `$HOME`,
     `~/Library`, drive roots, `/tmp`, `/var`, `/etc`, or any other location
     not inside a git repository.
- **`seshat serve` now refuses to auto-scan from a dangerous cwd** when not
  inside a git repository. Refusal is fast (no DB created, no directory walk)
  and prints a multi-line stderr message with three concrete suggestions:
  `cd` into a real project, run `seshat scan <path>`, or pass `<repo>` as
  an explicit positional argument.
  - The dangerous-cwd denylist is per-OS:
    - **macOS**: `$HOME` plus `~/Library`, `~/Documents`, `~/Downloads`,
      `~/Desktop`, `~/Pictures`, `~/Movies`, `~/Music`, `~/Public`,
      `~/.config`, `~/.cache`, and absolute roots `/`, `/Users`,
      `/Applications`, `/System`, `/Library`, `/private`, `/tmp`, `/var`,
      `/usr`, `/etc`, `/opt`.
    - **Linux**: `$HOME`, `/`, `/home`, `/etc`, `/var`, `/tmp`, `/usr`,
      `/opt`, `/root`, `/proc`, `/sys`, `/dev`, plus `$XDG_CONFIG_HOME`
      (fallback `~/.config`), `$XDG_CACHE_HOME` (fallback `~/.cache`),
      `$XDG_DATA_HOME` (fallback `~/.local/share`).
    - **Windows**: all 26 drive roots (`A:\` … `Z:\`), `%USERPROFILE%`
      (+ `Documents`, `Downloads`, `Desktop`), `%APPDATA%`,
      `%LOCALAPPDATA%`, `%TEMP%`, `C:\Windows`, `C:\Program Files`,
      `C:\Program Files (x86)`, `C:\ProgramData`.
  - Comparison is case-insensitive on macOS/Windows, byte-exact on Linux,
    and uses component-wise `Path::starts_with` (so `/var2` does not match
    `/var`). Symlinks are resolved via `std::fs::canonicalize`.
- **`seshat serve <repo>` opt-out** continues to work even from a dangerous
  location, but now emits a multi-line `⚠️  Serving from a dangerous
  location` warning on stderr when the resolved repo isn't itself a git
  repository.
- **The file watcher no longer starts when auto-scan failed**, regardless of
  `[watcher] enabled`. The startup banner now reads
  `Watcher: disabled (auto-scan failed: <reason>)` so the failure mode is
  visible. A race-guard re-check inside the spawned watcher task closes the
  window where scan failure happens after the outer gate observed an
  in-progress scan.

### Added

- **`scan.additional_denylist_paths` config field** (`Vec<String>`) in
  `seshat.toml`. Each entry is matched as `cwd == path` OR `cwd is descendant
  of path`. Absolute paths only — tilde and environment variables are NOT
  expanded. Relative entries are skipped at runtime with a warn-level log.
  Example:

  ```toml
  [scan]
  additional_denylist_paths = ["/mnt/nfs", "/Volumes/BackupDrive"]
  ```

- **New `WatcherError::ScanFailed(String)`** variant carrying the underlying
  scan-failure reason, delivered through the existing watcher startup
  `oneshot` channel.
- **New `CliError::DangerousCwd { path, hint }`** variant with multi-line
  Display rendering the offending path, denylist explanation, and three
  actionable suggestions.
- **Subprocess integration tests** in `crates/seshat-bin/tests/serve_guardrails.rs`
  covering the four end-to-end behaviours: refusal from dangerous cwd, normal
  startup inside a git repo, override-warn on explicit dangerous repo, and
  the P0 path where `auto_scan_limit = 1` forces a scan failure and the
  watcher must remain disabled (asserted via stderr banner + RSS bound).

[Unreleased]: https://github.com/KSDaemon/seshat/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/KSDaemon/seshat/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/KSDaemon/seshat/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/KSDaemon/seshat/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/KSDaemon/seshat/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/KSDaemon/seshat/releases/tag/v0.1.1
