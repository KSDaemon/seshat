# PRD: Seshat Windows Self-Update

## Introduction

**Type:** Feature

Extend `seshat update` and `seshat update --check` so they actually fetch and install
the latest release on Windows, instead of printing a hard-coded "not supported"
message and exiting with an error. This closes the platform gap left intentionally
open by the original `prd-seshat-self-update.md` (EC-31) and brings Windows users
to parity with macOS/Linux.

The update flow is unchanged at the high level: query the **GitHub Releases API**,
match the binary asset for the current target triple, verify the SHA256 checksum
against `sha256sums.txt` in the same release, run a pre-flight `--version` check on
the extracted binary, and then atomically replace the running executable.

The Windows-specific work is concentrated in two places:

1. **Archive extraction:** the Windows release artifact is a `.zip` (not a `.tar.gz`),
   so the extractor needs to dispatch on archive type.
2. **Binary replacement:** Windows holds an exclusive lock on a running `.exe` and
   refuses to delete or overwrite it, but **does** permit renaming. The standard
   "rename current → backup, drop new in place, clean up backup later" pattern is
   used, implemented via the `self_replace` crate.

Background notice (from US-003 of the original PRD) and the 24-hour version cache
already work on Windows — they are pure stderr/JSON code paths with no platform-gated
behavior. This PRD only verifies that none of the Windows-specific changes regress
that behavior.

## Goals

- Give Windows users the same single-command upgrade flow as macOS/Linux users.
- Reuse 100% of the existing version-check, cache, asset-discovery, SHA256 verify,
  pre-flight, and rate-limit handling code paths — no duplication, no parallel
  Windows-only update implementation.
- Use a small, battle-tested crate (`self_replace`) for the platform-specific
  replace step instead of hand-rolling Win32 API calls and rebooting-required
  scheduling.
- Keep the surface area minimal: detect only **Direct** and **cargo install** on
  Windows in v1; defer Scoop / Chocolatey / winget detection to a follow-up tracked
  in the roadmap.
- Verify Windows code paths don't bitrot by adding `windows-latest` to the CI
  test matrix.
- Fix the related archive-layout regression (FR-23 from the original PRD) so that
  self-update works against real release tags on **all** platforms, not just
  Windows.

## User Stories

### US-001: `seshat update --check` on Windows — passive version check

**Description:** As a Seshat user on Windows, I want to quickly check whether a
newer version exists without installing anything, so I can decide whether to
upgrade.

**Acceptance Criteria:**
- [ ] On Windows, `seshat update --check` prints exactly one of:
  - "Seshat v0.2.0 is available (current: v0.1.0). Run `seshat update` to upgrade."
  - "Seshat is up to date (v0.1.0)."
- [ ] Uses the existing 24h version cache; does NOT hit the network if cache is fresh
- [ ] No output other than the one-line result (no progress bars, no logging noise)
- [ ] Exits with status 0 in both cases
- [ ] Network errors print: "Could not check for updates: [reason]" to stderr, exit 1
- [ ] Behavior is identical to macOS/Linux — same code path, no Windows-specific branch
- [ ] Typecheck/lint passes; `cargo clippy --all-targets -- -D warnings` clean

### US-002: `seshat update` on Windows — full self-update

**Description:** As a Seshat user on Windows, I want to upgrade to the latest
version with a single command.

**Acceptance Criteria:**
- [ ] The early-return Windows guard in `run_self_update()` (currently
      `crates/seshat-cli/src/update.rs:88-96`) is removed.
- [ ] `current_target()` recognises Windows: `("x86_64", "windows") => "x86_64-pc-windows-msvc"`.
- [ ] Detects install method: Direct binary or cargo install. All other methods
      (Scoop, Chocolatey, winget, manual install elsewhere) fall through to **Direct**
      for v1.
- [ ] Fetches the latest release from the GitHub API. If already latest →
      "Seshat is up to date (v0.1.0)." and exits 0.
- [ ] If a release tag is newer but no `.zip` asset exists for `x86_64-pc-windows-msvc`
      → treats as "no update" (no message, up to date), same as Unix when the matching
      `.tar.gz` is missing.
- [ ] `find_binary_asset` matches `.zip` (in addition to `.tar.gz` / `.tgz`) when the
      current target is `x86_64-pc-windows-msvc`.
- [ ] Downloads `sha256sums.txt` from the same release and verifies the archive
      checksum. Mismatch → abort with error, existing `seshat.exe` untouched.
- [ ] Shows download progress (existing `indicatif` spinner — already cross-platform).
- [ ] Extracts `.zip` archive into a temp directory; expected layout:
      `seshat-x86_64-pc-windows-msvc-v{version}/seshat.exe`.
- [ ] Pre-flight check: spawn `new_binary --version`. On non-zero exit → cleanup,
      report error, exit 1. (No macOS Gatekeeper handling on Windows; signal-based
      checks are skipped via `cfg(unix)` as today.)
- [ ] Atomically replaces the running executable via `self_replace::self_replace(new_path)`.
      On Windows this renames `seshat.exe` → `seshat.exe.old`, moves the new binary into
      place, and arranges for `.old` cleanup on next process startup. On Unix the same
      call collapses to `rename(2)` with the existing EXDEV fallback inside the crate.
- [ ] If installed via cargo install: prints the existing cargo note ("Note:
      `cargo install --list` will still show the old version. This is expected.").
- [ ] Prints "Seshat updated to v0.2.0." and exits 0.
- [ ] If download or extraction fails: prints error, does NOT corrupt the existing
      binary (downloads to TempDir; replace happens last).
- [ ] If the user lacks permission to write `current_exe()` (typical for
      `C:\Program Files\seshat\`): print "Permission denied. Try running as
      Administrator." and exit 1.
- [ ] Typecheck/lint passes; `cargo clippy --all-targets -- -D warnings` clean.

### US-003: Cleanup of leftover backup `.exe.old`

**Description:** As a Seshat user on Windows, I want any stale `.exe.old`
backup left next to `seshat.exe` (e.g. by a manual recovery or a
half-failed update) to be cleaned up automatically the next time I run a
seshat command, so my install directory doesn't accumulate stale files.

> **Implementation note (deviation from earlier draft).** Earlier drafts
> mandated calling `self_replace::self_delete_outside_path(env!("CARGO_PKG_NAME"))`
> directly. After integration we found that `self_replace 1.5.0` does
> *not* match the no-op-on-Unix model the original draft assumed: on Unix
> the function unconditionally `fs::remove_file(current_exe())` — calling
> it from `cargo test` would brick the test binary mid-run. On Windows
> the same crate already arranges its own post-shutdown cleanup of the
> relocated binary via a spawned `.__selfdelete__.exe` helper, so a
> caller-side `self_delete_outside_path` is not what cleans up the
> happy-path `.exe.old`. The wrapper described below is the safe shape:
> Windows-only, defensive, and aligned with what the crate actually does.

**Acceptance Criteria:**
- [ ] At startup of every command (in `crates/seshat-cli/src/lib.rs:run()`),
      `update::cleanup_stale_old_binary()` is called once, best-effort,
      errors ignored.
- [ ] On Windows the helper probes `<current_exe>.old` next to the running
      binary and removes it via `fs::remove_file`. On Unix the helper is a
      `cfg(windows)`-gated no-op — atomic `rename(2)` semantics in
      `replace_binary` never leave `.old` behind, so there is nothing to
      clean.
- [ ] Cleanup runs **before** command dispatch but **after** tracing init,
      so cleanup failures aren't logged as warnings.
- [ ] Cleanup is suppressed for the same commands that suppress
      `check_and_print_update_notice` — i.e. `Update` and `Completions`.
- [ ] If `seshat.exe.old` cannot be removed (file locked by another process,
      e.g. an antivirus scanner), the cleanup silently moves on — no error
      surfaced to the user.

### US-004: Background update notice still works on Windows

**Description:** As a Seshat user on Windows, I want the same gentle one-line
update notice on stderr that macOS/Linux users get when a new version is available.

**Acceptance Criteria:**
- [ ] Background notice prints to stderr on `serve`, `scan`, `status`, `review`,
      `init`, `uninstall`, `--help`, `--version`.
- [ ] Notice format: "Seshat v0.2.0 is available (current: v0.1.0). Run `seshat update`
      to upgrade."
- [ ] Suppressed for `seshat update`, `seshat update --check`, `seshat completions`.
- [ ] Network failures silently skip the check (no error, no delay, no output).
- [ ] Behavior is verified by a unit test that runs on `windows-latest`.

### US-005: Archive layout regression fix (FR-23)

**Description:** As a Seshat maintainer, I want `release.yml` to produce an archive
whose internal directory matches what `extract_binary` expects, so self-update
actually works on the next published tag — for **all** platforms, not just Windows.

**Acceptance Criteria:**
- [ ] `.github/workflows/release.yml` line 52 reads:
      `ARCHIVE_NAME="seshat-${TARGET_TRIPLE}-${GITHUB_REF_NAME}"` (restoring
      FR-23 from `prd-seshat-self-update.md`, dropped during the May 3 split).
- [ ] After the change, the extracted layout is:
  - macOS/Linux: `seshat-{triple}-vX.Y.Z/seshat`
  - Windows: `seshat-{triple}-vX.Y.Z/seshat.exe`
- [ ] Matches `extract_binary`'s `expected_dir = format!("seshat-{target}-v{version}")`.
- [ ] Existing test fixtures (`update.rs:1198`) remain valid — they already use the
      versioned dir name.
- [ ] Verified by a dry-run release against a test tag (or by reviewing the workflow
      diff plus existing tests, since no real release has happened yet — no `v*` tags
      exist in the repo as of 2026-05-09).

### US-006: Windows-latest in CI test matrix

**Description:** As a Seshat maintainer, I want the test suite to run on
`windows-latest` so Windows code paths don't bitrot.

**Acceptance Criteria:**
- [ ] `.github/workflows/ci.yml` test job uses a matrix that includes
      `windows-latest` alongside `ubuntu-latest`.
- [ ] On `windows-latest` the job runs `cargo test -p seshat-cli` (limited to the CLI
      crate to keep total CI runtime under 10 min).
- [ ] On `ubuntu-latest` the existing full-workspace test scope is preserved.
- [ ] Clippy and `cargo fmt --check` continue to run on Ubuntu only — no need to
      duplicate.
- [ ] CI is green on the new matrix entry before this PRD is closed.

## Functional Requirements

### Target detection
- **FR-1:** `current_target()` returns `"x86_64-pc-windows-msvc"` when
  `std::env::consts::ARCH == "x86_64"` and `std::env::consts::OS == "windows"`.
- **FR-2:** `aarch64-pc-windows-msvc` returns `"unsupported"` until release CI
  starts producing that artifact (out of scope here).

### Asset selection
- **FR-3:** When the current target is `x86_64-pc-windows-msvc`, `find_binary_asset`
  matches assets ending in `.zip`. On other targets the existing `.tar.gz` / `.tgz`
  matching is preserved.
- **FR-4:** The expected archive name embedded in `fetch_checksum_for_asset` is
  computed from the current target's archive extension —
  `.zip` for windows-msvc, `.tar.gz` otherwise.

### Archive extraction
- **FR-5:** `extract_binary` dispatches on the archive's file extension. `.tar.gz`
  takes the existing `tar` + `flate2` path. `.zip` takes a new `zip::ZipArchive` path.
- **FR-6:** Both paths apply the same safety constraints:
  - Skip empty entry names.
  - Skip entries containing `..` path components.
  - Skip entries whose canonicalised destination escapes `dest_dir`.
- **FR-7:** Expected binary path inside the archive: `seshat-{target}-v{version}/seshat`
  on Unix, `seshat-{target}-v{version}/seshat.exe` on Windows. Use
  `std::env::consts::EXE_SUFFIX` (`""` on Unix, `".exe"` on Windows) to compose
  the leaf name.
- **FR-8:** The set-executable step (`set_executable`) remains `cfg(unix)`; on
  Windows it stays a no-op (file modes are not used for execution permission).

### Replace
- **FR-9:** `replace_binary(new_binary, target_exe, temp_dir)` calls
  `self_replace::self_replace(new_binary)` and maps:
  - `Ok(())` → existing `Ok(())` flow,
  - `io::ErrorKind::PermissionDenied` → existing user-friendly message
    ("Try running as Administrator" on Windows, "Try: sudo seshat update" on Unix),
  - any other `io::Error` → `CliError::CommandFailed { reason: "failed to replace
    binary: {e}" }`.
- **FR-10:** The bespoke `EXDEV` (cross-filesystem) fallback in the current code is
  removed. `self_replace` already handles cross-FS via copy + remove internally.
- **FR-11:** `resolve_target_exe()` is unchanged; canonicalised UNC paths
  (`\\?\C:\...`) are accepted by `self_replace`.

### Cleanup
- **FR-12:** `crates/seshat-cli/src/lib.rs:run()` calls
  `update::cleanup_stale_old_binary()` exactly once, after `tracing` init
  and before command dispatch, gated by the same suppression set as
  `check_and_print_update_notice` (`Update`, `Completions`). The helper is
  `cfg(windows)`-gated; on Unix it is a no-op (see US-003 implementation
  note for why we don't call `self_replace::self_delete_outside_path`
  directly).
- **FR-13:** `cleanup_stale_old_binary` returns `()` and never surfaces an
  error to the caller; internal `fs::remove_file` failures are dropped via
  `let _ = ...`.

### Install method detection on Windows
- **FR-14:** `detect_install_method()` returns `InstallMethod::Direct` on Windows.
  Homebrew detection (`/Cellar/`) is preserved for macOS but is a no-op on Windows.
- **FR-15:** `is_cargo_install()` already uses `dirs::home_dir()` and `CARGO_HOME`,
  both of which are functional on Windows. No change required; behaviour is
  exercised by a new `cfg(windows)` unit test.
- **FR-16:** Detection of Scoop, Chocolatey, and winget is **not** implemented in
  v1. Users who installed via these tools will run self-update; the binary they
  manage gets replaced in place. This is safe (the replaced file will be
  overwritten the next time they run the manager's upgrade) but not idiomatic.
  Tracked as future work.

### Release pipeline
- **FR-17:** `.github/workflows/release.yml` line 52 restores
  `ARCHIVE_NAME="seshat-${TARGET_TRIPLE}-${GITHUB_REF_NAME}"`, so the extracted
  directory inside both `.tar.gz` and `.zip` artifacts contains the version.
- **FR-18:** No other release.yml change is required; the existing Windows
  `if [[ "${TARGET_TRIPLE}" == *windows* ]]` branch already produces a `.zip` via
  `7z`, copies `seshat.exe`, and uploads to the release.

### CI
- **FR-19:** `.github/workflows/ci.yml` test matrix gains `windows-latest`. The
  Windows job runs only `cargo test -p seshat-cli` to keep wall-clock CI cost
  manageable.

## Non-Goals (Out of Scope)

- **Detection of Scoop / Chocolatey / winget install methods.** Tracked in
  `_bmad-output/planning-artifacts/roadmap.md` as a follow-up to `#win-update`.
- **ARM64 Windows (`aarch64-pc-windows-msvc`).** Release CI does not yet build it.
- **Windows code signing.** Replaced binaries may surface SmartScreen warnings on
  first execution. A separate concern, tracked elsewhere.
- **Pre-release / draft tag support** (inherited from original PRD non-goals).
- **Restart-after-update.** The user re-launches manually; existing behaviour.
- **Rebuilding the existing macOS / Linux update flow.** All shared code paths
  stay intact; only platform-gated branches and the replace step change.
- **Adding an `aarch64-windows` matrix row to release.yml.** Defer until a
  concrete user need exists.

## Technical Considerations

### New workspace dependencies

| Crate | Why | Approx. weight |
|-------|-----|---------------|
| `self_replace` | Cross-platform replace of the running executable, including the Windows rename + cleanup trick | ~200 LOC, no transitive deps beyond `windows-sys` (Windows-only) |
| `zip` | Extract `.zip` archives produced by `release.yml` for the Windows target | Pull in with `default-features = false, features = ["deflate"]` to avoid pulling timezone / encryption code |

### Existing deps covering everything else

`ureq` (HTTP), `serde`/`serde_json` (JSON), `flate2` + `tar` (Unix archive),
`semver`-like comparison via `is_newer` (already in tree), `dirs` (data dir),
`chrono` (timestamps), `thiserror`, `sha2` (checksums), `indicatif` (progress),
`tempfile` (atomic staging).

### Error mapping

`replace_binary` translates `PermissionDenied` to a platform-aware hint:

| Platform | Hint |
|----------|------|
| Unix | `Permission denied updating <path>. Try: sudo seshat update` |
| Windows | `Permission denied updating <path>. Try running as Administrator.` |

All other `io::Error` variants map to the existing
`CliError::CommandFailed { reason: format!("failed to replace binary: {e}") }`.

### `self_replace` correctness notes

- On Windows: `self_replace::self_replace(new_path)` relocates the
  running `seshat.exe` out of the install directory (the crate spawns a
  `.__selfdelete__.exe` helper that deletes the relocated binary post
  shutdown) and moves `new_path` into place. The crate's own helper
  handles cleanup of the relocated binary; our `cleanup_stale_old_binary`
  wrapper is purely defensive against `<exe>.old` files left over from
  manual recovery or earlier `self_replace` versions.
- On Unix: collapses to `rename(new_path, current_exe)`. The crate also
  handles cross-FS via copy-and-replace internally, replacing the
  hand-rolled EXDEV fallback this PR removes.
- Symlinks: the crate canonicalises before replacing, matching our existing
  `resolve_target_exe()` behaviour.

### CI cost

`windows-latest` runners on GitHub Actions cost ~2× a Linux minute. Limiting the
Windows job to `cargo test -p seshat-cli` keeps the total Windows minutes well
under the macOS test job's runtime, so net CI cost increase is bounded.

### No changes to `seshat-mcp`

The MCP server is unaffected. Stdout stays clean; the only writes are to stderr
(notices) and to disk (cache, downloaded archives, replacement binary).

## Files Changed

| Action | File |
|--------|------|
| 🔧 Edit | `Cargo.toml` (workspace deps: add `self_replace`, `zip`) |
| 🔧 Edit | `crates/seshat-cli/Cargo.toml` (propagate the two deps) |
| 🔧 Edit | `crates/seshat-cli/src/update.rs` (Windows target, asset matcher, zip extraction, drop guard, `self_replace` integration) |
| 🔧 Edit | `crates/seshat-cli/src/lib.rs` (call `update::cleanup_stale_old_binary()` once at startup) |
| 🔧 Edit (1 line) | `.github/workflows/release.yml` (FR-17 — restore `${GITHUB_REF_NAME}`) |
| 🔧 Edit | `.github/workflows/ci.yml` (add `windows-latest` to test matrix) |
| 🔧 Edit | `_bmad-output/planning-artifacts/roadmap.md` (mark `#win-update` scope; add Scoop/Chocolatey/winget future-work entry) |
| 🔧 Edit | `_bmad-output/planning-artifacts/epics.md` (add Story 9.6 under Epic 9) |
| 🔧 Edit | `.ralph/tasks/prd-seshat-self-update.md` (annotate Windows non-goal as superseded by this PRD) |
| ✨ New (optional) | `_bmad-output/implementation-artifacts/9-6-windows-self-update.md` |

## Edge Cases (Catalogued)

### Asset selection and download
| # | Case | Handling |
|---|------|----------|
| EC-1 | Release has only `.tar.gz` assets, no `.zip` for Windows target | Treat as "no update available" (silent on background notice; "up to date" on `update --check`) |
| EC-2 | `.zip` asset exists but `sha256sums.txt` does not list it | `fetch_checksum_for_asset` returns "checksums file not found" → existing error path |
| EC-3 | `.zip` archive missing `seshat.exe` at expected path | `extract_binary` returns "extracted binary not found at expected path" |
| EC-4 | `.zip` archive contains entries that escape `dest_dir` | Skipped silently, same as `tar` path |

### Replace
| # | Case | Handling |
|---|------|----------|
| EC-5 | Antivirus locks `seshat.exe` mid-replace | `self_replace` returns `io::Error`; we map to `CommandFailed { reason: "failed to replace binary: <err>" }`; existing binary intact |
| EC-6 | User running multiple `seshat serve` processes | The OS allows `MoveFileEx` even with the file mapped/executing; replacement succeeds; running processes keep their open handle and finish; new invocations get the new binary |
| EC-7 | Permission denied (binary in `C:\Program Files\seshat\`) | Map to "Try running as Administrator" hint; exit 1 |
| EC-8 | `current_exe()` fails | Existing `CliError::CommandFailed { reason: "cannot determine current executable" }`; same on Unix |
| EC-9 | `seshat.exe.old` already exists from a previous failed update | `self_replace::self_replace` overwrites; cleanup helper removes on next launch |
| EC-10 | Cleanup helper called before any update happened | No-op (no `.old` file to delete); silent |

### Install method detection
| # | Case | Handling |
|---|------|----------|
| EC-11 | User installed via Scoop (`%USERPROFILE%\scoop\apps\seshat\current\seshat.exe`) | Detected as `Direct` in v1; self-replace runs and succeeds; user may want to re-run `scoop update seshat` afterwards (not surfaced — out of scope) |
| EC-12 | User installed via Chocolatey (`C:\ProgramData\chocolatey\lib\seshat\tools\seshat.exe`) | Detected as `Direct` in v1 |
| EC-13 | User installed via winget | Detected as `Direct` in v1 |
| EC-14 | User installed via `cargo install seshat` (Windows) | Detected via `~/.cargo/.crates2.json`; cargo note printed after replace, identical to Unix |

### Background notice and cache
| # | Case | Handling |
|---|------|----------|
| EC-15 | Notice printed during MCP `serve` | Already to stderr only; MCP stdout unaffected; same as Unix |
| EC-16 | Cache file in `%LOCALAPPDATA%\seshat\version-check.json` | `dirs::data_dir()` returns the right path on Windows; existing code works unchanged |
| EC-17 | Two `seshat.exe` processes race on cache write | Existing race tolerated; last writer wins |

### CI
| # | Case | Handling |
|---|------|----------|
| EC-18 | Windows CI flake on shared `windows-latest` runner | Re-run; if persistent, scope down further (e.g., only `cargo test -p seshat-cli --lib`) |
| EC-19 | `cargo test` on Windows produces line-ending warnings | Tests are byte-exact for archive fixtures; line-ending issues only affect Markdown / docs which aren't tested here |

## Test Plan

### Unit tests (no network)
- `current_target_is_known_on_main_platforms` extended: also asserts on
  `cfg(windows)`.
- `find_binary_asset_matches_windows_target` — `.zip` asset, `x86_64-pc-windows-msvc`
  target.
- `find_binary_asset_skips_zip_on_unix_target` — `.zip` asset, Linux target → no
  match.
- `extract_binary_from_valid_zip` — fixture built with `zip::write::ZipWriter`.
  Cross-platform (zip parsing is OS-agnostic, so we don't need `cfg(windows)`).
- `extract_binary_corrupted_zip_errors` — random bytes → error.
- `extract_binary_zip_skips_path_traversal` — entry named `../escape/seshat.exe` →
  silently skipped, no panic.
- `extract_binary_dispatches_on_extension` — same archive bytes named once `.zip`
  and once `.tar.gz`, asserts the correct backend is used (or fails cleanly when
  the bytes don't match the extension).
- `is_cargo_install_with_fake_crates2_json_on_windows` — `cfg(windows)` mirror of
  the existing Unix test.
- `replace_binary_translates_permission_denied_to_admin_hint_on_windows` —
  `cfg(windows)`, simulate read-only target.
- `cleanup_after_update_is_noop_on_unix` — `cfg(unix)`, calling
  `update::cleanup_stale_old_binary()` is a no-op (the function body is
  `cfg(windows)`-gated). Note: we deliberately do NOT exercise
  `self_replace::self_delete_outside_path` here — see US-003
  implementation note for the rationale.

### Integration tests (mocked GitHub)
- `run_self_update_windows_happy_path` — `cfg(windows)`, mocked HTTP server
  returning a hand-built `.zip`, valid checksum, dummy binary that exits 0 on
  `--version`. Asserts: target_exe content matches new binary; `.exe.old`
  exists post-update; subsequent run cleans up `.exe.old`.
- `run_self_update_windows_sha_mismatch` — checksum mismatch → existing binary
  unchanged, `CliError::CommandFailed` returned.
- `run_self_update_windows_no_zip_asset_for_target` — release has only `.tar.gz`
  → "no update available" path.
- `run_self_update_windows_preflight_fail` — extracted binary exits non-zero on
  `--version` → cleanup + error.
- `background_notice_prints_on_windows` — `cfg(windows)`, mocked cache + API,
  asserts stderr line.

### Smoketests (manual, not in CI)
- On a Windows VM/host:
  1. Build `seshat.exe` from `main`.
  2. Bump `Cargo.toml` version, push tag → release.yml uploads
     `seshat-x86_64-pc-windows-msvc-vX.Y.Z.zip` + `sha256sums.txt`.
  3. Roll local install back to a slightly older binary.
  4. `.\seshat.exe update --check` → expect "available" line.
  5. `.\seshat.exe update` → expect download, verify, preflight, replace,
     "Seshat updated to vX.Y.Z."
  6. `.\seshat.exe --version` from the same path → reports new version.
  7. `.\seshat.exe status` (any other command) → `seshat.exe.old` is silently
     removed.
- MCP side-channel check:
  - Spawn `seshat serve` from a host (Claude Code, etc.).
  - Run `query_project_context` from the host.
  - Confirm tool output is unchanged; no extra notices on stdout.

## Success Metrics

- Windows users can upgrade with a single command (`seshat update`) and observe
  the new version on the next invocation.
- Self-update against a real GitHub release tag works on macOS, Linux, **and**
  Windows. The FR-23 archive-layout fix unblocks the Unix path too.
- No regression on the Unix flow — existing tests pass unchanged.
- CI on `windows-latest` stays green for a full release cycle (catches Windows
  bitrot before it reaches users).
- Time-to-update for a Windows user: download + verify + replace finishes in under
  10 seconds on a typical ~5 MB binary on a 50 Mbit connection.
- Zero supply-chain regressions: SHA256 mismatch still aborts the replace and
  leaves the existing binary untouched on all platforms.

## Open Questions

*(All major design decisions resolved before this PRD was written.
None remain blocking.)*

- Should the Scoop / Chocolatey / winget detection follow-up land as part of a
  Windows-distribution epic or as a standalone story under Epic 9? Decision can
  wait until those package-manager artifacts actually exist.
- Should we eventually code-sign Windows binaries to suppress SmartScreen
  warnings? Tracked separately; not part of self-update.
