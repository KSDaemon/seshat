# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
