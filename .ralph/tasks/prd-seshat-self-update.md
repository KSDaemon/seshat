# PRD: Seshat Self-Update System

## Introduction

**Type:** Feature

Add `seshat update` and `seshat update --check` commands so users can discover and
install new versions of Seshat without needing to remember which package manager
they used or manually curl binaries from GitHub Releases.

The CLI queries the **GitHub Releases API** (`/repos/KSDaemon/seshat/releases/latest`)
as the single source of truth for both the latest version and all downloadable assets.
SHA256 checksums are verified against `sha256sums.txt` in the same release.

Additionally, every user-facing command (`serve`, `scan`, `status`, etc.) prints a
one-line update notice to **stderr** when a newer version's binaries are available.
The check is cached for 24 hours — at most one network call per day.

## Goals

- Give users a single, package-manager-agnostic way to update seshat
- Surface update availability at the moment users interact with the CLI (not just when they remember to check)
- Never block or slow down commands — the check is cached (24h TTL) and the notice is a single stderr line
- Handle installation methods correctly: direct binary, Homebrew, cargo install
- Verify downloaded binaries with SHA256 checksums
- Survive network failures gracefully (timeout → silent skip for background check; error for explicit commands)

## User Stories

### US-001: `seshat update --check` — passive version check

**Description:** As a seshat user, I want to quickly check whether a newer version
exists without installing anything, so I can decide whether to upgrade.

**Acceptance Criteria:**
- [ ] `seshat update --check` prints exactly one of:
  - "Seshat v0.2.0 is available (current: v0.1.0). Run `seshat update` to upgrade."
  - "Seshat is up to date (v0.1.0)."
- [ ] If installed via Homebrew: "Seshat v0.2.0 is available. You installed via Homebrew. Run `brew upgrade seshat`."
- [ ] Uses the 24h version cache; does NOT hit the network if cache is fresh
- [ ] No output other than the one-line result (no progress bars, no logging noise)
- [ ] Exits with status 0 in both cases
- [ ] Network errors print: "Could not check for updates: [reason]" to stderr, exit 1
- [ ] `--help` shows the `--check` flag
- [ ] Typecheck/lint passes

### US-002: `seshat update` — full self-update

**Description:** As a seshat user, I want to upgrade to the latest version with a
single command.

**Acceptance Criteria:**
- [ ] Detects install method (direct binary, Homebrew, cargo install)
- [ ] If installed via Homebrew: prints "Seshat was installed via Homebrew. Self-update is disabled. Run `brew upgrade seshat`." and exits 1
- [ ] If installed via direct binary or cargo install: proceeds
- [ ] Fetches latest release from GitHub API. If already latest → "Seshat is up to date (v0.1.0)." and exits 0
- [ ] If a release tag is newer but no binary asset exists for the current target triple → treats as "no update" (no message, up to date)
- [ ] Downloads the correct platform binary from the matched asset
- [ ] Downloads `sha256sums.txt` from the same release and verifies the archive checksum. Mismatch → abort with error, existing binary untouched
- [ ] Shows download progress (percentage or spinner)
- [ ] Extracts binary and runs pre-flight check: spawns `new_binary --version`
  - On success → proceeds with replacement
  - On Gatekeeper kill (macOS `Killed: 9`) → prints warning with `xattr -d com.apple.quarantine` instructions, cleans up, exits 1
- [ ] Atomically replaces current executable via `rename(2)` (resolve symlink first)
- [ ] If installed via cargo install: prints "Note: `cargo install --list` will still show the old version. This is expected."
- [ ] Prints "Seshat updated to v0.2.0." and exits 0
- [ ] On Windows: prints "Self-update is not supported on Windows. Use `cargo install seshat` or download from GitHub Releases." and exits 1
- [ ] If download or extraction fails: prints error, does NOT corrupt existing binary (downloads to temp first)
- [ ] If `rename(2)` fails with EXDEV (cross-filesystem): falls back to copy + remove
- [ ] Typecheck/lint passes

### US-003: Background update notice on CLI commands

**Description:** As a seshat user, I want to see a gentle notice when a newer
version exists, so I know to run `seshat update` without actively checking.

**Acceptance Criteria:**
- [ ] At startup of ANY command (except `seshat update` itself and `seshat update --check`), prints update notice to **stderr** if a newer version's binaries are available
- [ ] Uses 24h version cache — max one GitHub API call per day
- [ ] Notice format: "Seshat v0.2.0 is available (current: v0.1.0). Run `seshat update` to upgrade."
- [ ] Printed for: `serve`, `scan`, `status`, `review`, `init`, `uninstall`, `--help`, `--version`
- [ ] NOT printed during `seshat update` or `seshat update --check` (user is already checking/updating)
- [ ] Network failures silently skip the check (no error, no delay, no output)
- [ ] No binary asset found for this target → no notice (silent)
- [ ] Typecheck/lint passes

### US-004: Version cache system

**Description:** As a developer, I want a file-based version cache so the CLI
doesn't hit the GitHub API on every invocation.

**Acceptance Criteria:**
- [ ] Cache file at `$XDG_DATA_HOME/seshat/version-check.json` (same seshat directory as the call log)
- [ ] Schema: `{ "latest_version": "0.2.0", "checked_at": "2026-05-03T12:00:00Z" }`
- [ ] Cache considered stale after 24 hours
- [ ] If cache file doesn't exist → treated as stale → does live check
- [ ] On successful live check → writes cache immediately
- [ ] On failed live check → does NOT write cache (retains old stale entry)
- [ ] Corrupted cache JSON → treated as stale
- [ ] If no binary found for current target → cache writes `checked_at` with `latest_version = current_version` (prevents re-check for 24h)
- [ ] Creates `~/.seshat/` directory if it doesn't exist
- [ ] Typecheck/lint passes

## Functional Requirements

### Version resolution
- **FR-1:** Single version source: `GET https://api.github.com/repos/KSDaemon/seshat/releases/latest`
- **FR-2:** Parse `tag_name` from the response, strip leading `v`, compare with `env!("CARGO_PKG_VERSION")` using `semver`
- **FR-3:** Find a binary asset matching the current target triple in `assets[]`
- **FR-4:** If no matching binary asset → treat as "no update available"
- **FR-5:** Find `sha256sums.txt` asset in the same release

### Binary download and verification
- **FR-6:** Download `sha256sums.txt`, parse the SHA256 line for the matched archive
- **FR-7:** Compute SHA256 of the downloaded archive, abort if mismatch
- **FR-8:** Extract archive — expect `seshat-{target}-v{version}/seshat` inside
- **FR-9:** Spawn extracted binary with `--version` as pre-flight gatekeeper check
- **FR-10:** All network calls have a 15s timeout

### Replace
- **FR-11:** Resolve symlinks before replacing (replace the actual binary, not the symlink)
- **FR-12:** Atomic replace: download to temp → extract → pre-flight → `fs::rename(temp → current_exe)`
- **FR-13:** If `rename(2)` fails with EXDEV → copy + remove temp directory fallback
- **FR-14:** Never touch the existing binary until verification passes

### Install method detection
- **FR-15:** Binary path contains `/Cellar/` → Homebrew
- **FR-16:** Binary is a symlink pointing into `/Cellar/` → Homebrew
- **FR-17:** Everything else → Direct (includes cargo install)
- **FR-18:** Cargo install detection: check `~/.cargo/.crates2.json` then `~/.cargo/.crates.toml` for a `seshat` entry → if found, print cargo note after update
- **FR-19:** If detection files are missing or unparseable → skip cargo note gracefully

### Background notice
- **FR-20:** `check_and_print_update_notice()` called once in `run()` before every command dispatch
- **FR-21:** Suppressed only for `Command::Update` variants
- **FR-22:** All output goes to stderr — invisible to MCP protocol consumers

### CI
- **FR-23:** `.github/workflows/release.yml` line ~52: `ARCHIVE_NAME="seshat-${TARGET_TRIPLE}-${GITHUB_REF_NAME}"`

## Non-Goals (Out of Scope)

- Homebrew formula/tap creation (separate task)
- `--version` flag on `seshat update` (always latest only)
- Downgrade support
- Windows self-update (exe file-locking) — graceful "not supported" message
- Automatic background updates (daemon, cron, etc.)
- `_notice` injection into MCP protocol responses (stdio is machine-to-machine)
- Post-update restart/respawn — update replaces binary and exits; user starts next session manually
- Pre-release tag support

## Technical Considerations

### New workspace dependencies
| Crate | Why |
|-------|-----|
| `flate2` | Tar.gz decompression (release archives) |
| `tar` | Tar archive extraction |
| `sha2` | SHA256 checksum verification (**already in workspace deps**) |
| Enable `serde` on `chrono` | Serialize `DateTime` in cache JSON |

### Existing deps covering everything else
`ureq` (HTTP), `serde`/`serde_json` (JSON), `semver` (**to be added**), `dirs` (data dir),
`chrono` (timestamps), `thiserror` (error types), `sha2` (checksums)

### No changes to `seshat-mcp` crate
Notice goes to stderr, not MCP protocol. Zero impact.

### Rate limiting
GitHub API: 60 req/hour unauthenticated (public repo). 24h cache → max 1 call/day.
Optionally check `GITHUB_TOKEN` env var for 5000 req/hour authenticated limit.

### Cache location
`dirs::data_dir()/seshat/version-check.json` — same seshat directory used for call logs.

### Platform target resolution
Build-time via `std::env::consts::ARCH` + `std::env::consts::OS`, mapped to Rust target triples:
- `aarch64` + `macos` → `aarch64-apple-darwin`
- `x86_64` + `macos` → `x86_64-apple-darwin`
- `x86_64` + `linux` → `x86_64-unknown-linux-gnu`
- `aarch64` + `linux` → `aarch64-unknown-linux-gnu`

## Files Changed

| Action | File |
|--------|------|
| 🔧 Edit (1 line) | `.github/workflows/release.yml` |
| 🔧 Edit | `crates/seshat-cli/src/args.rs` |
| 🔧 Edit | `crates/seshat-cli/src/lib.rs` |
| ✨ New | `crates/seshat-cli/src/update.rs` |
| 🔧 Edit | `Cargo.toml` (workspace deps) |
| 🔧 Edit | `crates/seshat-cli/Cargo.toml` |

## Edge Cases (Catalogued)

### Version check & cache
| # | Case | Handling |
|---|------|----------|
| EC-1 | `~/.seshat/` doesn't exist | `mkdir -p` before writing cache |
| EC-2 | Cache file empty/truncated | Treat as stale → live check |
| EC-3 | Cache JSON wrong schema | Deserialization failure → treat as stale |
| EC-4 | Cache read-only filesystem | Skip write, proceed (check live, no cache) |
| EC-5 | Two seshat processes check simultaneously | Race on cache write — last writer wins; both valid |
| EC-6 | Version == current (semver equal) | "Up to date" |
| EC-7 | API response format changes | Deserialize failure → error |
| EC-8 | GitHub API rate limited | "Rate limited by GitHub. Try again in {n} minutes." (parse `X-RateLimit-Reset`) |
| EC-9 | Network timeout | Timeout 15s, return error (explicit) or silent skip (background) |
| EC-10 | Clock skew — `checked_at` in the future | Treat as fresh |
| EC-11 | Clock jumps (machine suspended) | Normal: stale after 24h regardless |

### Install method detection
| # | Case | Handling |
|---|------|----------|
| EC-12 | `/opt/homebrew/bin/seshat` not symlinked to Cellar | No symlink → Direct |
| EC-13 | `/usr/local/bin/seshat` (manual install) | No Cellar → Direct |
| EC-14 | `current_exe()` fails | Fall back to Direct |
| EC-15 | `~/.cargo/.crates.toml` missing | Skip cargo detection |
| EC-16 | `.crates2.json` exists instead of `.crates.toml` | Check both; newer cargo uses JSON |
| EC-17 | User renamed binary after cargo install | Path detection still works; cargo note may be inaccurate — acceptable |

### Download & replace
| # | Case | Handling |
|---|------|----------|
| EC-18 | Temp directory out of space | Report error |
| EC-19 | Download interrupted (partial file) | Delete partial, report error |
| EC-20 | SHA256 mismatch | Abort, report error, delete temp files |
| EC-21 | Corrupted tar.gz (truncated) | `flate2` decompression fails → error |
| EC-22 | tar.gz missing binary (wrong layout) | Report "Binary not found in archive" |
| EC-23 | `rename(2)` EXDEV (tmp on different volume) | Fall back to copy + remove |
| EC-24 | Permission denied on exe path | Report: "Permission denied. Try: sudo seshat update" |
| EC-25 | Binary is a symlink | Resolve symlink, replace target, leave symlink intact |
| EC-26 | Binary currently running by another process | `rename(2)` replaces path — old inode held; new invocations get new binary; safe |

### macOS Gatekeeper
| # | Case | Handling |
|---|------|----------|
| EC-27 | New binary triggers Gatekeeper | Pre-flight check spawns `new_binary --version` → `Killed: 9` → print `xattr` instructions |
| EC-28 | Original binary was codesigned | Expected: signature lost on replace. Warn user. |

### Platform
| # | Case | Handling |
|---|------|----------|
| EC-29 | Rosetta (x86_64 binary on aarch64 macOS) | Compile-time arch detection → matches binary's arch, not hardware. Correct. |
| EC-30 | Linux musl — not in CI | Deterministic: always `-linux-gnu` target |
| EC-31 | Windows | Graceful: "Self-update not supported on Windows. Use `cargo install seshat` or download from GitHub Releases." |

### Interaction with other commands
| # | Case | Handling |
|---|------|----------|
| EC-32 | Notice printed during `--help` | Keep — `--help` is user-facing; one line is not distracting |
| EC-33 | Notice printed during `--version` | Keep — version + notice on separate lines |
| EC-34 | `SESHAT_LOG=debug` floods stderr | Expected — tracing and notice both go to stderr |
| EC-35 | `2>/dev/null` redirect | Expected — user intentionally suppresses stderr |

### Race conditions
| # | Case | Risk |
|---|------|------|
| RC-1 | `seshat update` in T1, `seshat scan` in T2 | T2 may print stale notice; harmless |
| RC-2 | `seshat update` while `seshat serve` is running | Serve holds old inode; path replaced; no crash |
| RC-3 | Cache write race (two commands hit stale cache) | Small wasted API call; cache write race is benign |

## Test Plan

### Unit tests (no network)
- Cache: fresh/stale/missing/corrupted → correct behavior
- Cache: 24h TTL boundary (23h59m → fresh; 24h01m → stale)
- Semver: current > latest (dev ahead of release) → "up to date"
- Semver: current == latest → "up to date"
- Semver: current < latest → version available
- Semver: tag with `v` prefix → strip prefix before parsing
- `detect_install_method`: Cellar path → Homebrew
- `detect_install_method`: symlink to Cellar → Homebrew
- `detect_install_method`: `/usr/local/bin` no symlink → Direct
- `detect_install_method`: `current_exe()` fails → Direct (fallback)
- Cargo detection: `.crates2.json` has `seshat` → true
- Cargo detection: `.crates2.json` no `seshat` → false
- Cargo detection: `.crates2.json` missing → false
- Cargo detection: `.crates.toml` has `seshat` → true
- Cargo detection: both missing → false
- Cargo detection: corrupted file → false (graceful)
- Platform target: `aarch64` + `macos` → `"aarch64-apple-darwin"`
- Platform target: `x86_64` + `macos` → `"x86_64-apple-darwin"`
- Platform target: `x86_64` + `linux` → `"x86_64-unknown-linux-gnu"`
- Tar.gz extraction: valid archive yields binary
- Tar.gz extraction: corrupted archive → error
- Tar.gz extraction: missing binary in archive → error
- SHA256 verification: matching hash → ok
- SHA256 verification: mismatched hash → error
- Gatekeeper simulation: binary runs successfully → proceed
- Gatekeeper simulation: binary killed → warning + abort

### Integration tests (mocked network)
- `run_check()` with mocked GitHub API → correct stdout message
- `run_check()` with cached fresh entry → no network call
- `run_check()` Homebrew install mocks → brew message
- `run_update()` Homebrew detection → brew message, exit 1
- `run_update()` already latest → "Up to date", exit 0
- `run_update()` download succeeds → binary replaced, success message
- `run_update()` download fails → error, existing binary intact
- `run_update()` checksum mismatch → error, existing binary intact
- Background notice: printed for `scan`, `status`, `serve`, etc.
- Background notice: suppressed for `update` and `update --check`
- Network failure during background notice → silent skip

### Smoketests (manual, not in CI)
- `seshat update --check` against real GitHub API
- `seshat update` full cycle with real GitHub Release

## Success Metrics

- User discovers an update is available without checking manually (background notice on any command)
- User goes from noticing an update to running the new version in two commands: `seshat update --check` to verify, `seshat update` to install
- No regression: commands not slowed by update checks (cache guarantees ≤ 1 network call per day)
- Binary replacement never corrupts the existing installation
- SHA256 verification prevents supply-chain tampering

## Open Questions

*(All resolved during party mode — none remain.)*
