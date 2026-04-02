# Tech Spec: Smart DB Discovery for `seshat serve` + Forward-Compatible Tool Schemas

**Type:** Refinement / Bug Fix
**Epic:** 5 (MCP Server, Serve Command & Core Tools)
**Depends on:** All 12 Epic 5 user stories (completed)
**Estimated scope:** 7 files changed, ~200 lines added/modified

## Problem Statement

### Problem 1: Broken DB discovery logic

`seshat serve` currently picks the **most recently modified** `.db` file from
`$XDG_DATA_HOME/seshat/repos/`. This silently serves the wrong project when
multiple scanned projects exist. A developer in `~/Projects/walt-chat-backend/`
could get conventions from a completely different project.

### Problem 2: No explicit project selection

`Command::Serve` accepts only `--host` and `--port` — there is no way to
explicitly tell `seshat serve` which project to serve.

### Problem 3: Tool schemas lack `repo` and `scope` parameters

All 5 MCP tool request structs (`ProjectContextRequest`, `QueryConventionRequest`,
`RecordDecisionRequest`, `UpdateDecisionRequest`, `RemoveDecisionRequest`) have
no `repo` or `scope` fields. When Epic 6 adds multi-repo support, tool schemas
will need to change — breaking any AI agents that cached the schema.

Adding these as optional, ignored parameters now means zero schema changes
when multi-repo ships.

## Solution Design

### Task 1: Extract shared DB utilities into `crates/seshat-cli/src/db.rs`

**New file:** `crates/seshat-cli/src/db.rs`

Extract from `scan.rs::resolve_db_path()` and new helpers:

```rust
/// Get the XDG repos directory: $XDG_DATA_HOME/seshat/repos/
pub(crate) fn xdg_repos_dir() -> Result<PathBuf, CliError>;

/// Extract project name from the last path component.
pub(crate) fn project_name(path: &Path) -> String;

/// Resolve DB path for a project root directory.
/// Returns $XDG_DATA_HOME/seshat/repos/{project_name}.db
pub(crate) fn resolve_db_path(root: &Path) -> Result<PathBuf, CliError>;

/// Walk up from `from` to find the nearest `.git` directory.
/// Returns the parent of `.git` (the repository root).
pub(crate) fn find_git_root(from: &Path) -> Option<PathBuf>;

/// List all .db files in the repos directory.
pub(crate) fn list_available_projects(repos_dir: &Path) -> Result<Vec<PathBuf>, CliError>;
```

**Changes to `scan.rs`:** Remove private `resolve_db_path()`, replace with
`crate::db::resolve_db_path()`.

### Task 2: Add `repo` positional argument to `Command::Serve`

**File:** `crates/seshat-cli/src/args.rs`

```rust
/// Start the MCP server for AI agent connections.
Serve {
    /// Repository directory path or project name.
    /// Auto-detected from current working directory if omitted.
    repo: Option<PathBuf>,

    /// Host to bind the HTTP/SSE transport to (overrides config).
    #[arg(long)]
    host: Option<String>,

    /// Port for the HTTP/SSE transport (overrides config).
    #[arg(long)]
    port: Option<u16>,
},
```

Usage patterns:
```
seshat serve                              # auto-detect from cwd
seshat serve walt-chat-backend            # project name
seshat serve ~/Projects/walt-chat-backend # directory path
seshat serve --host 0.0.0.0 --port 8080  # auto-detect + custom transport
```

**File:** `crates/seshat-cli/src/lib.rs`

Update dispatch:
```rust
Command::Serve { repo, host, port } => serve::run_serve(repo.as_deref(), host, port),
```

### Task 3: Replace `discover_db()` with smart resolution

**File:** `crates/seshat-cli/src/serve.rs`

Replace `discover_db()` with `resolve_serve_db(explicit_repo: Option<&Path>)`.

**Priority chain:**

1. **Explicit `repo` argument** — if it's an existing directory, extract
   project name from it; otherwise treat as project name directly. Look for
   `{name}.db` in XDG repos dir. If not found → error "not scanned".

2. **Current working directory** — extract project name from `cwd`, look for
   `{name}.db`. If found → use it.

3. **Git root walk-up** — if cwd is a subdirectory of a project (e.g.
   `~/Projects/my-app/src/api/`), walk up to find `.git`, extract repo name
   from that directory. If found → use it.

4. **Single DB fallback** — if exactly one `.db` file exists in repos dir,
   use it unambiguously.

5. **Ambiguous / no projects** — error with list of available projects and
   usage hint.

**Error output for ambiguous case:**
```
error: could not determine which project to serve.

  Available scanned projects:
    • walt-chat-backend
    • seshat
    • my-other-project

  hint: run from the project directory, or specify:
        seshat serve <project-name>
        seshat serve <path-to-project>
```

**Update `run_serve` signature:**
```rust
pub fn run_serve(
    repo: Option<&Path>,
    host: Option<String>,
    port: Option<u16>,
) -> Result<(), CliError>
```

### Task 4: Add `repo` and `scope` to all 5 tool request schemas

**Files:**
- `crates/seshat-mcp/src/tools/project_context.rs`
- `crates/seshat-mcp/src/tools/query_convention.rs`
- `crates/seshat-mcp/src/tools/record_decision.rs`
- `crates/seshat-mcp/src/tools/update_decision.rs`
- `crates/seshat-mcp/src/tools/remove_decision.rs`

Add to **each** request struct:

```rust
/// Repository name or path. Auto-detected in single-repo mode.
/// Required in multi-repo daemon mode (Epic 6).
#[schemars(description = "Repository name. Auto-detected when server runs in project mode. Required in daemon mode.")]
pub repo: Option<String>,

/// Scope within the repository: 'root' (default) or a submodule name.
/// Reserved for submodule-aware queries (Epic 6).
#[schemars(description = "Scope: 'root' (default) or submodule name")]
pub scope: Option<String>,
```

**Handler behavior (Epic 5):** Both fields are **ignored** in all `handle()`
functions. The server's `repo_name` and `branch` are used as before.

**AI agent visibility:** These fields appear in `list_tools` JSON Schema,
so agents see them from day one. When Epic 6 ships, agents can start passing
`repo` without any tool schema change.

### Task 5: Update PRD and docs

**File:** `.ralph/tasks/prd-mcp-server-core-tools.md`
- US-003: Replace `--path` references with positional `repo` argument
- Note the priority chain for DB resolution
- Note forward-compatible `repo`/`scope` in tool schemas

**File:** `crates/seshat-cli/src/serve.rs`
- Update module-level doc comment

## Acceptance Criteria

- [ ] `seshat serve` auto-detects project from cwd (matching `{cwd_name}.db`)
- [ ] `seshat serve` walks up to git root when cwd is a subdirectory
- [ ] `seshat serve walt-chat-backend` resolves by project name
- [ ] `seshat serve ~/Projects/walt-chat-backend` resolves by directory path
- [ ] `seshat serve` with single DB in repos dir uses it automatically
- [ ] `seshat serve` with multiple DBs and no match shows available projects
- [ ] `seshat serve` with no DBs shows "run seshat scan first" hint
- [ ] All 5 tool request schemas include `repo: Option<String>` and `scope: Option<String>`
- [ ] `repo` and `scope` are visible in rmcp's `list_tools` JSON Schema output
- [ ] `repo` and `scope` are ignored in handler logic (Epic 5 single-repo mode)
- [ ] `scan.rs` uses shared `db::resolve_db_path()` instead of private copy
- [ ] All existing tests pass
- [ ] New unit tests for `resolve_serve_db()`, `find_git_root()`, `list_available_projects()`
- [ ] `cargo clippy` clean

## Files Changed

| File | Change |
|------|--------|
| `crates/seshat-cli/src/db.rs` | **NEW** — shared DB path utilities |
| `crates/seshat-cli/src/lib.rs` | Update `mod` declarations + Serve dispatch |
| `crates/seshat-cli/src/args.rs` | Add `repo: Option<PathBuf>` to Serve |
| `crates/seshat-cli/src/serve.rs` | Replace `discover_db()` with smart resolution |
| `crates/seshat-cli/src/scan.rs` | Use `crate::db::resolve_db_path()` |
| `crates/seshat-mcp/src/tools/project_context.rs` | Add `repo`, `scope` to request |
| `crates/seshat-mcp/src/tools/query_convention.rs` | Add `repo`, `scope` to request |
| `crates/seshat-mcp/src/tools/record_decision.rs` | Add `repo`, `scope` to request |
| `crates/seshat-mcp/src/tools/update_decision.rs` | Add `repo`, `scope` to request |
| `crates/seshat-mcp/src/tools/remove_decision.rs` | Add `repo`, `scope` to request |
| `.ralph/tasks/prd-mcp-server-core-tools.md` | Update US-003 with new discovery logic |

## Non-Goals

- Multi-repo daemon mode (`--daemon`) — Epic 6
- Actually using `repo`/`scope` parameters in tool handlers — Epic 6
- Submodule-aware scope routing — Epic 6
- HTTP/SSE transport multi-repo routing — Epic 6
