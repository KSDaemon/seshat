---
name: seshat
description: Use Seshat MCP tools BEFORE writing or modifying any code. Triggers on: implementing features, fixing bugs, refactoring, modifying functions, creating files, editing files, choosing patterns, adding dependencies, building components, making any change to the codebase.
---

# Seshat — Project Convention Intelligence

Seshat maintains a knowledge graph of this project's conventions, patterns,
and architectural decisions. Use it BEFORE writing code — it tells you how
this codebase works and validates your approach against established rules.

## Workflow

**1. Session start**
```
query_project_context()
```
Understand the stack, languages, modules, and top conventions.
Runs automatically via MCP on first connection.

**2. Before writing any new symbol (function / class / module / type / constant)**
```
query_code_pattern(query="<name or concept>")
```
Finds existing implementations with that name or similar intent.
Examples:
- `query_code_pattern(query="parse_config")` — before writing a config parser
- `query_code_pattern(query="retry")` — before writing retry logic
- `query_code_pattern(query="UserRepository")` — before writing a data access class

**3. Before choosing any pattern**
```
query_convention(topic="<area>")
```
Returns adopted patterns with confidence score and trend (rising/stable/declining).
Examples:
- `query_convention(topic="error handling")` — which error types, how propagated
- `query_convention(topic="logging")` — which logger, log levels, format
- `query_convention(topic="naming")` — camelCase, snake_case, PascalCase, file naming
- `query_convention(topic="testing")` — test framework, fixture style, assertion patterns

**4. Before writing (validate your plan)**
```
validate_approach(description="<what you plan to do>")
```
Returns: `approved` / `warnings_found` / `rules_violated` + `ready: true/false`.
If `ready: false` — address `what_would_help` before proceeding.
Examples:
- `validate_approach(description="add axios for HTTP calls")`
- `validate_approach(description="create a singleton DatabaseManager class")`
- `validate_approach(description="use console.log for debug output")`

**5. Before editing an existing file**
```
query_dependencies(path="<relative/file/path>")
```
Returns: direct dependencies, dependents, blast_radius (low/medium/high).
High blast radius = many things depend on this file, edit carefully.

**6. After discovering a new pattern not yet in the knowledge base**
```
record_decision(description="<pattern>", reason="<why>", category="<area>")
```
Persists the decision for future sessions — survives re-scans and context resets.
- `update_decision(id=<id>, description="<updated>")` — when a decision evolves
- `remove_decision(id=<id>, reason="<why>")` — when superseded; soft-deleted with audit trail

**7. Before committing or during code review**
```
map_diff_impact()
```
Maps uncommitted git changes to affected symbols, dependents, blast radius,
and convention risks in a single call. Helps assess the impact of your changes
before committing or raising a PR.

Returns:
- `changed_files` — list of changed files with status (modified/added/deleted/untracked/conflicted)
- `affected_symbols` — exports, public functions, and types touched, each with `dependent_count`, `dependents [{file, line}]`, and `blast_radius` (low/medium/high)
- `convention_risks` — conventions whose evidence files are being modified, with `topic`, `weight`, `adoption`, `is_golden_file`, and a human-readable `note`
- `blast_radius_summary` — aggregate `total_dependents`, `total_affected_symbols`, `total_changed_files`, `risk` (none/low/medium/high)
- `metadata.next_steps` — ranked, actionable suggestions

Parameters (all optional):
- `staged_only=true` — analyse only staged changes (`git diff --cached`); mutually exclusive with `base`
- `base="<ref>"` — compare against a branch, tag, or commit instead of HEAD (e.g. `"main"`, `"v1.2.3"`, `"origin/main"`)
- `repo_path="<path>"` — override the git root; defaults to the project root the server was started in; only needed for submodule analysis

Examples:
```
map_diff_impact()                          — all uncommitted changes vs HEAD
map_diff_impact(staged_only=true)          — staged changes only (pre-commit check)
map_diff_impact(base="main")               — everything on this branch vs main
map_diff_impact(base="origin/main")        — compare against remote main
map_diff_impact(repo_path="vendor/libfoo") — analyse a submodule
```

## All 9 Tools

| Tool | When to use |
|------|-------------|
| `query_project_context` | Session start — stack, modules, top conventions |
| `query_convention` | Before choosing any pattern — error handling, naming, logging, etc. |
| `query_code_pattern` | Before writing any new symbol — find existing implementations |
| `query_dependencies` | Before editing a file — understand blast radius |
| `validate_approach` | Before writing — verify plan against rules and conventions |
| `record_decision` | After discovering a pattern — persist institutional knowledge |
| `update_decision` | When a decision evolves — update reasoning or classification |
| `remove_decision` | When a decision is superseded — soft-delete with reason |
| `map_diff_impact` | Before committing or code review — assess change impact |

## Do NOT use Seshat for:
- Reading file contents (use file reading tools)
- Searching for string literals or config values (use text search)
- Running tests, builds, or compiling the project
