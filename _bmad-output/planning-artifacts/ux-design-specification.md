---
stepsCompleted: [cli-ux]
inputDocuments: [prd.md, architecture.md]
---

# UX Design Specification — Seshat

**Author:** Kostik
**Date:** 2026-03-22

---

Seshat has two UX surfaces: Developer-facing CLI and Agent-facing MCP. This document specifies both.

## CLI UX Design

### Design Principles

- **Factual, not subjective** — dry facts, no personality/commentary in output
- **Progressive disclosure** — default shows summary, `--verbose` shows details
- **Zero-config first experience** — every command works without flags
- **Consistent format** — all messages follow `{level}: {message}` pattern
- **Color-coded** — errors red, warnings yellow, info default. Respect `NO_COLOR`.
- **Copy-paste ready** — config snippets, commands in "Next Steps" are directly usable

### Verbosity Levels

| Flag | Shows |
|------|-------|
| `--quiet` | Errors + final summary only |
| (default) | Errors + warnings + summary + key findings |
| `--verbose` | Everything + skipped files + detector details + timing |

### Data Directory

Database and cache files stored in XDG-compliant data directory (via `dirs` crate):
- macOS: `~/Library/Application Support/seshat/repos/`
- Linux: `~/.local/share/seshat/repos/`
- Windows: `%APPDATA%\seshat\repos\`

DB file naming: `{project-name}-{path-hash}.db` to avoid collisions.

---

### Command: `seshat scan <path>`

**Purpose:** First contact with Seshat. Scans project, builds knowledge graph, shows what was discovered. This is Seshat's business card.

**Two-phase progress:**

```
$ seshat scan ./my-project

  seshat v0.1.0

  Discovering files...  2,847 found
  Scanning ████████████████████████████████████ 2,847/2,847 [00:12]
```

Phase 1: fast file discovery (walkdir). Phase 2: Tree-sitter parsing with known total for accurate progress bar.

**Full output:**

```
$ seshat scan ./my-project

  seshat v0.1.0

  Discovering files...  2,847 found
  Scanning ████████████████████████████████████ 2,847/2,847 [00:12]

  ── Project Overview ──────────────────────────────────────────

  Languages     TypeScript 72% ▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░  1,204 files
                Python     24% ▓▓▓▓▓░░░░░░░░░░░░░░░    412 files
                Shell       4% ▓░░░░░░░░░░░░░░░░░░░░     38 files

  Modules       34 detected
  Dependencies  127 packages (98 npm, 29 pip)

  ── Conventions Detected (23) ─────────────────────────────────

  ● 15 high confidence (>85%)
  ◐ 6  medium confidence (50-85%)
  ○ 2  low confidence (<50%)

  Top findings:
    ● Import grouping: stdlib → external → internal     93%
    ● Error handling: custom AppError with thiserror     91%
    ● Logging: tracing with structured spans             89%
    ● Naming: snake_case files, PascalCase types         88%
    ◐ Barrel exports from index.ts                       67%
    ◐ Constructor dependency injection                   62%

  ── Submodules ────────────────────────────────────────────────

    frontend/ → TypeScript project (1,204 files)

  ── Next Steps ────────────────────────────────────────────────

    Run  seshat review                to validate detected conventions
    Run  seshat serve                 to start MCP server
    Run  seshat init                  to generate MCP config

  23 conventions detected. Run seshat review to validate.

  Database: ~/.local/share/seshat/repos/my-project.db (12.4 MB)
```

**With `--verbose`:**

Additional sections shown:
```
  ── Skipped Files (3) ─────────────────────────────────────────

    warn: src/legacy/old_module.js — parse error (unexpected token)
    warn: vendor/minified.js — file too large (2.1 MB > 512 KB limit)
    warn: data/binary.dat — unsupported file type

  ── Detector Details ──────────────────────────────────────────

    dependency_usage    412 files analyzed   18 findings   23ms
    imports             1,616 files analyzed 47 findings   45ms
    error_handling      1,616 files analyzed 12 findings   31ms
    naming              2,847 files analyzed 23 findings   67ms
    ...

  ── Timing ────────────────────────────────────────────────────

    Discovery:     0.8s
    Parsing:       8.2s (parallel, 8 cores)
    Detection:     2.1s
    Storage:       1.3s
    Total:        12.4s
```

---

### Command: `seshat review`

**Purpose:** Interactive convention validation. Triples as onboarding, calibration, and measurement.

**TUI Layout:**

```
┌─ Seshat Convention Review ───────────────────────── 1/23 ─┐
│                                                            │
│  Import grouping: stdlib → external → internal             │
│                                                            │
│  Nature: Convention    Confidence: 93%    Weight: Strong    │
│                                                            │
│  Example (src/services/auth.ts:1):                         │
│  ┌────────────────────────────────────────────────────┐    │
│  │ import { readFile } from 'fs';                     │    │
│  │ import axios from 'axios';                         │    │
│  │ import { AuthService } from '../services';         │    │
│  └────────────────────────────────────────────────────┘    │
│                                                            │
│  Found in: 47/50 files (94% adoption)                      │
│                                                            │
├────────────────────────────────────────────────────────────┤
│  [y] Confirm   [n] Reject   [p] Partial   [s] Skip        │
│  [↑↓] Navigate   [/] Search   [q] Finish                  │
└────────────────────────────────────────────────────────────┘
```

**Key bindings:**
| Key | Action |
|-----|--------|
| `y` | Confirm convention — promote to Strong weight |
| `n` | Reject — demote to Observation or remove |
| `p` | Partial — mark as partially correct, keep current weight |
| `s` | Skip — no change, move to next |
| `↑` / `↓` | Navigate between conventions |
| `/` | Open search/filter — type keyword to filter conventions |
| `q` | Finish review — show summary |

**Search mode:**

```
┌─ Seshat Convention Review ───────────────── /import█  ─┐
│                                                         │
│  Filtered: 3 of 23 conventions match "import"           │
│                                                         │
│  > Import grouping: stdlib → external → internal   93%  │
│    Barrel exports from index.ts                    67%  │
│    Type-only imports separated                     45%  │
│                                                         │
├─────────────────────────────────────────────────────────┤
│  [Enter] Select   [Esc] Clear filter   [q] Finish      │
└─────────────────────────────────────────────────────────┘
```

**Review complete:**

```
  ── Review Complete ───────────────────────────────────────────

    ✓ Confirmed   19
    ✗ Rejected     3
    ~ Partial      1
    ⊘ Skipped      0

    Precision: 82.6%
    Status: ✓ Seshat is calibrated and ready to use

    Knowledge graph updated.
    Confirmed conventions promoted to Strong.
    Rejected items demoted to Observation.
```

If precision < 70%:
```
    Precision: 58.3%
    Status: ⚠ Low precision. Seshat may not be reliable for this project.
            Consider filing an issue with project details.
```

---

### Command: `seshat serve`

**Purpose:** Start MCP server. Long-running background process.

**Startup output:**

```
$ seshat serve

  seshat v0.1.0

  Loading repos:
    ✓ my-project (main) — 23 conventions, 2,847 files
    ✓ my-project::frontend (main) — 18 conventions, 1,204 files

  Watcher: active (hot tier + warm tier)
  MCP server: listening
    stdio:  enabled
    http:   http://localhost:39271

  Ready. Press Ctrl+C to stop.
```

**Graceful shutdown:**

```
  ^C
  info: Shutting down...
  info: Flushing pending updates...
  info: MCP server stopped.
  info: Seshat stopped. Uptime: 2h 14m.
```

---

### Command: `seshat status`

**Purpose:** Show state of indexed projects and server.

```
$ seshat status

  seshat v0.1.0

  ── Indexed Projects ──────────────────────────────────────────

  my-project        main       2,847 files   23 conventions   12.4 MB
    └─ frontend/    main       1,204 files   18 conventions    8.1 MB
  seshat            feat/scan    847 files   12 conventions    4.2 MB

  ── Watcher ───────────────────────────────────────────────────

    Status: active
    Hot tier:  142 updates since start
    Warm tier: last recalculation 18s ago

  ── Server ────────────────────────────────────────────────────

    MCP: listening (stdio + http://localhost:39271)
    Uptime: 2h 14m
    Tool calls: 847 (avg 127ms)
```

If not serving:
```
  ── Server ────────────────────────────────────────────────────

    MCP: not running. Run `seshat serve` to start.
```

---

### Command: `seshat init [client]`

**Purpose:** Generate MCP configuration for AI coding clients.

**Auto-detect mode (no argument):**

```
$ seshat init

  Detected AI coding clients in PATH:

    ✓ claude — Claude Code
    ✓ opencode — OpenCode

  ── Claude Code ───────────────────────────────────────────────

  Add to ~/Library/Application Support/Claude/claude_desktop_config.json:

  ┌─────────────────────────────────────────────────────────────┐
  │ {                                                           │
  │   "mcpServers": {                                           │
  │     "seshat": {                                             │
  │       "command": "seshat",                                  │
  │       "args": ["serve", "--repo", "/Users/kostik/my-proj"]  │
  │     }                                                       │
  │   }                                                         │
  │ }                                                           │
  └─────────────────────────────────────────────────────────────┘

  ── OpenCode ──────────────────────────────────────────────────

  Add to .opencode/config.json:

  ┌─────────────────────────────────────────────────────────────┐
  │ {                                                           │
  │   "mcpServers": {                                           │
  │     "seshat": {                                             │
  │       "command": "seshat",                                  │
  │       "args": ["serve", "--repo", "/Users/kostik/my-proj"]  │
  │     }                                                       │
  │   }                                                         │
  │ }                                                           │
  └─────────────────────────────────────────────────────────────┘

  Tip: Run from your project directory for auto-detected paths.
```

**Explicit client:**

```
$ seshat init cursor

  ── Cursor ────────────────────────────────────────────────────

  Add to Cursor MCP settings:
  ...
```

**No clients found:**

```
$ seshat init

  No AI coding clients detected in PATH.

  Supported clients: claude-code, opencode, cursor
  Run `seshat init <client>` to generate config for a specific client.
```

---

### Command: `seshat --version`

```
$ seshat --version
seshat 0.1.0 (a3b4c5d6)
```

---

### Error Output Pattern

All errors follow consistent format. Color-coded when terminal supports it.

```
  error: Directory not found: /nonexistent

  hint: Check the path and try again.
  hint: Run `seshat scan --help` for usage.
```

```
  error: No scanned projects found.

  hint: Run `seshat scan <path>` first to index a project.
```

```
  error: Unknown client: vscode

  hint: Supported clients: claude-code, opencode, cursor
  hint: Run `seshat init --help` for usage.
```

**Warning during scan (inline):**

```
  warn: Skipped src/legacy/old_module.js (parse error)
```

---

## MCP Response UX (Agent-Facing)

### Envelope Format (ADR-9)

Every tool response:

```json
{
  "status": "success",
  "tool": "query_convention",
  "repo": "/Users/kostik/my-project",
  "branch": "main",
  "scope": "root",
  "duration_ms": 47,
  "data": { },
  "metadata": { }
}
```

Error:

```json
{
  "status": "error",
  "tool": "query_convention",
  "repo": "/Users/kostik/my-project",
  "error": {
    "code": "REPO_NOT_SCANNED",
    "message": "Repository has not been scanned. Run `seshat scan` first.",
    "suggestion": "seshat scan /Users/kostik/my-project"
  }
}
```

### Tool: `query_project_context`

```json
{
  "status": "success",
  "tool": "query_project_context",
  "repo": "/Users/kostik/my-project",
  "branch": "main",
  "scope": "root",
  "duration_ms": 23,
  "data": {
    "languages": [
      {"name": "TypeScript", "percentage": 72, "file_count": 1204},
      {"name": "Python", "percentage": 24, "file_count": 412},
      {"name": "Shell", "percentage": 4, "file_count": 38}
    ],
    "modules": [
      {"path": "src/services/", "file_count": 12, "primary_language": "TypeScript"},
      {"path": "src/routes/", "file_count": 8, "primary_language": "TypeScript"},
      {"path": "scripts/", "file_count": 15, "primary_language": "Python"}
    ],
    "dependencies": {
      "total": 127,
      "by_domain": {
        "http_client": {"canonical": "axios", "adoption": 47, "alternatives": ["node-fetch"]},
        "logging": {"canonical": "pino", "adoption": 34, "alternatives": []},
        "testing": {"canonical": "vitest", "adoption": 28, "alternatives": ["jest"]},
        "validation": {"canonical": "zod", "adoption": 19, "alternatives": []}
      }
    },
    "submodules": [
      {"path": "frontend/", "languages": ["TypeScript"], "file_count": 1204}
    ],
    "conventions_count": 23,
    "precision": 0.826
  },
  "metadata": {
    "last_scan": "2026-03-22T14:30:00Z",
    "files_indexed": 2847
  }
}
```

### Tool: `query_convention`

```json
{
  "status": "success",
  "tool": "query_convention",
  "repo": "/Users/kostik/my-project",
  "branch": "main",
  "scope": "root",
  "duration_ms": 31,
  "data": {
    "conventions": [
      {
        "id": "conv_import_grouping",
        "nature": "Convention",
        "weight": "Strong",
        "confidence": 0.93,
        "adoption": {"count": 47, "total": 50, "rate": 0.94},
        "description": "Imports grouped by category: stdlib first, then external packages, then internal modules. Separated by blank lines.",
        "source": "auto_detected",
        "user_confirmed": true,
        "examples": [
          {
            "file": "src/services/auth.ts",
            "line": 1,
            "end_line": 5,
            "snippet": "import { readFile } from 'fs';\n\nimport axios from 'axios';\nimport { z } from 'zod';\n\nimport { AuthService } from '../services';"
          }
        ]
      }
    ]
  },
  "metadata": {
    "query": "imports",
    "results_count": 1
  }
}
```

### Tool: `query_code_pattern`

```json
{
  "status": "success",
  "tool": "query_code_pattern",
  "repo": "/Users/kostik/my-project",
  "branch": "main",
  "scope": "root",
  "duration_ms": 89,
  "data": {
    "patterns": [
      {
        "description": "API endpoint with Fastify route handler, zod validation, and error handling",
        "file": "src/routes/users.ts",
        "line": 12,
        "end_line": 35,
        "snippet": "app.post('/api/v1/users', {\n  schema: {\n    body: CreateUserSchema,\n  },\n  handler: async (request, reply) => {\n    try {\n      const user = await userService.create(request.body);\n      return reply.code(201).send({ data: user });\n    } catch (err) {\n      throw new AppError('USER_CREATE_FAILED', err);\n    }\n  }\n});",
        "truncated": false
      }
    ],
    "existing_implementations": [
      {
        "description": "Rate limiter utility already exists",
        "file": "src/shared/http/rate-limiter.ts",
        "line": 1,
        "end_line": 18,
        "snippet": "export class RateLimiter { ... }",
        "used_by": 8,
        "truncated": true
      }
    ]
  },
  "metadata": {
    "query": "API endpoint",
    "search_type": "fts5",
    "patterns_count": 1,
    "existing_count": 1
  }
}
```

### Tool: `validate_approach`

```json
{
  "status": "success",
  "tool": "validate_approach",
  "repo": "/Users/kostik/my-project",
  "branch": "main",
  "scope": "root",
  "duration_ms": 156,
  "data": {
    "verdict": "warnings_found",
    "summary": "Found: 2 convention warning(s), 1 duplicate(s) — use existing implementation(s).",
    "rules": [],
    "contradictions": [],
    "duplicates": [
      {
        "severity": "do_not_recreate",
        "message": "Function `escape_quotes()` already exists",
        "existing": {
          "file": "src/shared/string_utils.py",
          "line": 23,
          "end_line": 31,
          "snippet": "def escape_quotes(text: str) -> str:\n    \"\"\"Escape single and double quotes.\"\"\"\n    return text.replace(\"'\", \"\\\\'\").replace('\"', '\\\\\"')"
        },
        "used_by": 14
      }
    ],
    "conventions": [
      {
        "convention_id": "conv_import_grouping",
        "severity": "should_fix",
        "message": "Imports should be grouped: stdlib → external → internal",
        "confidence": 0.93,
        "correct_example": {
          "file": "src/services/auth.ts",
          "line": 1,
          "snippet": "import { readFile } from 'fs';\n\nimport axios from 'axios';\n\nimport { AuthService } from '../services';"
        }
      },
      {
        "convention_id": "conv_error_handling",
        "severity": "should_fix",
        "message": "Use custom AppError type for error handling",
        "confidence": 0.91,
        "correct_example": {
          "file": "src/services/auth.ts",
          "line": 45,
          "snippet": "throw new AppError('AUTH_FAILED', { cause: err });"
        }
      }
    ],
    "decisions": [],
    "observations": []
  },
  "metadata": {
    "approach_length": 234,
    "checks_performed": 23,
    "issues_found": 3
  }
}
```

### Tool: `query_dependencies`

```json
{
  "status": "success",
  "tool": "query_dependencies",
  "repo": "/Users/kostik/my-project",
  "branch": "main",
  "scope": "root",
  "duration_ms": 42,
  "data": {
    "target": "src/shared/http/retry.ts",
    "dependents": [
      {"file": "src/services/api-client.ts", "line": 3, "import": "RetryWithBackoff"},
      {"file": "src/services/webhook.ts", "line": 5, "import": "RetryConfig"},
      {"file": "src/services/notification.ts", "line": 2, "import": "retryFetch"}
    ],
    "dependencies": [
      {"file": "src/shared/http/config.ts", "import": "HttpConfig"},
      {"file": "src/shared/logging.ts", "import": "logger"}
    ],
    "dependents_count": 8,
    "dependencies_count": 2,
    "blast_radius": "medium",
    "backward_compatibility_note": "8 modules depend on this. Changes must be backward-compatible or coordinated."
  },
  "metadata": {
    "query": "src/shared/http/retry.ts"
  }
}
```

### Input Validation Errors

```json
{
  "status": "error",
  "tool": "query_convention",
  "repo": "/Users/kostik/my-project",
  "error": {
    "code": "EMPTY_TOPIC",
    "message": "Topic parameter is required.",
    "suggestion": "Provide a topic like 'imports', 'error_handling', 'logging', 'naming'."
  }
}
```

```json
{
  "status": "error",
  "tool": "query_dependencies",
  "repo": "/Users/kostik/my-project",
  "error": {
    "code": "TARGET_NOT_FOUND",
    "message": "File or module not found: src/nonexistent.ts",
    "suggestion": "Check the file path. Use query_project_context to see available modules."
  }
}
```
