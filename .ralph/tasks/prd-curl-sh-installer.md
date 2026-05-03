# PRD: curl | sh Installer

## 1. Introduction/Overview

**Type:** Feature

Add a one-liner `curl | sh` installer for Seshat. Currently the only installation method is `cargo install seshat-bin`, which requires the full Rust toolchain. This blocks developers using Python, TypeScript, Go, and other languages. A one-line installer removes the barrier — download and go.

**Repository:** `KSDaemon/seshat` (github.com/KSDaemon/seshat)

---

## 2. Goals

- `curl -fsSL https://raw.githubusercontent.com/KSDaemon/seshat/main/install.sh | sh` — install Seshat with a single command
- Auto-detect OS + architecture, download the correct prebuilt binary from GitHub Releases
- Verify SHA256 checksum of the downloaded binary
- Install to `~/.local/bin` with a warning if the directory is not in PATH
- Support WSL (Windows Subsystem for Linux)
- Fix the bug in `release.yml` — GitHub Releases are never created due to a broken trigger
- Add `sha256sums.txt` to GitHub Release for integrity verification
- Dry-run mode (`--dry-run`) for preview without installation
- Shellcheck + actionlint: 0 warnings, 0 errors

---

## 3. User Stories

### US-001: Fix release workflow to publish GitHub Releases

**Description:** As a developer, I want `release.yml` to actually publish GitHub Releases with prebuilt binaries when a version tag is pushed.

**Acceptance Criteria:**

- [ ] `release.yml` triggers on `push: tags: ['v*']` (in addition to existing `push: branches: [main]`)
- [ ] `build-binaries` and `upload-release` run only on tag push (`if: startsWith(github.ref, 'refs/tags/')`)
- [ ] `release-plz` job does not break on tag push (it internally checks what to do)
- [ ] `build-binaries` step generates `sha256sums.txt` from all built archives
- [ ] `build-binaries`: a separate `.sha256` file for each target
- [ ] `upload-release` attaches `sha256sums.txt` to the GitHub Release
- [ ] `actionlint` passes without errors on all three workflow files

### US-002: Create install.sh script

**Description:** As a developer without the Rust toolchain, I want to install Seshat with `curl | sh`.

**Acceptance Criteria:**

- [ ] **OS detection**: `uname -s` + `uname -m` → target triple:

| OS | Architecture | Target |
|----|-------------|--------|
| Linux | x86_64 / amd64 | `x86_64-unknown-linux-gnu` |
| Linux | aarch64 / arm64 | `aarch64-unknown-linux-gnu` |
| Darwin | x86_64 / amd64 | `x86_64-apple-darwin` |
| Darwin | arm64 / aarch64 | `aarch64-apple-darwin` |

- [ ] **WSL detection**: if `uname -s = Linux` and `/proc/version` contains `microsoft` — prints "Detected WSL environment"
- [ ] **Unsupported platform**: clear error message with a link to GitHub Releases for manual Windows `.zip` download
- [ ] **GitHub API**: `curl -fsSL https://api.github.com/repos/KSDaemon/seshat/releases/latest` → parses `tag_name`
- [ ] **Download**: downloads `seshat-{target}.tar.gz` from `https://github.com/KSDaemon/seshat/releases/download/{tag}/`
- [ ] **Checksum**: downloads `sha256sums.txt`, verifies SHA256 of the archive
- [ ] **Extract**: `tar xzf` → finds the binary inside the extracted directory
- [ ] **Install**: `mkdir -p ~/.local/bin`, `cp seshat ~/.local/bin/seshat`, `chmod +x`
- [ ] **Idempotent**: re-running overwrites the existing binary without errors
- [ ] **PATH warning**: if `~/.local/bin` is not in `$PATH` — prints instructions on how to add it
- [ ] **Trap cleanup**: removes the temporary directory on exit (including errors)
- [ ] **Non-zero exit**: exit code ≠ 0 on any error
- [ ] **set -eu**: strict mode — error on unset variable, error on command failure
- [ ] **POSIX shell**: compatible with bash, zsh, dash (sh, not bash)
- [ ] **`--dry-run`**: flag shows what would be done without actual installation
- [ ] **Shellcheck**: `shellcheck install.sh` — 0 warnings, 0 errors

### US-003: Add install instructions to project docs

**Description:** As a new user, I can find install instructions in the repo.

**Acceptance Criteria:**

- [ ] `tools/README.md`: "Installing Seshat" section with curl command + dry-run + cargo alternative
- [ ] Commands use the correct repository: `KSDaemon/seshat`

---

## 4. Functional Requirements

| FR | Description | AC |
|----|----------|-----|
| FR-1 | `release.yml` fixed — triggers on `tags: ['v*']`, publishes GitHub Release with binaries | US-001 |
| FR-2 | `release.yml` generates `sha256sums.txt` and `.sha256` for each target | US-001 |
| FR-3 | `install.sh` detects OS and architecture via `uname` | US-002 |
| FR-4 | `install.sh` detects WSL via `/proc/version` | US-002 |
| FR-5 | `install.sh` fetches latest release via GitHub API | US-002 |
| FR-6 | `install.sh` verifies SHA256 checksum before installation | US-002 |
| FR-7 | `install.sh` installs the binary to `~/.local/bin` | US-002 |
| FR-8 | `install.sh` warns if `~/.local/bin` is not in PATH | US-002 |
| FR-9 | `install.sh` supports `--dry-run` | US-002 |
| FR-10 | `install.sh` handles errors correctly: trap, set -eu, non-zero exit | US-002 |
| FR-11 | `tools/README.md` contains installation instructions | US-003 |
| FR-12 | Shellcheck: 0 warnings | US-002 |
| FR-13 | Actionlint: 0 errors on all workflows | US-001 |

---

## 5. Non-Goals (Out of Scope)

- Windows native in `install.sh` (for Windows — manual `.zip` download or `cargo install`)
- Custom domain `seshat.dev` (not yet; hosted from `raw.githubusercontent.com/KSDaemon/seshat/main/`)
- Interactive prompts (directory selection, confirmation)
- Custom install directory via flag (only `~/.local/bin`)
- Homebrew formula, apt/dnf/yum repositories, Nix package, Docker image
- Installing a specific version (`--version v0.2.0`)
- Auto-adding `~/.local/bin` to shell rc files (warning only)
- GPG signature verification (SHA256 checksum only)
- GitHub API response caching (not needed yet — 1 request per install)

---

## 6. Design Considerations

**install.sh structure (~100 lines):**

```
1. set -eu
2. Configuration: REPO, INSTALL_DIR, BIN_NAME
3. OS/Arch detection (case uname -s / uname -m)
4. WSL detection (/proc/version)
5. --dry-run check
6. mktemp + trap cleanup EXIT
7. GitHub API → tag_name
8. Download tarball + sha256sums.txt
9. Verify checksum (sha256sum)
10. Extract (tar xzf)
11. mkdir -p + cp + chmod +x
12. PATH warning if needed
```

**WSL detection:**
```sh
grep -qi microsoft /proc/version 2>/dev/null
```
Standard, reliable method. Linux x86_64 binaries work without issues in WSL.

---

## 7. Technical Considerations

- **GitHub API rate limit**: unauthenticated — 60 req/hour. One call per install. For heavy usage, add `GITHUB_TOKEN` or caching.
- **Raw.githubusercontent.com**: cached by CDN, available without delays.
- **Shell**: POSIX sh (#!/bin/sh), not bash. `$()` instead of backticks, `=` instead of `==` in test, `case` instead of `[[ ]]`.
- **sha256sum**: available on all Linux and macOS. On macOS the built-in `shasum -a 256` utility works identically.
- **macOS sha256sum**: macOS has no `sha256sum`, it has `shasum -a 256`. Must handle both variants.
- **Tarball structure**: the release workflow packages a `seshat-{target}/` directory with the binary inside. `tar xzf` extracts preserving the structure; need to find the binary inside via `find`.
- **release.yml fix**: minimal change — add `tags: ['v*']` to `on.push` and fix `if:` gates.

---

## 8. Success Metrics

- A developer without the Rust toolchain installs Seshat with a single command
- Installation takes < 10 seconds (most time — downloading ~5-15 MB)
- SHA256 verification prevents use of corrupted binaries
- WSL users get the same experience as native Linux
- Shellcheck: 0 warnings. Actionlint: 0 errors.
- Existing CI (`ci.yml`, `lint-workflows.yml`) is not broken
- `release-plz` flow (creating a PR with changelog on push to main) continues to work

---

## 9. Open Questions

_None. All questions are resolved:_

- Repository: `KSDaemon/seshat` (confirmed via git remote)
- Install directory: `~/.local/bin` (confirmed)
- Checksum verification: sha256 (confirmed)
- WSL: supported (confirmed)
- Shellcheck + actionlint: mandatory (confirmed)
