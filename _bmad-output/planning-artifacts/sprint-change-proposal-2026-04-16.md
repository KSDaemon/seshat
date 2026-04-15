# Sprint Change Proposal — 2026-04-16

**Trigger:** Dogfooding analysis + competitive research (MegaMemory, codebase-memory-mcp,
code-review-graph, axon, SocratiCode) revealed three gaps not covered by existing epics:

1. `seshat init` does not write agent instructions (AGENTS.md / CLAUDE.md / SKILL.md / hooks)
2. Epic 10 (File Watcher) is fully implemented but not marked COMPLETED in epics.md
3. Git worktree support is missing — seshat has no concept of worktree directories,
   branch detection is hardcoded to `"main"`, and `find_git_root` fails for worktrees
   where `.git` is a file rather than a directory
4. New MCP capability: `map_diff_impact` — maps uncommitted git changes to affected symbols
5. Lifecycle commands: `seshat update` (version check) and `seshat uninstall` (clean removal)
6. Auto-scan on first MCP call — if project not yet scanned, seshat should scan automatically
   rather than returning an error, enabling true zero-config experience for new users

**Change scope:** Moderate — new stories added to existing epics, no PRD goals affected.

---

> **Update 2026-04-16 (added during party mode):** Gap 6 added — auto-scan on first MCP call.

---

## Section 1: Issue Summary

### Gap 1 — Agent Instructions Not Written by `seshat init`

Call log analysis shows seshat MCP tools are called only at session start
(`query_project_context` once) and rarely during active coding. Root cause:
no AGENTS.md / CLAUDE.md / SKILL.md / hooks written at install time.
All three leading competitors (MegaMemory, codebase-memory-mcp, code-review-graph)
write agent instruction files as part of their install command.

**Artifacts created during analysis:**
- `rules/seshat.md` — compact AGENTS.md content with idempotency markers
- `skills/seshat/SKILL.md` — full reference skill for on-demand loading
- `rules/hooks/seshat-session-start` — soft SessionStart hook (exit 0)
- `rules/hooks/seshat-pre-tool` — soft PreToolUse hook, 1 nudge/session (exit 0)

### Gap 2 — Epic 10 Implemented but Not Marked Complete

`seshat-watcher` crate: 1,176 lines across 3 modules, 15 tests.
- Hot tier: `notify-debouncer-full`, 500ms debounce, re-parse → update IR
- Warm tier: 30s interval, full convention recalculation
- Bulk rescan: >N events in 2s window → full `scan_project`
- Watcher integrated in `seshat serve`, launched as background tokio task
All Story 10.1/10.2/10.3 ACs are met. Status update required only.

### Gap 3 — Git Worktree Support Missing (Epic 11 prerequisite)

Current state:
- `BranchRepository` with `switch_branch`, `create_snapshot` — fully implemented in storage
- `find_git_root` walks up checking `.git` existence — but `.git` is a FILE in worktrees
- `orchestrator.rs` hardcodes `BranchId::from("main")` at 8 call sites
- Real git branch is never read; `get_current_branch()` returns whatever is in the DB

Worktree scenario:
```
/projects/myapp/           ← main worktree, branch: main
  .git/                    ← directory
  .seshat/seshat.db        ← full index

/projects/myapp-feat1/     ← worktree, branch: feature/foo
  .git                     ← FILE containing: "gitdir: ../.git/worktrees/feat1"
  (no .seshat/)            ← no index, seshat does nothing useful
```

Required behavior:
1. Detect worktree (`.git` is file, not directory)
2. Parse `gitdir:` → resolve canonical `.git` dir → find main repo root + DB
3. Read actual branch name from `HEAD` or `gitdir` path
4. Re-use main repo DB, switch to worktree branch context
5. Incremental scan of worktree-specific changes
6. Watcher works normally, knows which branch it's on

### Gap 4 — `map_diff_impact` MCP Tool Missing

No tool maps uncommitted git changes to affected symbols + conventions.
Axon has this, code-review-graph has it. Complements `validate_approach`
(describes plan) with runtime evidence (actual changes).

### Gap 6 — No Auto-Scan on First MCP Call

Current behavior: if user runs `seshat serve` in an unscanned repo, `resolve_serve_db`
returns an error and the server never starts. User must manually run `seshat scan` first.

Required behavior (zero-config experience):
1. `seshat serve` starts even if no DB exists
2. On first tool call → detect project root → launch `scan_project` in background tokio task
3. Return immediate response with `status: "scanning"`, `estimated_seconds: ~N`,
   and guidance: "Seshat is indexing this project. Call again in a few seconds."
4. Subsequent calls: if scan still running → return same "scanning" status
5. Once scan completes → watcher takes over, all tools work normally
6. For worktree: auto-scan the parent repo (not the worktree dir itself)

This matches the behavior of codebase-memory-mcp which auto-indexes on first tool use.

### Gap 5 — `seshat update` / `seshat uninstall` Missing

All three competitors have version checking on startup and clean uninstall.
Without `seshat uninstall`, removal requires manual editing of 4+ config files.

---

## Section 2: Impact Analysis

### Epic Impact

| Epic | Impact |
|---|---|
| Epic 9 (Init) COMPLETED | Add Story 9.2 (agent instructions), 9.3 (update), 9.4 (uninstall) |
| Epic 10 (Watcher) | Mark **[COMPLETED]** — fully implemented |
| Epic 11 (Branch-Aware) | Expand Story 11.1 to include worktree detection and DB resolution |
| Epic 7 (Advanced MCP Tools) | Add Story 7.4 (`map_diff_impact` tool) |

### PRD Impact: None

All changes are additive. No MVP goals modified. No requirements removed.

### Architecture Impact

- `crates/seshat-cli/src/db.rs` — `find_git_root` must handle worktree `.git` file
- `crates/seshat-cli/src/init.rs` — add instruction writing, hooks, skill deployment
- `crates/seshat-scanner/src/orchestrator.rs` — replace `BranchId::from("main")` with detected branch
- `crates/seshat-cli/src/serve.rs` — detect actual git branch via `gix`, pass to orchestrator
- New: `crates/seshat-cli/src/update.rs` — version check via crates.io API
- New: `crates/seshat-cli/src/uninstall.rs` — inverse of init.rs

---

## Section 3: Recommended Approach

**Direct Adjustment** — add stories to existing epics, no replan needed.

Sequence:
1. Mark Epic 10 COMPLETED (no code needed)
2. Story 9.2 (agent instructions) — next after current work
3. Story 11.1 expanded (worktree + branch detection) — then Epic 11
4. Story 9.3/9.4 (update/uninstall) — can be parallel or after 11
5. Story 7.4 (map_diff_impact) — after Epic 11

---

## Section 4: Detailed Change Proposals

### Story 9.2: Agent Instructions in `seshat init` [NEW]

**As a developer,**
I want `seshat init` to write Seshat usage instructions into my AI agent's config,
So that my AI agent knows when and how to use Seshat tools during coding sessions.

**Acceptance Criteria:**

**Given** `seshat init` (any scope/target combination)
**When** MCP config is written for an agent
**Then** also write/append `rules/seshat.md` content to the agent's instruction file
**And** use idempotency markers `<!-- seshat:start -->` / `<!-- seshat:end -->`
**And** append if no markers found, replace between markers if found
**And** target files: AGENTS.md (OpenCode, Codex), CLAUDE.md (Claude Code, Claude Desktop),
  `.cursorrules` (Cursor), GEMINI.md (Gemini CLI)
**And** write `skills/seshat/SKILL.md` to `~/.claude/skills/seshat/SKILL.md` (Claude Code)
  and `~/.config/opencode/skills/seshat/SKILL.md` (OpenCode)
**And** install hooks for Claude Code: copy `rules/hooks/seshat-session-start` to
  `~/.claude/hooks/seshat-session-start` and register in `~/.claude/settings.json`
  as `SessionStart` on startup/resume/clear/compact matchers
**And** install hooks for Claude Code: copy `rules/hooks/seshat-pre-tool` to
  `~/.claude/hooks/seshat-pre-tool` and register as `PreToolUse` on Grep/Glob/Read/Search
**And** `--dry-run` shows all planned writes without executing
**And** `--skip-instructions` flag skips instruction/skill/hook writing (MCP only)
**And** all file content embedded in binary via `include_str!()` from `rules/` and `skills/`

**Implementation files:**
- `rules/seshat.md` ✅ created
- `skills/seshat/SKILL.md` ✅ created
- `rules/hooks/seshat-session-start` ✅ created
- `rules/hooks/seshat-pre-tool` ✅ created
- `crates/seshat-cli/src/init.rs` — add instruction writing logic

---

### Story 9.3: `seshat update` — Version Check [NEW]

**As a developer,**
I want Seshat to notify me when a newer version is available,
So that I don't unknowingly run stale tooling with outdated MCP schemas.

**Acceptance Criteria:**

**Given** `seshat serve` starts
**When** cached version check is older than 24h (or missing)
**Then** background task fetches `https://crates.io/api/v1/crates/seshat` (non-blocking)
**And** result cached in `metadata` table with TTL timestamp
**And** if newer version found: print notice line at startup and in first MCP response
  `_notice` field: `"Seshat {current} → {latest} available. Run: cargo install seshat"`
**And** if network unavailable: silently skip, no error
**And** `seshat update` CLI command: explicit check + print result + exit 0

**Implementation files:**
- New `crates/seshat-cli/src/update.rs`
- `crates/seshat-cli/src/serve.rs` — spawn background update check
- `crates/seshat-mcp/src/envelope.rs` — add optional `_notice` field to response

---

### Story 9.4: `seshat uninstall` — Clean Removal [NEW]

**As a developer,**
I want `seshat uninstall` to cleanly remove all Seshat configuration,
So that I can remove Seshat without manually hunting through config files.

**Acceptance Criteria:**

**Given** `seshat uninstall [client] [--global | --project] [--dry-run]`
**Then** detect same clients as `seshat init`
**And** remove `seshat` entry from MCP config (JSON patch inverse)
**And** remove `<!-- seshat:start -->...<!-- seshat:end -->` block from instruction files
**And** remove `~/.claude/skills/seshat/` directory
**And** remove seshat hooks from `~/.claude/settings.json`
**And** remove hook scripts from `~/.claude/hooks/seshat-*`
**And** does NOT remove the binary or `.seshat/*.db` files
**And** `--dry-run` shows all planned removals without executing
**And** confirms each action with `[y/N]` (same UX as init)

**Implementation files:**
- New `crates/seshat-cli/src/uninstall.rs`
- `crates/seshat-cli/src/args.rs` — add `Uninstall` subcommand

---

### Story 9.5: Auto-Scan on First MCP Call [NEW]

**As a developer,**
I want Seshat to automatically scan my project on first use,
So that I get zero-config experience — no manual `seshat scan` required.

**Acceptance Criteria:**

**Given** `seshat serve` starts in a directory with no existing DB
**Then** server starts successfully (no error)
**And** creates empty in-memory state sufficient to accept MCP connections

**Given** AI agent calls any Seshat MCP tool (e.g. `query_project_context`)
**When** project is not yet scanned
**Then** run `scan_project` synchronously (blocking) before responding
**And** once scan completes: return normal tool response
**And** include in `metadata`: `{ "auto_scanned": true, "first_run": true }`
**And** watcher starts automatically after scan completes
**And** if project exceeds `auto_scan_limit` (default: 50,000 files): return error
  `"Project too large for auto-scan. Run: seshat scan"` instead of blocking
**Note:** Blocking scan is the correct model (matches codebase-memory-mcp `index_repository`
  behavior). MCP clients typically have 30–120s timeout which covers most projects.
  No polling/streaming needed — agent simply waits for the response.

**Given** AI agent is in a git worktree directory
**When** project is not yet scanned
**Then** detect parent repo root (via worktree `.git` file resolution)
**And** scan the parent repo (not the worktree directory)
**And** return scanning status referencing parent repo name

**Given** scan fails (parse errors, permission denied)
**Then** return error with actionable message: `"Scan failed: {reason}. Try: seshat scan --verbose"`

**Implementation files:**
- `crates/seshat-cli/src/serve.rs` — handle missing DB: start without DB, spawn auto-scan task
- `crates/seshat-mcp/src/server.rs` — add scanning state, return scanning response when not ready
- `crates/seshat-mcp/src/envelope.rs` — add `status: "scanning"` response variant
- `crates/seshat-cli/src/db.rs` — `resolve_serve_db_or_auto` variant that doesn't error on missing DB

---

### Story 11.1 (EXPANDED): Git Worktree + Branch Detection

**Original AC kept. Added:**

**Given** `seshat serve` starts in a git worktree directory
**When** `.git` is a file (not directory) containing `gitdir: <path>`
**Then** parse `gitdir:` to resolve canonical `.git` directory
**And** walk up from resolved `.git` to find main repo root
**And** locate and use main repo's `.seshat/seshat.db`
**And** read actual branch name from worktree's `HEAD` file
**And** pass real `BranchId` (not `"main"`) to orchestrator and watcher

**Given** `seshat serve` starts in any git repo (worktree or main)
**When** determining branch
**Then** read actual branch via `gix::discover` → `HEAD` reference
**And** replace all `BranchId::from("main")` hardcodes in orchestrator.rs

**Integration test requirements:**
```
test: worktree_auto_init
  setup: create git repo, full scan + seshat serve
  action: git worktree add ../feat-worktree feature/foo
  action: start seshat serve in ../feat-worktree
  assert: detects main repo DB
  assert: branch_id = "feature/foo" (not "main")
  assert: incremental scan completes < 5s
  assert: MCP query_project_context returns correct branch

test: worktree_isolated_conventions
  setup: same as above
  action: add a file with different pattern in worktree
  action: warm tier fires
  assert: convention in worktree branch does not appear in main branch

test: multiple_worktrees_same_db
  setup: main repo + 2 worktrees (feat-a, feat-b)
  assert: all three seshat serve instances use same .db
  assert: each has distinct branch context
  assert: no data corruption between branches
```

**Implementation files:**
- `crates/seshat-cli/src/db.rs` — `find_git_root_or_worktree()` replacing `find_git_root()`
- `crates/seshat-cli/src/serve.rs` — detect real branch via `gix`
- `crates/seshat-scanner/src/orchestrator.rs` — replace 8x `BranchId::from("main")`

---

### Story 7.4: `map_diff_impact` MCP Tool [NEW]

**As an AI agent,**
I want to call `map_diff_impact()` before committing or during code review,
So that I understand which conventions and dependents are at risk from current changes.

**Acceptance Criteria:**

**Given** a project with uncommitted changes
**When** `map_diff_impact()` called (no arguments required)
**Then** run `git diff --name-only HEAD` → list of changed files
**And** for each changed file: load IR → extract exported symbols
**And** for each symbol: query dependents (blast radius)
**And** return: `{ changed_files, affected_symbols, convention_risks, blast_radius_summary }`
**And** `convention_risks`: conventions whose files are among changed set (may be affected)
**And** `blast_radius_summary`: total dependents count, risk level (low/medium/high)

**Optional parameters:**
- `staged_only: bool` — diff only staged changes (default: false, includes unstaged)
- `base: string` — diff against specific ref (default: HEAD)

**Response example:**
```json
{
  "tool": "map_diff_impact",
  "changed_files": ["src/parser/mod.rs", "src/ir.rs"],
  "affected_symbols": [
    { "name": "collect_calls_bfs", "file": "src/parser/mod.rs",
      "dependent_count": 4, "blast_radius": "medium" }
  ],
  "convention_risks": [
    { "topic": "error handling", "confidence": 0.87,
      "note": "src/ir.rs participates in this convention" }
  ],
  "blast_radius_summary": { "total_dependents": 12, "risk": "medium" },
  "metadata": { "next_steps": ["review affected_symbols before committing",
    "call validate_approach if adding new patterns"] }
}
```

**Implementation files:**
- `crates/seshat-graph/src/diff_impact.rs` — new module
- `crates/seshat-mcp/src/server.rs` — register tool
- Uses `gix` (already a dependency) for git diff

---

## Section 5: Implementation Handoff

**Scope:** Moderate — new stories, no architectural replan.

| Story | Effort | Who | When |
|---|---|---|---|
| Epic 10 → COMPLETED | 0 (status update) | SM | Immediately |
| Story 9.2 (agent instructions) | ~3 days | Dev | Next sprint |
| Story 11.1 expanded (worktree) | ~4 days | Dev | After 9.2 |
| Story 9.3 (update check) | ~1 day | Dev | Parallel with 11 |
| Story 9.4 (uninstall) | ~2 days | Dev | After 11.1 |
| Story 9.5 (auto-scan on first call) | ~2 days | Dev | After 9.2, before 11 |
| Story 7.4 (map_diff_impact) | ~3 days | Dev | After 11 |

**Success criteria:**
- `seshat init` writes AGENTS.md section + skill + hooks on first run
- `seshat init` second run: idempotent, no duplicate sections
- Git worktree: `seshat serve` auto-detects main repo DB, correct branch
- 3 integration tests for worktree scenarios pass
- `seshat update` prints version notice when newer available
- `seshat uninstall` cleanly removes all config without touching DB/binary
- `map_diff_impact()` returns affected symbols + convention risks for uncommitted changes
