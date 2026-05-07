# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — Merge-aware Decisions and DB Freshness

### Breaking

- **DB schema redesigned. Existing DBs are incompatible — delete
  `~/.local/share/seshat/repos/<project>.db` and rescan.** Migrations
  V11 (new `branches` table) and V12 (new `decisions` table) replace
  the previous "decisions stored as `nodes` rows with
  `ext_data.source = 'user'`" contract. No data migration is performed;
  the wipe-and-rescan path is the only supported upgrade. Rationale and
  trade-offs are documented in
  [ADR 14.1](_bmad-output/planning-artifacts/14-1-merge-aware-decisions.md).
- **MCP `record_decision` / `update_decision` / `remove_decision`
  identifier changed** from a numeric rowid to the
  `description_hash` (16-character hex string). Scripts that captured
  the old `id` from `record_decision` and threaded it back into
  `update_decision` / `remove_decision` must switch to passing the
  hash. The `query_*` envelope shape is unchanged.

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

[0.1.1]: https://github.com/KSDaemon/seshat/releases/tag/v0.1.1
