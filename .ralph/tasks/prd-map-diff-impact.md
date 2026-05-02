# PRD: map_diff_impact MCP Tool

## Introduction

**Type:** Feature

Add a new MCP tool `map_diff_impact` that allows AI agents to assess the impact of uncommitted
git changes before committing or during code review. The tool maps changed files to their exported
symbols, dependents, blast radius, and convention risks — all in a single call.

**Problem:** Today an AI agent must manually run `git diff`, then call `query_dependencies` for each
changed file, then call `query_convention` for each file — N×M calls. `map_diff_impact` reduces
this to one call, shortening the feedback loop and catching risky changes before they land.

**Competitive context:** Both Axon and code-review-graph provide pre-commit diff-to-dependency
mapping for AI agents.

## Goals

- Map uncommitted git changes to affected exported symbols with dependent counts and blast radius
- Identify which conventions are at risk because their evidence files are being modified
- Provide constructive `next_steps` — not stop-flags — so agents make informed decisions
- Handle edge cases gracefully: no changes, untracked files, deleted files, merge conflicts, detached HEAD
- Use batch operations to avoid O(N²) IR loads

## User Stories

### US-001: `DiffImpactData` types and `get_changed_files()`
**Description:** As a developer, I need data structures and git diff integration so the tool can
identify which files are changed and their status.

**Acceptance Criteria:**
- [ ] Define `ChangedFile`, `FileStatus` (Modified/Added/Deleted/Untracked/Conflicted), `AffectedSymbol`, `DependentRef`, `ConventionRisk`, `AdoptionSummary`, `BlastRadiusSummary`, `ImpactMetadata`, `DiffImpactData`, `DiffImpactRequest` in `crates/seshat-graph/src/diff_impact.rs`
- [ ] All structs derive `Serialize`
- [ ] `get_changed_files(repo_path, staged_only, base) -> Vec<ChangedFile>` uses `gix` to diff working tree vs HEAD / `--cached` / `base...HEAD`
- [ ] Detects merge conflict markers and sets `status: "conflicted"`
- [ ] Detects files not in IR as `status: "untracked"`
- [ ] Register `pub mod diff_impact;` in `crates/seshat-graph/src/lib.rs` with re-exports
- [ ] `cargo build --workspace` passes
- [ ] `cargo clippy --workspace` passes

### US-002: `query_dependencies_batch()` and `compute_affected_symbols()`
**Description:** As a developer, I need a batch dependency query so that multiple files'
dependents can be resolved in a single IR load.

**Acceptance Criteria:**
- [ ] Add `query_dependencies_batch(conn, branch_id, paths: &[String]) -> Vec<DependencyData>` to `crates/seshat-graph/src/dependencies.rs`
- [ ] Loads IR once, builds dependents index, returns results for all requested paths — O(N) instead of N×O(IR_load)
- [ ] `compute_affected_symbols(conn, branch_id, changed_files) -> Vec<AffectedSymbol>` extracts `exports` + `public functions` from each changed file's IR, calls `query_dependencies_batch`
- [ ] Each `AffectedSymbol` includes `name`, `file`, `kind`, `dependent_count`, `dependents: [{file, line}]`, `blast_radius`
- [ ] `cargo build --workspace` passes
- [ ] `cargo clippy --workspace` passes

### US-003: `compute_convention_risks()`
**Description:** As a developer, I need to identify which conventions are at risk because their
evidence files are being modified, with nuanced note generation for different scenarios.

**Acceptance Criteria:**
- [ ] SQL query uses `json_each(json_extract(ext_data, '$.evidence'))` to batch-match changed files against convention nodes
- [ ] Filters: only conventions where `weight IN ('rule','strong')` OR `adoption_count >= 3`
- [ ] Groups results by `(description, affected_file)` into `Vec<ConventionRisk>`
- [ ] **Standard file note:** `"{affected_file} contributes evidence to the '{description}' convention ({confidence_pct}% confidence, {adoption_count}/{total_count} files follow). Changing this file may reduce its convention compliance."`
- [ ] **Golden file note:** `"{affected_file} is a golden file for this convention — it has the highest compliance score in the project. If you intentionally evolve this pattern, consider calling record_decision afterwards to update the convention baseline."` — no WARNING, no stop-flag
- [ ] **Deleted file note:** `"{affected_file} was evidence for the '{description}' convention. After deletion, the convention's confidence may decrease."`
- [ ] Golden file status does NOT inflate `blast_radius_summary.risk`
- [ ] `cargo build --workspace` passes
- [ ] `cargo clippy --workspace` passes

### US-004: `map_diff_impact()` main function
**Description:** As a developer, I need the orchestrating function that ties all steps together and
generates summary + next_steps.

**Acceptance Criteria:**
- [ ] `map_diff_impact(conn, branch_id, repo_path, request) -> DiffImpactData` calls `get_changed_files` → `compute_affected_symbols` → `compute_convention_risks`
- [ ] `blast_radius_summary` aggregates: `total_dependents`, `total_affected_symbols`, `total_changed_files`, `risk` (none/low/medium/high based on max blast radius among affected symbols)
- [ ] `metadata.next_steps` includes constructive suggestions based on severity:
  - `"review affected_symbols with dependent_count >= 3 before committing"`
  - `"{file} is a golden file for '{topic}' — if intentionally changing the pattern, call record_decision to capture the new expectation"`
  - `"run test suite: the N dependents may break"`
  - `"deleted file {file} — verify no remaining imports"`
  - `"nothing to review"` (when 0 changes)
- [ ] `metadata.branch` includes current branch name
- [ ] `cargo build --workspace` passes
- [ ] `cargo clippy --workspace` passes

### US-005: MCP handler and server registration
**Description:** As a developer, I need the MCP handler layer and tool registration so the tool
is callable via the MCP protocol.

**Acceptance Criteria:**
- [ ] Create `crates/seshat-mcp/src/tools/diff_impact.rs` with `MapDiffImpactRequest` (staged_only, base, repo, scope, file_path)
- [ ] `handle(conn, repo_name, branch, repo_path, req) -> String` validates:
  - `staged_only` and `base` together → error `"staged_only and base are mutually exclusive"`
  - Not a git repo → error `"not a git repository"`
  - Detached HEAD → included as `note` (not error)
- [ ] Wraps result in `ResponseEnvelope::success` with metadata
- [ ] Register in `crates/seshat-mcp/src/tools/mod.rs`: `pub mod diff_impact;`
- [ ] Add `#[tool(description = "...")]` method in `server.rs` `#[tool_router]` block
- [ ] `impl_tool_request!(MapDiffImpactRequest);` in `server.rs`
- [ ] Add call logger match arm `"map_diff_impact"` → `diff_impact_result()` counting `changed_file_count`, `affected_symbol_count`, `convention_risk_count`, `blast_radius`
- [ ] Update `ServerHandler::get_info()` server instructions to list `map_diff_impact`
- [ ] `cargo build --workspace` passes
- [ ] `cargo clippy --workspace` passes

### US-006: Skills, rules, and hooks update
**Description:** As a product owner, I want agent guidance files updated so AI agents discover
and use `map_diff_impact` automatically at the right moments.

**Acceptance Criteria:**
- [ ] `skills/seshat/SKILL.md`: Add workflow step 7 «Before committing or during code review» → `map_diff_impact()`
- [ ] `skills/seshat/SKILL.md`: Add `map_diff_impact` row to the «All Tools» table
- [ ] `rules/seshat.md`: Add trigger row «Before committing or during code review» → `map_diff_impact()`
- [ ] `.config/opencode/AGENTS.md`: Add trigger row «Before committing or during code review» → `map_diff_impact()`
- [ ] `rules/hooks/seshat-session-start`: Add `map_diff_impact` to tool reminder list

### US-007: Tests — edge cases and correctness
**Description:** As a QA engineer, I need comprehensive test coverage for all edge cases so the
tool never panics and always returns correct, helpful data.

**Acceptance Criteria:**

- [ ] **No uncommitted changes** → `changed_files: []`, `blast_radius_summary.risk: "none"`, `next_steps: ["nothing to review"]`
- [ ] **1 modified file, 0 exported symbols** → `changed_files: [1]`, `affected_symbols: []`, `risk: "none"`
- [ ] **1 modified file, exported fn with 5 dependents** → `affected_symbols: [{dependent_count: 5, blast_radius: "medium"}], risk: "medium"`
- [ ] **Deleted file** → `status: "deleted"`, convention_risks note о потере evidence
- [ ] **Deleted file, no remaining imports** → `affected_symbols: []`
- [ ] **Untracked file (not in IR)** → `status: "untracked"`, исключён из affected_symbols и convention_risks
- [ ] **Merge conflict file** → `status: "conflicted"`, note `"resolve before analysis"`, исключён
- [ ] **File not in knowledge graph** → note `"not in knowledge graph — run seshat scan first"`
- [ ] **Golden file modified** → `convention_risks.is_golden_file: true`, note без WARNING
- [ ] **Golden file + 0 convention risks** → не влияет на общий `risk`
- [ ] **Convention confidence < 0.50** → не попадает в `convention_risks`
- [ ] **Convention weight = weak, adoption < 3** → не попадает в `convention_risks`
- [ ] **`staged_only = true`** → `git diff --cached`, только staged изменения
- [ ] **`staged_only + base` together** → ошибка `"staged_only and base are mutually exclusive"`
- [ ] **`base = "main"`** → `git diff main...HEAD`
- [ ] **No git repo** → ошибка `"not a git repository"`
- [ ] **Detached HEAD** → note + branch = commit hash, не ошибка
- [ ] **Empty project (0 files in IR)** → `affected_symbols: []`, `convention_risks: []`
- [ ] **Multiple changed files, overlapping symbols** → дедупликация символов (один символ — одна запись)
- [ ] **Binary file changed** → пропускается
- [ ] **Batch: 100+ changed files** → не падает, соблюдается `MAX_IR_FILES`
- [ ] `cargo test --workspace` passes

### US-008: Integration verification
**Description:** As a developer, I need to verify the full pipeline works end-to-end with a real
project and update documentation.

**Acceptance Criteria:**
- [ ] `cargo build --workspace` — компилируется без ошибок
- [ ] `cargo test --workspace` — все тесты проходят
- [ ] `cargo clippy --workspace` — без warnings
- [ ] Ручное тестирование на реальном проекте с uncommitted changes
- [ ] Обновить `_bmad-output/planning-artifacts/epics.md` — отметить Story 7.7 как реализованную

## Functional Requirements

- **FR-1:** Tool accepts optional parameters: `staged_only: bool` (default `false`), `base: string?` (default `null`), plus standard Seshat routing fields `repo`, `scope`, `file_path`
- **FR-2:** Runs `git diff --name-only HEAD` (or `--cached`, or `base...HEAD`) via `gix` to identify changed files
- **FR-3:** Classifies each changed file into status: `modified`, `added`, `deleted`, `untracked` (not in IR), `conflicted` (contains merge markers)
- **FR-4:** For each changed file: loads IR from `files_ir`, extracts exported symbols + public functions
- **FR-5:** For each exported symbol: queries dependent files and counts via `query_dependencies_batch` — one IR load for all files
- **FR-6:** Classifies blast radius per symbol: `low` (<3 dependents), `medium` (3–10), `high` (>10)
- **FR-7:** Queries `nodes` table via `json_each(json_extract(ext_data, '$.evidence'))` to find conventions whose evidence files are in the changed set
- **FR-8:** Filters convention risks to show only significant conventions: `weight IN ('rule','strong')` or `adoption_count >= 3`
- **FR-9:** Generates human-readable `note` for each convention risk, with distinct phrasing for standard files, golden files, and deleted files
- **FR-10:** Golden file convention risks use constructive tone: suggest `record_decision`, never use WARNING or stop-flags
- **FR-11:** Produces `blast_radius_summary` with total dependent count and overall risk level (`none`/`low`/`medium`/`high`)
- **FR-12:** Generates `metadata.next_steps` with ranked, actionable suggestions
- **FR-13:** Returns full response in standard Seshat `ResponseEnvelope` format via MCP protocol
- **FR-14:** Logs each call via `CallLogger` with summary stats (file count, symbol count, risk count, blast radius)
- **FR-15:** Validates mutual exclusivity of `staged_only` and `base`
- **FR-16:** Handles detached HEAD gracefully (note, not error)
- **FR-17:** Handles non-git-directory with clear error

## Non-Goals

- Not a commit-hook that blocks commits — purely advisory, callable at agent's discretion
- Does not analyze diff hunks or content-level changes (future enhancement)
- Does not compute transitive dependents (only direct)
- Does not auto-fix convention violations
- Does not replace `query_dependencies` or `query_convention` — it composes them

## Design Considerations

### Tool Description (for MCP registration)

```
Map uncommitted git changes to affected symbols, dependents, and convention risks.
Call BEFORE committing or during code review to understand which conventions and
dependents are at risk from current uncommitted changes. Returns changed_files,
affected_symbols (with blast_radius per symbol), convention_risks, and blast_radius_summary.
Optional: staged_only (diff --cached), base (compare against another branch).
```

### Response example

```json
{
  "status": "success",
  "data": {
    "tool": "map_diff_impact",
    "changed_files": [
      {"path": "src/parser/mod.rs", "status": "modified"},
      {"path": "src/ir.rs", "status": "modified"},
      {"path": "src/old_feature.rs", "status": "deleted"}
    ],
    "affected_symbols": [
      {
        "name": "collect_calls_bfs",
        "file": "src/parser/mod.rs",
        "kind": "function",
        "dependent_count": 4,
        "dependents": [
          {"file": "src/ir.rs", "line": 12},
          {"file": "src/main.rs", "line": 45}
        ],
        "blast_radius": "medium"
      },
      {
        "name": "IrFormat",
        "file": "src/ir.rs",
        "kind": "type",
        "dependent_count": 8,
        "dependents": [
          {"file": "src/serializer.rs", "line": 3},
          {"file": "src/cli.rs", "line": 56}
        ],
        "blast_radius": "medium"
      }
    ],
    "convention_risks": [
      {
        "topic": "error handling",
        "description": "Use Result<T, E> for all fallible functions; avoid panics",
        "confidence_pct": 87,
        "weight": "strong",
        "adoption": {"count": 45, "total": 52, "rate_pct": 87},
        "affected_file": "src/ir.rs",
        "is_golden_file": false,
        "note": "src/ir.rs contributes evidence to the 'error handling' convention (87% confidence, 45/52 files follow). Changing this file may reduce its convention compliance."
      },
      {
        "topic": "serialization",
        "description": "Use Serde derive macros for all public types",
        "confidence_pct": 92,
        "weight": "strong",
        "adoption": {"count": 38, "total": 41, "rate_pct": 93},
        "affected_file": "src/ir.rs",
        "is_golden_file": true,
        "note": "src/ir.rs is a golden file for this convention — it has the highest compliance score in the project. If you intentionally evolve this pattern, consider calling record_decision afterwards to update the convention baseline."
      }
    ],
    "blast_radius_summary": {
      "total_dependents": 14,
      "total_affected_symbols": 3,
      "total_changed_files": 3,
      "risk": "medium"
    },
    "metadata": {
      "branch": "feature/new-parser",
      "next_steps": [
        "review affected_symbols with dependent_count >= 3 before committing",
        "IrFormat touched with 8 dependents in src/serializer.rs, src/cli.rs — check for breaking changes",
        "src/ir.rs is a golden file for 'serialization' — if intentionally changing the pattern, call record_decision",
        "deleted file src/old_feature.rs — verify no remaining imports",
        "run test suite: the 14 dependents may break"
      ]
    }
  }
}
```

## Technical Considerations

### Dependencies
- `gix` — already a workspace dependency for branch listing and tree diff in `seshat-cli`
- `serde`, `serde_json` — already used project-wide
- No new external dependencies required

### Files to create
| File | Purpose |
|------|---------|
| `crates/seshat-graph/src/diff_impact.rs` | Core business logic |
| `crates/seshat-mcp/src/tools/diff_impact.rs` | MCP handler |

### Files to modify
| File | Change |
|------|--------|
| `crates/seshat-graph/src/lib.rs` | Add `pub mod diff_impact;` + re-exports |
| `crates/seshat-graph/src/dependencies.rs` | Add `query_dependencies_batch()` |
| `crates/seshat-mcp/src/tools/mod.rs` | Add `pub mod diff_impact;` |
| `crates/seshat-mcp/src/server.rs` | Register tool, add call logger arm, update instructions |
| `crates/seshat-mcp/src/call_logger.rs` | Add `diff_impact_result()` |
| `skills/seshat/SKILL.md` | Add workflow step + tool table row |
| `rules/seshat.md` | Add trigger row |
| `.config/opencode/AGENTS.md` | Add trigger row |
| `rules/hooks/seshat-session-start` | Add tool to reminder list |

### Architecture
Three layers matching existing tool pattern:
```
seshat-graph/src/diff_impact.rs   ← core (git diff via gix + IR + conventions)
seshat-mcp/src/tools/diff_impact.rs ← MCP handler (validation, envelope)
seshat-mcp/src/server.rs           ← registration #[tool(...)]
```

## Success Metrics

- AI agents discover and call `map_diff_impact` before commits (measured via call logger)
- Blast radius information prevents accidental breaking changes to heavily-depended-on symbols
- Convention risk notes lead to `record_decision` calls for intentional golden file changes
- Tool handles all 21 edge cases without panics or incorrect output

## Open Questions

- Should we add content-level diff analysis (which lines changed in each file) in a future PRD?
- Should `map_diff_impact` become part of a pre-commit hook (opt-in) rather than purely advisory?
- Should the tool cache convention risk lookups when called multiple times in the same session?
