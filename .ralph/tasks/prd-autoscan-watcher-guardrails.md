# PRD: Auto-scan & Watcher Guardrails for `seshat serve`

## Introduction

**Type:** Fix

`seshat serve` can leak unbounded memory (measured 11.4 GB in 3 min 24 sec, on track for 90+ GB) when started in a directory containing many files (e.g. `$HOME`). Two scoped fixes ship together as a single PR:

- **P0** — When auto-scan is aborted or fails, the file watcher must not start. Currently it starts anyway and triggers `notify-debouncer-full`'s recursive walk + in-memory `FileIdMap` build, which is the root of the leak.
- **P1** — Refuse to enter the AutoScan branch entirely when cwd is a known-dangerous location (e.g. `$HOME`, `~/Library`, drive roots) and not inside a git repository.

P2 work (replacing `notify-debouncer-full`, skipping its `FileIdMap`, dynamic activation by tree size) is explicitly out of scope.

## Evidence

**Direct CPU profile (PID 39524, captured 2026-05-06):**

```
seshat_cli::serve::run_serve
  → tokio::task::spawn_blocking
    → notify_debouncer_full::Debouncer::add_root
      → FileIdMap::add_path
        → walkdir::IntoIter::next
          → std::sys::fs::read_dir → __opendir2 / open
```

Process spawned by interactive zsh from `$HOME`. Memory growth: 0 → 5 GB in 90 s, 5 → 11.4 GB in next 110 s. Single CPU-bound thread (~99% CPU). MALLOC_SMALL: 758 regions (vs 21–37 in healthy processes). Watcher init never finished — leak is in `Debouncer::add_root` which synchronously walks the watched tree to build an inode→path cache.

**Earlier observed crash (PID 93112):** spawned by Claude Desktop's `local-agent-mode-sessions` (computer-use) with cwd=`$HOME`, lived 13 hours, accumulated **91.8 GB physical footprint**, 15 639 MALLOC_SMALL regions before being killed.

**Why the existing `auto_scan_limit` doesn't help:** `crates/seshat-core/src/config.rs:71` defines `auto_scan_limit = 50_000`. This guard correctly aborts the scan (calls `scan_state.mark_failed`), but `crates/seshat-cli/src/serve.rs:752` (`if watcher_enabled`) doesn't check `scan_state` — the watcher launches anyway with `project_root = cwd = $HOME`.

Snapshots saved at `~/.seshat/monitor/leak-snapshot-39524/`:
- `sample-5sec.txt` — CPU profile pinpointing `Debouncer::add_root`
- `vmmap-summary.txt`, `vmmap-summary-final.txt` — before/after memory regions
- `vmmap-full.txt` — full memory map (443 KB)
- `lsof.txt`, `threads.txt` — fd / thread state

## Goals

- **G1** — `seshat serve` invoked from a dangerous cwd (e.g. `$HOME`) exits cleanly within 1 second with a clear error message; no DB created, no scanning, no watcher.
- **G2** — When auto-scan is aborted (large-project guard) or fails, the file watcher does not start. Memory stays bounded.
- **G3** — Existing happy-path scenarios (real git repo, with or without existing DB; worktrees) keep working unchanged.
- **G4** — Power-user opt-out exists — explicit `seshat serve --repo <path>` warns to stderr but proceeds.

## User Stories

### US-001: `is_dangerous_cwd()` helper with per-OS denylist

**Description:** As a backend module, I need a single function that returns `true` if a given path is at or under a known-dangerous location, so that `serve` can refuse to scan/watch there.

**Acceptance Criteria:**
- [ ] New module `crates/seshat-cli/src/dangerous_path.rs` with public `pub fn is_dangerous_cwd(path: &Path, additional: &[String]) -> bool`.
- [ ] Per-OS denylist via `cfg(target_os = "...")`:
  - **macOS:** `$HOME` itself, `~/Library`, `~/Documents`, `~/Downloads`, `~/Desktop`, `~/Pictures`, `~/Movies`, `~/Music`, `~/Public`, `~/.config`, `~/.cache`, `/`, `/Users`, `/Applications`, `/System`, `/Library`, `/private`, `/tmp`, `/var`, `/usr`, `/etc`, `/opt`
  - **Linux:** `$HOME`, `/`, `/home`, `/etc`, `/var`, `/tmp`, `/usr`, `/opt`, `/root`, `/proc`, `/sys`, `/dev`, `$XDG_CONFIG_HOME` (or `~/.config`), `$XDG_CACHE_HOME` (or `~/.cache`), `$XDG_DATA_HOME` (or `~/.local/share`)
  - **Windows:** drive roots (any path matching `^[A-Za-z]:\\?$`), `%USERPROFILE%`, `%USERPROFILE%\Documents`, `%USERPROFILE%\Downloads`, `%USERPROFILE%\Desktop`, `%APPDATA%`, `%LOCALAPPDATA%`, `%TEMP%`, `C:\Windows`, `C:\Program Files`, `C:\Program Files (x86)`, `C:\ProgramData`
- [ ] Path matching logic: `cwd` is dangerous if **`cwd == denylist_entry` OR `cwd` is a descendant of `denylist_entry`** (tested via `Path::starts_with`, which does component-wise prefix matching). Subdirs of denylisted parents are also dangerous (per user decision 4A: "parent wins").
- [ ] Paths are canonicalized via `std::fs::canonicalize` before comparison; symlinks resolved. macOS and Windows: case-insensitive comparison (lowercase both sides via `to_string_lossy().to_lowercase()` before `starts_with`). Linux: byte-exact.
- [ ] `$HOME` / `%USERPROFILE%` resolved via `dirs::home_dir()` (add `dirs` to `seshat-cli` deps if not present). XDG dirs resolved via `std::env::var` with documented fallbacks (`~/.config`, `~/.cache`, `~/.local/share`).
- [ ] Denylist entries that don't exist on the current machine (e.g. `/Users` on Linux) are silently skipped — `is_dangerous_cwd` operates only on entries that resolve.
- [ ] Unit tests cover: exact match, subdir match, sibling not matched (`/var/foo` does not match `/var2`), symlink-to-dangerous resolves to dangerous, case difference on macOS/Windows, missing env-var graceful fallback, malformed `additional` entry (relative path) is skipped with a warn log.
- [ ] Typecheck and `cargo clippy --all-targets -- -D warnings` pass.

### US-002: Configurable `additional_denylist_paths` in `ScanConfig`

**Description:** As a user with non-standard setups, I want to add custom dangerous paths via `seshat.toml` so that the guardrail is extensible without code changes.

**Acceptance Criteria:**
- [ ] `crates/seshat-core/src/config.rs::ScanConfig` gains:
  ```rust
  /// Additional paths treated as dangerous (block auto-scan from cwd).
  /// Each entry is matched as: cwd == path OR cwd is a descendant of path.
  /// Use absolute paths — tilde (~) and env vars are NOT expanded.
  #[serde(default)]
  pub additional_denylist_paths: Vec<String>,
  ```
- [ ] `Default` impl returns empty `Vec`, fully backward-compatible with old TOML files.
- [ ] `seshat.example.toml` documents the field with a commented example:
  ```toml
  # [scan]
  # additional_denylist_paths = ["/mnt/nfs", "/Volumes/BackupDrive"]
  ```
- [ ] Existing `ScanConfig` round-trip serialization tests still pass; new test verifies field deserializes from TOML and missing-field defaults to empty.
- [ ] Typecheck and clippy pass.

### US-003: P1 — Block AutoScan when cwd is dangerous and not in git repo

**Description:** As `serve`, I must refuse to start auto-scan when invoked from a dangerous directory without a git repository, but proceed (with stderr warning) when the user explicitly passed `--repo`.

**Acceptance Criteria:**
- [ ] In `crates/seshat-cli/src/db.rs::resolve_serve_db_or_project_root`, before existing branch logic:
  - If `explicit_repo.is_none()` (user did NOT pass `--repo`): check `is_dangerous_cwd(&cwd, &scan_config.additional_denylist_paths) && find_git_root(&cwd).is_none()`. If true, return `Err(CliError::DangerousCwd { path, hint })`.
  - If `explicit_repo.is_some()`: run the same check on the resolved `project_root`. If true, log a warn line to stderr (see message below) and continue with normal AutoScan / ExistingDb resolution.
- [ ] New `CliError::DangerousCwd { path: PathBuf, hint: String }` variant in the existing `CliError` location (verify during implementation; likely `crates/seshat-cli/src/error.rs` or `lib.rs`). Display impl produces a multi-line, user-friendly message:
  ```
  Refusing to auto-scan from a dangerous location: /Users/kostik

  This directory is on the built-in denylist (system or user-home root).
  Recursive watching here would consume tens of gigabytes of memory.

  Try one of:
    • cd into a real project directory (with a .git/) and rerun: seshat serve
    • Run seshat scan /path/to/project explicitly to build a DB once.
    • Pass --repo /path/to/project to force-serve a specific location.
  ```
- [ ] Stderr warn line for the `--repo` override case:
  ```
  ⚠️  Serving from a dangerous location: /Users/kostik
     This may consume excessive memory if the directory contains many files.
     Proceeding because --repo was passed explicitly.
  ```
- [ ] Exit code on refusal is non-zero (use existing `CliError`-to-exit-code convention; e.g. exit 2). No DB file is created. No directory walking happens.
- [ ] Unit tests in `db.rs::tests` cover:
  - cwd inside a temp `$HOME`-like dir with no `.git` → returns `DangerousCwd`
  - cwd inside a real git repo (temp dir with `.git/` subdir) → returns `AutoScan` or `ExistingDb` as today
  - cwd inside a temp `$HOME`-like dir + `explicit_repo = Some(safe_path)` → proceeds (no error, no warn)
  - cwd anywhere + `explicit_repo = Some(dangerous_path)` → proceeds with stderr warn captured
- [ ] Tests use `tempfile::TempDir` and inject the home-dir override via a test-only helper (e.g. `is_dangerous_cwd_for_test(path, home_override, additional)`); no dependency on the real `$HOME`.
- [ ] Typecheck and clippy pass.

### US-004: P0 — Watcher does not start when auto-scan failed

**Description:** As `serve`, when `scan_state` reports a failure (e.g. project too large), I must not spawn the watcher tokio task, regardless of `config.watcher.enabled`.

**Acceptance Criteria:**
- [ ] In `crates/seshat-cli/src/serve.rs` around line 752, change the gating condition from:
  ```rust
  let watcher_rx = if watcher_enabled {
  ```
  to:
  ```rust
  let watcher_should_start = watcher_enabled && scan_state.error_message().is_none();
  let watcher_rx = if watcher_should_start {
  ```
- [ ] Inside the spawned watcher task, after `wait_scan.wait_for_scan()`, re-check `wait_scan.error_message()` (race guard: scan may have failed during the wait). If `Some`, log `info!` and bail out without calling `start_watcher`. Send `Err(WatcherError::ScanFailed(msg))` (new variant) on `watcher_tx` so downstream startup logic remains consistent.
- [ ] Add `WatcherError::ScanFailed(String)` variant to `crates/seshat-watcher/src/error.rs`; ensure `Display`/`Debug` are implemented.
- [ ] Confirm the existing startup-banner branch at `serve.rs:921-922` (`watcher_status = "disabled (auto-scan failed)"`) still triggers; if the new gating means `has_auto_scan && scan_state.error_message().is_some()` is the right condition, no change needed. Optionally enrich the banner with the failure reason: `disabled (auto-scan failed: too many files)` (see OQ-5).
- [ ] Add a unit/integration test exercising the gate: build a `ScanState` via `ScanState::in_progress()` then `mark_failed("test")`, run the relevant code path (extract a small helper if needed for testability), assert no `WatcherHandle` is constructed. May require a tiny refactor — pull the gating expression into a free function `fn watcher_should_start(enabled: bool, state: &ScanState) -> bool`.
- [ ] Manual verification: temporarily disable P1 (or use `--repo` override), invoke `cd ~ && seshat serve --repo ~` with monitor running. RSS must stay < 200 MB over 5 minutes idle. Compare against baseline `leak-snapshot-39524/` snapshots.
- [ ] Typecheck and clippy pass.

### US-005: Integration tests for end-to-end behaviour

**Description:** As a maintainer, I want subprocess-level tests that cover the user-visible behaviour of `seshat serve` from various directories so that regressions are caught in CI.

**Acceptance Criteria:**
- [ ] New file `crates/seshat-cli/tests/serve_guardrails.rs`.
- [ ] Test 1: `seshat serve` invoked with cwd=`temp_dir_under_$HOME` (no git) → exits non-zero in <1 s, stderr contains `Refusing to auto-scan`.
- [ ] Test 2: `seshat serve` invoked with cwd=`temp_dir/git-init/` (real git repo, created via `Command::new("git").args(["init"])`) → starts (verify by polling stderr for "MCP server" startup line, then send SIGTERM and assert clean exit).
- [ ] Test 3: `seshat serve --repo /tmp/empty-no-git` → starts with stderr containing `⚠️  Serving from a dangerous location` warn line.
- [ ] Test 4: P0 path — start `seshat serve` in a small real project but with `auto_scan_limit = 1` in temp `seshat.toml` to force scan failure; assert stderr banner shows `watcher: disabled (auto-scan failed)` and process memory (via `ps`) stays under 200 MB after 30 s.
- [ ] Tests use `assert_cmd::Command` (add as dev-dep if missing) and `tempfile::TempDir`. Invoke the binary via `target/debug/seshat` (or `env!("CARGO_BIN_EXE_seshat")`).
- [ ] Each test runs in <30 s.
- [ ] CI runs them on macOS and Linux. Windows: include but allow `#[cfg(not(target_os = "windows"))]` on tests that need POSIX-specific paths if needed.

### US-006: Documentation, CHANGELOG, monitor-experiment rerun

**Description:** As a downstream user, I need clear release notes and updated docs so I understand the new behaviour.

**Acceptance Criteria:**
- [ ] `CHANGELOG.md` (create if not present, otherwise append) gets an entry under next-version `### Fixed`:
  ```
  - serve: refuse to auto-scan from dangerous cwd (e.g. $HOME, ~/Library, drive roots).
    Pass --repo <path> to override with a stderr warning. Eliminates a memory leak
    that could grow to 90+ GB in long-running sessions (notify-debouncer-full was
    building an inode→path cache for the entire watched tree).
  - serve: watcher no longer starts when auto-scan was aborted or failed
    (e.g. project exceeded auto_scan_limit).
  ```
- [ ] `seshat.example.toml` documents `additional_denylist_paths` with one commented example entry.
- [ ] `README.md` — section about `seshat serve` (if it exists) updated to mention the dangerous-cwd guard and the `--repo` opt-out.
- [ ] Bump patch version in workspace `Cargo.toml` (e.g. 0.1.x → 0.1.(x+1)).
- [ ] Post-merge manual verification: re-run `tools/monitor-seshat.sh`; attempt `cd ~ && seshat serve` — process must refuse to start (exit <1 s, no entry in monitor log). Then attempt `cd ~ && seshat serve --repo ~` — process must start, watcher must be active, and after 5 min monitor log must show stable footprint < 200 MB (because P0 isn't relevant here — DB exists or we entered ExistingDb path; for the leak-replay scenario specifically, use `--repo /Users/kostik` from a separate cwd to recreate the AutoScan-on-$HOME path with override active, and verify scan aborts at `auto_scan_limit` and watcher does NOT start).

## Functional Requirements

- **FR-1** — `is_dangerous_cwd(path, additional)` returns `true` if `path` (canonicalized) equals or is a descendant of any built-in per-OS denylist entry, or any entry in `additional`.
- **FR-2** — When `seshat serve` is invoked with no `--repo` argument, it calls `is_dangerous_cwd(cwd, scan_config.additional_denylist_paths)` AND `find_git_root(cwd).is_none()`. If both true → exit with `CliError::DangerousCwd` (non-zero code, no DB created, no walking).
- **FR-3** — When `seshat serve --repo <path>` is invoked, the dangerous-cwd check still runs on the resolved project root, but a positive result emits a stderr warning (multi-line) instead of exiting.
- **FR-4** — When `serve` reaches the watcher-launch decision and `scan_state.error_message().is_some()`, no watcher tokio task is spawned and no `notify-debouncer-full::Debouncer` is constructed.
- **FR-5** — When `scan_state` transitions from `InProgress` to `Failed` *during* `wait_for_scan` (race), the watcher task that was already spawned must re-check and bail out before calling `start_watcher`, sending `Err(WatcherError::ScanFailed)` on `watcher_tx`.
- **FR-6** — The startup banner shows `watcher: disabled (auto-scan failed)` (or richer variant per OQ-5) when watcher is gated off due to scan failure.
- **FR-7** — `ScanConfig::additional_denylist_paths` defaults to empty `Vec`; missing field in TOML works without error.
- **FR-8** — All new error / warn messages are multi-line, suggest concrete next actions, and include the offending path.
- **FR-9** — Path canonicalization is case-insensitive on macOS/Windows, byte-exact on Linux. Symlinks are followed.

## Non-Goals (Out of Scope)

- **NG-1** — Reconfiguring or replacing `notify-debouncer-full` to avoid building `FileIdMap`. (P2)
- **NG-2** — Switching to raw `notify` crate without debouncer. (P2)
- **NG-3** — Per-file-count or per-tree-size cap that activates / deactivates the watcher dynamically. (P2)
- **NG-4** — Worktree-aware project detection beyond what `find_git_root` already supports — `.git` as a file with `gitdir:` reference is already handled today.
- **NG-5** — `denylist_overrides = []` to remove built-in entries. Keep guardrails non-removable for now.
- **NG-6** — Auto-scan triggered by file marker (`Cargo.toml`/`package.json`) instead of `.git`. The "in git repo" check is sufficient for now per user decision.
- **NG-7** — Recovery from a partial leak — if a user already has a 90 GB process, this PRD does not include automatic kill/restart logic.
- **NG-8** — `SESHAT_BYPASS_DANGEROUS_CWD=1` env-var escape hatch. `--repo` is sufficient.

## Technical Considerations

### File map of changes

| File | Change |
|------|--------|
| `crates/seshat-cli/src/dangerous_path.rs` (NEW) | `is_dangerous_cwd()`, per-OS denylist constants via `cfg`, canonicalization helpers, unit tests |
| `crates/seshat-cli/src/lib.rs` | Wire new module |
| `crates/seshat-cli/src/db.rs` | `resolve_serve_db_or_project_root` calls `is_dangerous_cwd`; `--repo` override warning; new tests |
| `crates/seshat-cli/src/error.rs` (or wherever `CliError` lives — verify) | New `CliError::DangerousCwd { path, hint }` variant + Display |
| `crates/seshat-cli/src/serve.rs` | Gate watcher launch on `scan_state.error_message().is_none()`; in-task re-check after `wait_for_scan`; banner enrichment (optional per OQ-5) |
| `crates/seshat-cli/Cargo.toml` | Add `dirs` dep if not present; `assert_cmd`, `tempfile` as dev-deps if not present |
| `crates/seshat-watcher/src/error.rs` | New `WatcherError::ScanFailed(String)` variant |
| `crates/seshat-core/src/config.rs` | `ScanConfig::additional_denylist_paths: Vec<String>` field with `#[serde(default)]` |
| `seshat.example.toml` | Document `additional_denylist_paths` with example |
| `crates/seshat-cli/tests/serve_guardrails.rs` (NEW) | Subprocess-level integration tests |
| `CHANGELOG.md` | Entry under next-version `### Fixed` |
| `README.md` | Section on `serve` updated if it documents the command |
| Workspace `Cargo.toml` | Patch version bump |

### Cross-platform notes

- Use `dirs::home_dir()` (or existing `home`-style dep — verify before adding new dep).
- `cfg(target_os = "macos")`, `cfg(target_os = "linux")`, `cfg(target_os = "windows")` for denylist tables. Use `cfg(unix)` for entries shared by macOS+Linux.
- `Path::canonicalize` follows symlinks. If a denylist entry doesn't exist on this machine, skip silently.
- macOS/Windows case-insensitivity: lowercase both via `path.to_string_lossy().to_lowercase()`. On Linux: byte-exact.
- Windows drive roots: regex-match `^[A-Za-z]:\\?$` rather than enumerating drives.
- `path.starts_with(prefix)` is the `Path` trait method — does component-wise prefix matching, which is what we want (`/var` does not match `/var2/foo`).

### Testing strategy

- `assert_cmd::Command` + `tempfile::TempDir`. Invoke the built binary via `env!("CARGO_BIN_EXE_seshat")`.
- For unit tests: pass home-dir as parameter (or via test-only helper that resolves once); avoid touching the real `$HOME`.
- Capture stderr to verify warning / error messages.

### Backward compatibility

- `ScanConfig::additional_denylist_paths` is `#[serde(default)]` — old configs work.
- Behaviour change: `seshat serve` from `$HOME` now refuses. Documented in CHANGELOG. Recovery path is `cd <project>` or `--repo <path>`.

### Performance

- `is_dangerous_cwd` runs once per `serve` invocation. Canonicalize cost: O(path-depth) syscalls — negligible.
- No hot-path impact on MCP tool calls or scanning itself.

## Success Metrics

- **SM-1** — `seshat serve` from `$HOME` (or any denylisted dir without `.git`) exits with non-zero in <1 second; no `notify-debouncer-full::Debouncer` ever constructed.
- **SM-2** — Re-running the monitor experiment shows zero new processes with cwd=`$HOME` and unbounded growth — the bug class is closed.
- **SM-3** — All existing `cargo test` suites pass on macOS and Linux.
- **SM-4** — `seshat serve` running idle in a real project stays bounded (<200 MB resident) over 30-min monitor session.
- **SM-5** — Manual review confirms stderr error/warn messages are actionable.

## Open Questions

- **OQ-1** — Should `is_dangerous_cwd` also check macOS-specific `~/.cache`, `~/.config`? **Tentative:** yes, listed explicitly above. Confirm in review.
- **OQ-2** — `cd /tmp && mkdir test && cd test && seshat serve` — `/tmp` is denylisted, so `/tmp/test` is also denylisted (parent wins per US decision 4A). Acceptable? **Tentative:** yes; user can use `--repo /tmp/test` to opt in. Confirm in review.
- **OQ-3** — Exact location of `CliError` definition — needs to be confirmed during implementation (likely `crates/seshat-cli/src/error.rs`, but may be inline in `lib.rs` or per-command).
- **OQ-4** — Banner message: keep generic `"disabled (auto-scan failed)"` or enrich to `"disabled (auto-scan failed: too many files)"` using `scan_state.error_message()`? **Tentative:** enrich — low cost, more informative.
- **OQ-5** — Should we add a special-case allowance for `seshat scan` (the explicit one-shot scan command, not `serve`) to bypass the dangerous-cwd check entirely? `scan` does not start a watcher, so the leak doesn't apply. **Tentative:** yes — `scan` should keep its current behaviour; only `serve` enforces the guard. Verify during implementation.
- **OQ-6** — Should the `ScanState::wait_for_scan()` semantics change at all (e.g. propagate the failure as Result), or only the call-site re-check pattern? **Tentative:** keep `wait_for_scan` unchanged, do the re-check at call site — minimal blast radius. Confirm in review.
