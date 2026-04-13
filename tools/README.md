# Seshat MCP Dev Tools

Tools for manually testing, debugging, and validating the Seshat MCP server.
Useful for catching regressions that unit tests miss — call real tools against a
real database and inspect the actual output.

## Prerequisites

All approaches require a scanned project database. Run `seshat scan <path>` at
least once before using these tools.

---

## Option 1: mcptools — recommended for interactive debugging

A purpose-built CLI for MCP servers. Best for quick exploration during
development.

**Install (once):**

```bash
brew tap f/mcptools && brew install mcp
```

**List all registered tools:**

```bash
mcp tools seshat serve
mcp tools --format json seshat serve
```

**Call a specific tool:**

```bash
mcp call query_project_context --params '{}' seshat serve
mcp call query_code_pattern --params '{"query":"handleRequest","kind":"function"}' seshat serve
mcp call query_convention --params '{"topic":"error handling"}' seshat serve
mcp call query_dependencies --params '{"path":"crates/seshat-mcp/src/server.rs"}' seshat serve
mcp call validate_approach --params '{"description":"add error handler","approach_type":"new_feature"}' seshat serve
```

**Interactive REPL (explore freely):**

```bash
mcp shell seshat serve
# Inside the shell:
# > tools
# > query_code_pattern {"query":"handleRequest"}
# > Ctrl+D to exit
```

**Show MCP server logs (stderr) while calling:**

```bash
mcp call query_project_context --params '{}' --server-logs seshat serve
```

---

## Option 2: npx inspector — no install required

The official MCP Inspector from Anthropic. Works out of the box with Node.js.
Slightly more verbose syntax but zero setup.

**List tools:**

```bash
npx @modelcontextprotocol/inspector --cli seshat serve --method tools/list
```

**Call a tool:**

```bash
npx @modelcontextprotocol/inspector --cli seshat serve \
  --method tools/call \
  --tool-name query_code_pattern \
  --tool-arg query=handleRequest \
  --tool-arg kind=function
```

**Browser UI (interactive form-based exploration):**

```bash
npx @modelcontextprotocol/inspector seshat serve
# Opens http://localhost:6274 in your browser
```

---

## Option 3: mcp-smoke.py — recommended for regression testing

A thin Python script in this repo. Best for scripted smoke tests, capturing
baselines, and detecting regressions between commits.

**Install dependencies (once):**

```bash
pip install -r tools/requirements.txt
# or
pip install 'mcp[cli]>=1.0.0'
```

### List tools

```bash
python tools/mcp-smoke.py list -- seshat serve
```

### Call a tool

Parameters are passed as `key=value` pairs — no JSON quoting needed:

```bash
python tools/mcp-smoke.py call query_code_pattern query=handleRequest kind=function -- seshat serve
python tools/mcp-smoke.py call query_convention topic="error handling" -- seshat serve
python tools/mcp-smoke.py call query_dependencies path=crates/seshat-mcp/src/server.rs -- seshat serve
python tools/mcp-smoke.py call validate_approach description="add new handler" approach_type=new_feature -- seshat serve
python tools/mcp-smoke.py call remove_decision id=42 reason="superseded" -- seshat serve
```

Integer parameters (like `id`) are coerced automatically.

For the `examples` parameter in `record_decision` / `update_decision` (the only
nested field in the entire API), use `--params` to mix in raw JSON:

```bash
python tools/mcp-smoke.py call record_decision \
  description="Use anyhow for all errors" \
  nature=convention \
  --params '{"examples":[{"file":"crates/seshat-cli/src/error.rs","line":1}]}' \
  -- seshat serve
```

`key=value` and `--params` are merged; `--params` wins on conflicts.

### Run all smoke scenarios

Scenarios are defined in `tools/mcp-scenarios.json`. Each scenario calls a tool
and checks the response structure.

```bash
python tools/mcp-smoke.py smoke -- seshat serve
```

Exit code `0` if all pass, `1` if any fail.

### Capture a baseline

Save the current output of all scenarios to `tools/mcp-baseline.json`. Run this
on a known-good commit.

```bash
python tools/mcp-smoke.py baseline -- seshat serve
```

`mcp-baseline.json` is gitignored — it reflects your local database, not the
repo state.

### Detect regressions (diff)

Compare the current output against the saved baseline. Any structural change or
value drift is reported.

```bash
python tools/mcp-smoke.py diff -- seshat serve
```

Exit code `0` if no regressions, `1` if any found. Use this before merging a
change to verify nothing broke.

### Typical regression workflow

```bash
# Before making changes — capture known-good state
python tools/mcp-smoke.py baseline -- seshat serve

# ... make code changes, rebuild ...
cargo build

# After changes — check for regressions
python tools/mcp-smoke.py diff -- seshat serve
```

### Pointing at a different project

All three tools accept any path to `seshat serve`. Pass extra flags after `--`:

```bash
python tools/mcp-smoke.py smoke -- seshat serve --repo /path/to/other-project
mcp tools seshat serve --repo /path/to/other-project
```

---

## Adding smoke scenarios

Edit `tools/mcp-scenarios.json`. Each scenario:

```json
{
  "name": "unique_scenario_name",
  "tool": "tool_name",
  "params": { "key": "value" },
  "assert": {
    "has_keys": ["status", "data"],
    "not_empty": ["data"],
    "equals": { "status": "success" }
  }
}
```

Available assertions:

| Key | Type | Description |
|---|---|---|
| `has_keys` | `string[]` | Top-level keys that must be present. Supports dot-notation: `"data.results"` |
| `not_empty` | `string[]` | Keys whose value must be truthy / non-empty |
| `equals` | `{key: value}` | Exact value matches |
| `allow_error` | `bool` | If `true`, a `status: "error"` response is not a failure on its own — useful for testing server behaviour on invalid inputs |
