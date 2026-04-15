# Story 9.2: Agent Instructions in `seshat init`

Status: review

## Story

As a **developer**,
I want `seshat init` to write Seshat usage instructions into my AI agent's config files,
so that my AI agent knows when and how to use Seshat MCP tools during coding sessions
without any manual configuration.

## Acceptance Criteria

1. **Given** `seshat init` runs (any scope/target combination, any client)
   **When** MCP config is written for an agent
   **Then** also write/append `rules/seshat.md` content to the agent's instruction file
   using idempotency markers `<!-- seshat:start -->` / `<!-- seshat:end -->`

2. **Given** the agent instruction file already contains seshat markers
   **When** `seshat init` runs again
   **Then** replace content between markers (no duplicate sections)

3. **Given** the agent instruction file does NOT contain seshat markers
   **When** `seshat init` runs
   **Then** append the seshat section to end of file (create file if needed)

4. **Given** `seshat init` is run for Claude Code
   **Then** write instruction section to `~/.claude/CLAUDE.md` (global)
   **And** install `skills/seshat/SKILL.md` to `~/.claude/skills/seshat/SKILL.md`
   **And** install `rules/hooks/seshat-session-start` to `~/.claude/hooks/seshat-session-start`
   **And** install `rules/hooks/seshat-pre-tool` to `~/.claude/hooks/seshat-pre-tool`
   **And** register hooks in `~/.claude/settings.json`:
     - `SessionStart` with matchers: `startup`, `resume`, `clear`, `compact`
     - `PreToolUse` with matcher: `Grep|Glob|Read|Search`

5. **Given** `seshat init` is run for OpenCode
   **Then** write instruction section to `~/.config/opencode/AGENTS.md` (global scope)
   **And** install `skills/seshat/SKILL.md` to `~/.config/opencode/skills/seshat/SKILL.md`

6. **Given** `seshat init` is run for Claude Desktop or Cursor
   **Then** write instruction section to appropriate file (see table below)
   **And** NO hooks or skills installed (not supported by those clients)

7. **Given** `--dry-run` flag
   **Then** show all planned writes/registrations without executing any

8. **Given** `--skip-instructions` flag
   **Then** write only MCP config entry (original behavior), skip all instruction/skill/hook writing

9. **All file content** must be embedded in the binary via `include_str!()` from
   `rules/seshat.md`, `skills/seshat/SKILL.md`, `rules/hooks/seshat-session-start`,
   `rules/hooks/seshat-pre-tool` — no filesystem reads at runtime

10. **Given** instruction file cannot be written (permissions, locked)
    **Then** show error with path and suggestion, continue with remaining operations (non-fatal)

## Target Files per Client

| Client | Instruction file (global) | Skill dir | Hooks |
|--------|--------------------------|-----------|-------|
| Claude Code | `~/.claude/CLAUDE.md` | `~/.claude/skills/seshat/` | `~/.claude/hooks/` + `~/.claude/settings.json` |
| OpenCode | `~/.config/opencode/AGENTS.md` | `~/.config/opencode/skills/seshat/` | — |
| Claude Desktop | `~/.config/seshat/CLAUDE.md` or skip | — | — |
| Cursor | skip (`.cursorrules` is project-level, out of scope for now) | — | — |

> **Decision:** For initial implementation, focus on Claude Code + OpenCode (the two
> fully supported clients). Claude Desktop and Cursor instruction writing can be added
> in a follow-up. AC-6 covers these with "no-op" behavior.

## Tasks / Subtasks

- [ ] **Task 1: Embed source files in binary** (AC: 9)
  - [ ] Add `include_str!()` constants in `crates/seshat-cli/src/instructions.rs` (new file):
    ```rust
    pub const AGENTS_MD_CONTENT: &str = include_str!("../../../rules/seshat.md");
    pub const SKILL_MD_CONTENT: &str = include_str!("../../../skills/seshat/SKILL.md");
    pub const HOOK_SESSION_START: &str = include_str!("../../../rules/hooks/seshat-session-start");
    pub const HOOK_PRE_TOOL: &str = include_str!("../../../rules/hooks/seshat-pre-tool");
    ```
  - [ ] Verify paths resolve correctly from `crates/seshat-cli/` — adjust `../` depth as needed
  - [ ] Add `pub mod instructions;` to `crates/seshat-cli/src/lib.rs`

- [ ] **Task 2: Implement `upsert_instructions()` function** (AC: 1, 2, 3, 10)
  - [ ] Create `crates/seshat-cli/src/instructions.rs` with:
    ```rust
    const MARKER_START: &str = "<!-- seshat:start -->";
    const MARKER_END: &str = "<!-- seshat:end -->";

    pub fn upsert_instructions(path: &Path, content: &str, dry_run: bool) -> Result<UpsertResult, CliError>
    ```
  - [ ] `UpsertResult` enum: `Created`, `Appended`, `Updated`, `DryRun`
  - [ ] Algorithm:
    1. If file doesn't exist → create with `{MARKER_START}\n{content}\n{MARKER_END}\n`
    2. If file exists but no markers → append `\n{MARKER_START}\n{content}\n{MARKER_END}\n`
    3. If file exists with markers → replace between markers (inclusive)
    4. If `dry_run` → return `DryRun` without writing
  - [ ] Handle permissions error → return `CliError::IoWithPath` (non-fatal in caller)

- [ ] **Task 3: Implement `install_skill()` function** (AC: 4, 5)
  - [ ] In `instructions.rs`:
    ```rust
    pub fn install_skill(target_dir: &Path, content: &str, dry_run: bool) -> Result<SkillResult, CliError>
    ```
  - [ ] Creates `{target_dir}/SKILL.md` (creates parent dirs if needed)
  - [ ] Idempotent: always overwrites (skill content is versioned via binary release)

- [ ] **Task 4: Implement `install_hooks_claude_code()` function** (AC: 4)
  - [ ] In `instructions.rs`:
    ```rust
    pub fn install_hooks_claude_code(hooks_dir: &Path, settings_path: &Path, dry_run: bool) -> Result<(), CliError>
    ```
  - [ ] Copy `HOOK_SESSION_START` to `{hooks_dir}/seshat-session-start` with `chmod 0o755`
  - [ ] Copy `HOOK_PRE_TOOL` to `{hooks_dir}/seshat-pre-tool` with `chmod 0o755`
  - [ ] Register in `settings.json`:
    - Read existing `settings.json` (create `{}` if missing)
    - Merge hooks under `"hooks"` key (idempotent: check if already present by command path)
    - Structure:
      ```json
      {
        "hooks": {
          "PreToolUse": [{
            "matcher": "Grep|Glob|Read|Search",
            "hooks": [{"type": "command", "command": "~/.claude/hooks/seshat-pre-tool"}]
          }],
          "SessionStart": [
            {"matcher": "startup", "hooks": [{"type": "command", "command": "~/.claude/hooks/seshat-session-start"}]},
            {"matcher": "resume",  "hooks": [{"type": "command", "command": "~/.claude/hooks/seshat-session-start"}]},
            {"matcher": "clear",   "hooks": [{"type": "command", "command": "~/.claude/hooks/seshat-session-start"}]},
            {"matcher": "compact", "hooks": [{"type": "command", "command": "~/.claude/hooks/seshat-session-start"}]}
          ]
        }
      }
      ```
    - Idempotency: if seshat hook commands already present → skip (no duplicate entries)
    - Write with `write_backup()` before modifying (reuse existing backup helper from `init.rs`)

- [ ] **Task 5: Wire into `run_init()`** (AC: 1–8)
  - [ ] Add `skip_instructions: bool` parameter to `run_init()` signature
  - [ ] Update `args.rs`: add `--skip-instructions` flag to `init` subcommand
  - [ ] After each successful MCP config write, call instruction writing:
    - For Claude Code: `upsert_instructions(~/.claude/CLAUDE.md, ...)` + `install_skill(~/.claude/skills/seshat/, ...)` + `install_hooks_claude_code(...)`
    - For OpenCode: `upsert_instructions(~/.config/opencode/AGENTS.md, ...)` + `install_skill(~/.config/opencode/skills/seshat/, ...)`
    - For others: skip (no-op)
  - [ ] In dry-run mode: pass `dry_run=true` to all instruction functions
  - [ ] Print results using existing `print_ok()` / `print_info()` / `print_error()` helpers:
    - `"  ✓ Instructions written to ~/.claude/CLAUDE.md"`
    - `"  ✓ Skill installed: ~/.claude/skills/seshat/SKILL.md"`
    - `"  ✓ Hooks registered in ~/.claude/settings.json"`

- [ ] **Task 6: Tests** (AC: 1–10)
  - [ ] Unit tests in `instructions.rs`:
    - `upsert_creates_new_file_when_absent`
    - `upsert_appends_when_no_markers`
    - `upsert_replaces_between_markers`
    - `upsert_idempotent_on_second_run` (run twice, assert single section)
    - `upsert_dry_run_does_not_write`
    - `install_skill_creates_dir_and_file`
    - `install_skill_overwrites_existing`
    - `install_hooks_creates_scripts_with_correct_permissions`
    - `install_hooks_registers_in_settings_json`
    - `install_hooks_idempotent_on_second_run`
    - `install_hooks_merges_with_existing_settings`
  - [ ] Integration test in `crates/seshat-cli/tests/`:
    - `init_writes_instructions_for_opencode`: mock opencode config, run `run_init`, assert AGENTS.md created with seshat markers + skill file created

## Dev Notes

### Existing patterns to reuse in `init.rs`

**`write_backup(path)`** — already implemented, use before modifying `settings.json`:
```rust
// init.rs line 417
pub fn write_backup(path: &Path) -> Result<PathBuf, CliError>
```

**`print_ok()` / `print_info()` / `print_error()`** — output helpers, use these:
```rust
// init.rs lines 528-542
fn print_ok(message: &str, color: bool)
fn print_info(message: &str, color: bool)
fn print_error(message: &str, color: bool)
```

**`ask_yn(prompt, dry_run)`** — confirmation prompt:
```rust
// init.rs line 549
fn ask_yn(prompt: &str, dry_run: bool) -> bool
```

**XDG paths** — OpenCode global config:
```rust
// config.rs — xdg_config_dir() or similar
// ~/.config/opencode/ already resolved in resolve_opencode_config()
```

**`find_git_root()`** — already in `db.rs`, imported in `init.rs` via `crate::db::find_git_root`

### `include_str!()` path resolution

`include_str!()` resolves relative to the source file location at **compile time**.
From `crates/seshat-cli/src/instructions.rs`:
```
../../../rules/seshat.md
```
resolves to `<workspace_root>/rules/seshat.md` ✅

Verify with:
```bash
realpath crates/seshat-cli/src/../../../rules/seshat.md
# → /Users/kostik/Projects/seshat/rules/seshat.md ✅
```

### Hook `settings.json` schema

Claude Code `settings.json` is at `~/.claude/settings.json` (separate from `.claude.json`).
It may already contain other hooks (e.g., from codebase-memory-mcp). Structure must be merged,
not replaced. Use `serde_json` Value merging. Example of existing content on this machine:
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep|Glob|Read|Search",
        "hooks": [{"type": "command", "command": "~/.claude/hooks/cbm-code-discovery-gate"}]
      }
    ]
  }
}
```

Idempotency check: before inserting a seshat hook entry into a `hooks` array, check if any
existing entry already has `"command": "~/.claude/hooks/seshat-session-start"` (or seshat-pre-tool).
If yes, skip. If no, push new entry into the array.

### File permissions for hooks

Hook scripts must be executable. Use:
```rust
use std::os::unix::fs::PermissionsExt;
std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
```
This is Unix-only — wrap in `#[cfg(unix)]`.

### `--skip-instructions` flag

Add to `args.rs` in the `Init` subcommand:
```rust
/// Skip writing agent instructions, skills, and hooks. Only write MCP config.
#[arg(long)]
skip_instructions: bool,
```

Then thread through `run_init(client, scope, dry_run, skip_instructions)`.

### Source files (already created)

All four source files exist and are ready:
- `rules/seshat.md` — compact AGENTS.md content (~430 chars, with markers)
- `skills/seshat/SKILL.md` — full reference (~3,900 chars)
- `rules/hooks/seshat-session-start` — executable bash script
- `rules/hooks/seshat-pre-tool` — executable bash script (gate: 1 nudge/session via PPID)

### Crate dependencies

No new crates needed. Already available in `seshat-cli`:
- `serde_json` — for `settings.json` read/modify/write
- `std::fs` — for file operations
- `which` — already used for client detection

### Project Structure

```
crates/seshat-cli/src/
  instructions.rs          ← NEW (Task 1+2+3+4)
  init.rs                  ← MODIFY (Task 5): wire in + add skip_instructions param
  args.rs                  ← MODIFY (Task 5): add --skip-instructions flag
  lib.rs                   ← MODIFY: add pub mod instructions

rules/
  seshat.md                ← EXISTS (source for AGENTS_MD_CONTENT)
  hooks/
    seshat-session-start   ← EXISTS (source for HOOK_SESSION_START)
    seshat-pre-tool        ← EXISTS (source for HOOK_PRE_TOOL)

skills/
  seshat/
    SKILL.md               ← EXISTS (source for SKILL_MD_CONTENT)
```

### Testing approach

Unit tests should use `tempfile::tempdir()` for all file operations (already a dev-dependency
in seshat-cli). Integration test should mock the full init flow against a temp dir and verify
all written files contain correct content.

### What NOT to touch

- `patch_json_config()` — MCP config patching, no changes needed
- `handle_claude_code_via_cli()` — `claude mcp add` path, add instruction writing AFTER it
- `write_backup()` — reuse as-is
- `detect_clients()` — no changes
- Existing test suite in `init.rs` — must all pass unchanged

## Dev Agent Record

### Agent Model Used

anthropic/claude-sonnet-4-6

### Debug Log References

### Completion Notes List

### File List
- `crates/seshat-cli/src/instructions.rs` (new)
- `crates/seshat-cli/src/init.rs` (modified)
- `crates/seshat-cli/src/args.rs` (modified)
- `crates/seshat-cli/src/lib.rs` (modified)
