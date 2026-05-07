# Seshat

Convention-aware project intelligence for AI agents.

Seshat builds and maintains a per-project knowledge graph of conventions,
patterns, and architectural decisions, and exposes it to AI agents via an MCP
server. Agents query Seshat before writing code so generated changes match
the project's existing style and rules.

This README is a quick-start. The full design lives in
`_bmad-output/planning-artifacts/`.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/KSDaemon/seshat/main/install.sh | sh
```

Or from source:

```bash
cargo install --path crates/seshat-bin
```

## Quick start

```bash
seshat init              # set up MCP integration with Claude Code / Cursor / etc.
seshat scan              # scan the current project
seshat serve             # run as a long-lived MCP server (auto-invoked by clients)
seshat review            # interactive TUI to confirm/reject auto-detected conventions
```

`seshat serve` and `seshat review` automatically detect git branch changes
and incremental commits since the last scan, and refresh the database
before serving / opening the TUI. In non-git directories, the freshness
checks are skipped silently and Seshat operates as a single-branch project
named `main`.

## CLI Reference

### Top-level commands

| Command | Purpose |
|---|---|
| `seshat init` | Configure MCP integration with detected AI clients |
| `seshat scan [path]` | Scan a project and persist its knowledge graph |
| `seshat serve [path]` | Run as MCP server (stdio); auto-detects HEAD changes |
| `seshat review [path]` | Interactive TUI to triage auto-detected conventions |
| `seshat decisions <subcommand>` | Manage project-wide user decisions |
| `seshat uninstall` | Remove Seshat configuration from AI clients |

Run `seshat <command> --help` for full flag documentation.

### `seshat decisions`

Decisions are project-wide records of approve / reject / partial / recorded
outcomes for conventions. They survive branch deletion and are the source
of truth for the review queue's exclusion filter — once a convention has a
decision in any state, the review TUI no longer surfaces it on any branch.

```text
seshat decisions list   [--state STATE] [--branch BRANCH] [--format table|json]
seshat decisions forget <HASH> [--yes]
seshat decisions export <FILE>
seshat decisions import <FILE> [--strict]
```

#### `decisions list`

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

#### `decisions forget`

Remove a decision so the underlying convention re-enters the review queue
on the next scan.

```bash
seshat decisions forget abcd1234            # prompts for confirmation
seshat decisions forget abcd1234 --yes      # non-interactive (scripts / CI)
```

The hash argument accepts either the full `description_hash` or an
unambiguous prefix of at least 4 characters. Ambiguous prefixes report the
matching short hashes so you can lengthen by hand.

#### `decisions export` and `decisions import`

Round-trip decisions across machines or back up before a destructive
operation.

```bash
seshat decisions export decisions.json
seshat decisions import decisions.json            # silent latest-wins on conflicts
seshat decisions import decisions.json --strict   # fail (no writes) on any conflict
```

The export shape matches `decisions list --format json`. Import UPSERTs by
`description_hash`; on conflict the row with the larger `decided_at` wins
silently. `--strict` aborts before any write if the input contains a hash
that already exists in the local DB, listing every conflicting hash so you
can resolve them by hand.

## Configuration

Copy `seshat.example.toml` to `seshat.toml` (project-local) or
`$XDG_CONFIG_HOME/seshat/seshat.toml` (user-global). Every key has a
default, so an empty file is valid.

## Upgrading

Schema migrations are auto-applied on first DB open. Major releases that
break the schema are called out in `CHANGELOG.md` with the recovery
command (typically: delete the per-project `.db` under
`~/.local/share/seshat/repos/<project>.db` and rerun `seshat scan`).

## License

MIT — see `LICENSE`.
