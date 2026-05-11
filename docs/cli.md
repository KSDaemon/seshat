# Seshat CLI Reference

Full reference for the `seshat` binary. For a high-level overview of
what Seshat is and why it exists, see the [README](../README.md).

Run `seshat <command> --help` for inline help on any command.

---

## Top-level commands

Commands are listed roughly in the order you'll use them — typical
flow is `init` once, then `scan`, then let your AI client do the rest.

| Command | Purpose |
|---|---|
| [`seshat init`](#seshat-init) | Register Seshat with your AI client(s) — run once per machine |
| [`seshat scan`](#seshat-scan) | Build (or update) the knowledge graph for a project |
| [`seshat review`](#seshat-review) | Interactive TUI to triage auto-detected conventions |
| [`seshat status`](#seshat-status) | Show indexed projects, submodules, and DB info |
| [`seshat serve`](#seshat-serve) | MCP server entry point — auto-invoked by AI clients, rarely run manually |
| [`seshat decisions`](#seshat-decisions) | List / forget / export / import project-wide decisions |
| [`seshat completions`](#seshat-completions) | Print a shell completion script |
| [`seshat update`](#seshat-update) | Check for newer versions or upgrade the binary |
| [`seshat uninstall`](#seshat-uninstall) | Reverse `init` — remove Seshat configuration from AI clients |

`seshat serve` and `seshat review` automatically detect git branch
changes and incremental commits since the last scan, and refresh the
database before serving / opening the TUI. In non-git directories the
freshness checks are skipped silently and Seshat operates as a
single-branch project named `main`.

---

## `seshat scan`

Scan a project directory and display the analysis report. This is
typically the first command you run on a new project — it builds the
knowledge graph from scratch (or updates it incrementally on
subsequent runs).

```text
seshat scan [OPTIONS] <PATH>
```

| Argument / flag | Effect |
|---|---|
| `<PATH>` | Path to the project directory to scan (required) |
| `-v`, `--verbose` | Show skipped files, detector details, and timing |
| `-q`, `--quiet` | Show only errors and the final summary |
| `--exclude-submodules` | Skip git submodules (they are scanned by default) |

```bash
seshat scan .                        # scan current project
seshat scan ~/code/my-app --verbose  # detailed scan output
seshat scan . --exclude-submodules   # skip vendored submodules
```

---

## `seshat serve`

> **You normally don't run this yourself.** Once `seshat init` has
> registered Seshat with your AI client, the client spawns
> `seshat serve` on demand and tears it down when the session ends.
> The flags below are for debugging, custom transports, or
> standalone usage.

Start the MCP server that AI agents connect to. Speaks the Model
Context Protocol over stdio by default; HTTP/SSE transports are
configured via `seshat.toml` (or the flags below).

On startup, `serve` compares `branches.last_scanned_commit` against
`git rev-parse HEAD` for the active branch. If they differ
(e.g. after a `git pull`), a background incremental sync runs
automatically so the graph reflects on-disk reality.

```text
seshat serve [OPTIONS] [REPO]
```

| Argument / flag | Effect |
|---|---|
| `[REPO]` | Repository directory or project name. Auto-detected from CWD if omitted |
| `--host <HOST>` | Bind host for HTTP/SSE transport (overrides config) |
| `--port <PORT>` | Bind port for HTTP/SSE transport (overrides config) |
| `--call-log [<PATH>]` | Log MCP tool calls to JSONL. Default: `$XDG_DATA_HOME/seshat/call-log.jsonl` |

```bash
seshat serve                             # serve the current project over stdio
seshat serve ~/code/other-app            # serve a different repo
seshat serve --call-log                  # log every MCP call for later analysis
```

In practice you rarely invoke this manually — AI clients
(Claude Code, Cursor, opencode, Claude Desktop) spawn it for you once
`seshat init` has registered the MCP integration.

---

## `seshat status`

Show what Seshat knows about: indexed projects, their submodules, and
database paths.

```text
seshat status [OPTIONS]
```

| Flag | Effect |
|---|---|
| `-v`, `--verbose` | Show full database paths and additional detail |

```bash
seshat status            # quick summary
seshat status --verbose  # include DB paths and metadata
```

---

## `seshat review`

Interactive TUI for triaging auto-detected conventions. The review
queue lists conventions that have not yet been decided on; you
approve, reject, or mark them partial. Decisions are persisted
project-wide and survive branch deletion and merges (see
[`decisions`](#seshat-decisions) for managing them after the fact).

Before opening the TUI, `review` runs a blocking incremental sync to
HEAD so the queue reflects the current code, not the snapshot from
the last scan.

```text
seshat review [OPTIONS]
```

| Flag | Effect |
|---|---|
| `--no-sync` | Skip the pre-TUI sync. Use for emergency/debug access when sync would be slow; implies the queue may be stale |

---

## `seshat init`

Generate MCP configuration entries for installed AI clients. Seshat
auto-detects which clients are present and offers to patch their
config files (with backups for JSON; copy-paste snippets for JSONC).
Also installs agent instruction files, skill bundles, and hooks so
the agent knows to call Seshat tools proactively.

```text
seshat init [OPTIONS] [CLIENT]
```

| Argument / flag | Effect |
|---|---|
| `[CLIENT]` | Specific client to configure. Auto-detects all if omitted. Supported: `claude-code`, `claude-desktop`, `opencode`, `cursor` |
| `--project` | Always write to project-level configs (e.g. `.claude/settings.local.json`, `./opencode.json`) |
| `--global` | Always write to global user configs |
| `--dry-run` | Show what would change without writing any files |
| `--skip-instructions` | Write only MCP config; skip agent instructions, skills, and hooks |

```bash
seshat init                          # auto-detect, smart-scope (project if exists, else global)
seshat init claude-code --project    # only Claude Code, project-level
seshat init --dry-run                # preview without writing
```

---

## `seshat update`

Self-update the installed `seshat` binary. Checks the GitHub releases
feed, verifies the SHA256, and atomically replaces the running
binary.

```text
seshat update [OPTIONS]
```

| Flag | Effect |
|---|---|
| `--check` | Only check whether a newer version exists; do not install |

```bash
seshat update --check    # is there a newer version?
seshat update            # install it
```

When Seshat was installed via Homebrew (macOS), `update` detects this
and routes you to `brew upgrade seshat` instead. Windows
package-manager detection (Scoop / Chocolatey / winget) is on the
roadmap.

---

## `seshat completions`

Print a shell completion script to stdout.

```text
seshat completions [SHELL]
```

| Argument | Effect |
|---|---|
| `[SHELL]` | Target shell: `bash`, `zsh`, `fish`, `powershell`, `elvish`. Auto-detected from `$SHELL` if omitted |

```bash
seshat completions                                       # auto-detect
seshat completions bash > /etc/bash_completion.d/seshat
seshat completions zsh  > "${fpath[1]}/_seshat"
seshat completions fish > ~/.config/fish/completions/seshat.fish
```

The Unix release tarballs bundle pre-generated scripts in
`completions/`, so manual generation is usually only needed for
in-place updates or unusual install layouts.

---

## `seshat decisions`

Manage project-wide user decisions. Decisions are records of
approve / reject / partial / recorded outcomes for conventions —
once a convention has a decision in any state, the review TUI no
longer surfaces it on any branch.

```text
seshat decisions list   [--state STATE] [--branch BRANCH] [--format table|json]
seshat decisions forget <HASH> [--yes]
seshat decisions export <FILE>
seshat decisions import <FILE> [--strict]
```

### `decisions list`

Lists every decision in the current project's database. Defaults to a
human-readable table; pass `--format json` for an array suitable for
re-import.

| Flag | Values | Effect |
|---|---|---|
| `--state` | `approved`, `rejected`, `partial`, `recorded` | Filter by decision state |
| `--branch` | any branch name | Filter by `decided_on_branch` |
| `--format` | `table` (default), `json` | Output format |

```bash
seshat decisions list
seshat decisions list --state approved
seshat decisions list --branch main --format json
```

### `decisions forget`

Remove a decision so the underlying convention re-enters the review
queue on the next scan.

```bash
seshat decisions forget abcd1234            # prompts for confirmation
seshat decisions forget abcd1234 --yes      # non-interactive (scripts / CI)
```

The hash argument accepts either the full `description_hash` or an
unambiguous prefix of at least 4 characters. Ambiguous prefixes
report the matching short hashes so you can lengthen by hand.

### `decisions export` / `decisions import`

Round-trip decisions across machines or back up before a destructive
operation.

```bash
seshat decisions export decisions.json
seshat decisions import decisions.json            # silent latest-wins on conflicts
seshat decisions import decisions.json --strict   # fail (no writes) on any conflict
```

The export shape matches `decisions list --format json`. Import
UPSERTs by `description_hash`; on conflict the row with the larger
`decided_at` wins silently. `--strict` aborts before any write if the
input contains a hash that already exists in the local DB, listing
every conflicting hash so you can resolve them by hand.

---

## `seshat uninstall`

Reverse of `seshat init`: remove all Seshat configuration from
detected AI clients (MCP entries, instruction sections, skill
directories, hook scripts). Does **not** remove the `seshat` binary
or the per-project `.db` files.

```text
seshat uninstall [OPTIONS] [CLIENT]
```

| Argument / flag | Effect |
|---|---|
| `[CLIENT]` | Specific client. Auto-detects all if omitted. Supported: `claude-code`, `claude-desktop`, `opencode`, `cursor` |
| `--project` | Only uninstall from project-level configs |
| `--global` | Only uninstall from global user configs |
| `--dry-run` | Show what would be removed without making changes |

```bash
seshat uninstall                       # remove from all detected clients
seshat uninstall claude-code --project # only project-level Claude Code
seshat uninstall --dry-run             # preview
```

---

## Upgrading

Schema migrations are auto-applied on first DB open. Major releases
that break the schema are called out in
[`CHANGELOG.md`](../CHANGELOG.md) with the recovery command
(typically: delete the per-project `.db` under
`~/.local/share/seshat/repos/<project>.db` and rerun `seshat scan`).

## See also

- [README](../README.md) — overview, quick start, comparison with
  alternatives
- [Roadmap](../_bmad-output/planning-artifacts/roadmap.md) — what's
  next
- [Competitive analysis](research/competitive-analysis-2026-03-30.md)
  — full review of adjacent code-intelligence MCP tools
