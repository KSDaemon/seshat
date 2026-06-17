# PRD: `validate_approach` — Retrieval, Not Verdict

## Introduction

**Type:** Fix (breaking — pre-1.0, no backward compatibility)

`validate_approach` is a deterministic MCP tool: it runs an FTS5 keyword search
over the project knowledge graph. It has no LLM, no reasoning, and never sees the
code the agent is about to write. Yet today it returns an evaluative **verdict**
(`approved` / `info_only` / `warnings_found` / `rules_violated`), a boolean
**`ready`** gate, a **`severity: "must_fix"`** label on rules, and a
judgment-laden **`what_would_help`** list.

This is a category error: the tool conflates **retrieval** ("which graph records
share keywords with this description?") with **judgment** ("does this plan comply
with the rules?"). It cannot make the second call — it has neither the input (the
future code) nor the mechanism (reasoning). The verdict therefore over-claims.

**Receipts (production call-log, `~/.seshat/call-log.jsonl`, 32 calls):**

- `rules_violated`: 18, `warnings_found`: 12, `info_only`: 2, `approved`: **0**
- `ready: false`: **29 / 32**

Wildly different tasks ("bump version to 1.0.0", "reword a system prompt", "add
per-user credentials") all return `rules_violated` / `ready: false` because some
rule incidentally shares ≥2 tokens. The result is **alarm fatigue**: when ~90% of
responses are a red light, the agent learns to ignore the signal — defeating the
tool's purpose.

This work strips the judgment layer entirely. `validate_approach` becomes an
honest retrieval tool: it returns the relevant graph records plus neutral counts,
and the **procedural guidance** tells the agent to read them, judge applicability
itself, apply what fits, and dig deeper or correct stale records as needed. The
agent — the only participant with both the code and the reasoning — becomes the
judge.

## Goals

- Remove every evaluative output: `verdict`, `ready`, `severity`/`must_fix`,
  `what_would_help`, and the verdict-branched `next_steps`.
- Return **raw section counts**, not a scalar label. No descriptive scalar either.
- When nothing matches, say so plainly ("No matching rules or conventions found —
  proceed using your own judgment"), so silence reads as permission, not failure.
- Keep returning **enough content inline to act** (description + 1 evidence snippet
  + `description_hash`), at current caps — no extra MCP round-trips for the common
  case.
- Rewrite the procedural guidance (tool description, `SKILL.md`, CLAUDE.md snippet)
  to instruct the agent: **read → judge applicability → apply → query more → update
  stale records.**
- Preserve the retrieval-quality machinery (token extraction, stop-words, the
  `MIN_RULE_RELEVANCE_TOKENS` gate) unchanged — it is ranking, not judgment.
- Keep telemetry working by logging section counts instead of `verdict`/`ready`.

## User Stories

### US-001: Strip the judgment layer from the graph engine
**Description:** As a maintainer, I want `validate_approach` in `seshat-graph` to
compute only retrieval results, so the engine stops asserting conclusions it cannot
justify.

**File:** `crates/seshat-graph/src/validate_approach.rs`

**Acceptance Criteria:**
- [ ] `compute_verdict` function and the `verdict` field removed.
- [ ] `ready` field and its gate (`verdict != "rules_violated" && !has_stale_conventions`) removed.
- [ ] `build_what_would_help` function and the `what_would_help` field removed.
- [ ] Stale-convention detection (`has_stale_conventions`, `LOW_CONFIDENCE_THRESHOLD_PCT` usage in the gate) removed — low confidence is already visible to the agent via `confidence_pct` on each convention.
- [ ] Retrieval path unchanged: `significant_tokens`, `STOP_WORDS`, `MIN_RULE_RELEVANCE_TOKENS` rule-relevance gate, bucket partitioning, contradiction and duplicate search all behave exactly as before.
- [ ] Per-section caps (10/section, 1 evidence snippet/convention) and `truncated` flag unchanged.
- [ ] `cargo test -p seshat-graph` passes (tests updated — see US-006).

### US-002: Rename `rules` → `relevant_rules` and de-judge the rule struct
**Description:** As an agent, I want the rules section named for what it is
(relevant, not violated) and addressable, so I can act on it and correct it.

**File:** `crates/seshat-graph/src/validate_approach.rs`

**Acceptance Criteria:**
- [ ] `ValidateApproachData.rules` field renamed to `relevant_rules`.
- [ ] Struct `RuleViolation` renamed to `RelevantRule`.
- [ ] `severity` field removed from the rule struct (no more `"must_fix"`).
- [ ] `description_hash` field added to `RelevantRule`, populated from the source `ConventionResult.description_hash` (so the agent can `update_decision`/`remove_decision` a stale rule).
- [ ] `evidence` (optional single snippet) retained, populated as today.
- [ ] `rules_from_conventions` helper updated to the new struct/fields.

### US-003: Neutral count-based summary with an explicit empty case
**Description:** As an agent, I want a factual one-line summary of what was found,
so I can see at a glance whether there is anything to read — without a verdict.

**File:** `crates/seshat-graph/src/validate_approach.rs`

**Acceptance Criteria:**
- [ ] `build_summary` rewritten to contain only counts, no evaluative words (no "verdict", "approved", "violated", "ready").
- [ ] Non-empty case, e.g.: `"Found 5 relevant rules, 3 conventions, 0 contradictions, 2 duplicates matching your description."` (counts cover relevant_rules, conventions, decisions, observations, contradictions, duplicates).
- [ ] Empty case (all sections empty): `"No matching rules or conventions found — proceed using your own judgment."`
- [ ] `summary` field retained on `ValidateApproachData`; `verdict`/`ready`/`what_would_help` are gone.

### US-004: Handler returns static procedural guidance
**Description:** As an agent, I want the response's `next_steps` to tell me how to
use the records honestly, identical regardless of what was found, so I am never
told a finding "violates" my plan.

**File:** `crates/seshat-mcp/src/tools/validate_approach.rs`

**Acceptance Criteria:**
- [ ] The `match data.verdict` block (lines ~103-128) removed.
- [ ] `next_steps` is a single static, procedural list (same for every response), conveying: "These are graph records whose keywords overlap your description — not a verdict on your plan. Read each relevant rule and convention, decide whether it applies to what you're about to write, and bring your plan into line with the ones that do. For detail use `query_convention(topic)` / `query_code_pattern` / `query_dependencies`. If a rule is stale or you disagree with a recorded decision, `update_decision` / `remove_decision` so future sessions inherit the correction."
- [ ] The existing duplicate hint (`if duplicate_count > 0 → "Consider reusing existing code patterns…"`) retained — it is non-evaluative.
- [ ] Handler tests updated: assertions on `verdict`/`ready` removed; assert the new `relevant_rules` field, `summary`, and presence of static `next_steps`.
- [ ] `cargo test -p seshat-mcp` passes.

### US-005: Rewrite the tool description (MCP schema)
**Description:** As an agent reading the tool catalog, I want the description to
promise retrieval, not a verdict, so I form correct expectations before calling.

**File:** `crates/seshat-mcp/src/server.rs` (the `validate_approach` `#[tool(description=...)]`, ~line 641)

**Acceptance Criteria:**
- [ ] All mention of "verdict (approved/info_only/warnings_found/rules_violated)", "evidence gating (ready: true/false)", and "must-fix violations" removed.
- [ ] New description states it returns relevant graph records (relevant rules, conventions, decisions, duplicates, contradictions) whose keywords overlap the description — material to check the plan against, not a judgment of the plan.
- [ ] Keeps the "use BEFORE writing code" intent and the follow-up pointers (`query_code_pattern`, `query_dependencies`).

### US-006: Telemetry logs section counts, not verdict/ready
**Description:** As a maintainer, I want the call-log to keep meaningful metrics so
I can prove the redesign reduced noise.

**Files:** `crates/seshat-mcp/src/call_logger.rs`, `crates/seshat-mcp/src/call_logger_keys.rs`

**Acceptance Criteria:**
- [ ] `validate_approach_result` logs counts: `{relevant_rules, conventions, decisions, observations, duplicates, contradictions}`.
- [ ] `verdict` and `ready` keys removed from the logged result for this tool.
- [ ] Existing `.jsonl` history files are left untouched (no migration).
- [ ] `cargo test` for the logger (if any) passes; the field is still emitted as valid JSON.

### US-007: Rewrite the prompting in SKILL.md
**Description:** As an agent loading the seshat skill, I want step 4 to teach the
honest workflow, so I stop waiting for a `ready: true`.

**File:** `skills/seshat/SKILL.md` (block "4. Before writing", lines ~46-59)

**Acceptance Criteria:**
- [ ] Line ~50 ("Returns: `approved`/`warnings_found`/`rules_violated` + `ready: true/false`") replaced with a retrieval-framed summary ("Returns relevant rules, conventions, decisions, duplicates — material to check your plan against").
- [ ] Line ~51 ("If `ready: false` — address `what_would_help` before proceeding") replaced with the procedural guidance: read → judge applicability → apply → query more → update stale records.
- [ ] `file_context` paragraph (duplicate blast-radius enrichment) retained.
- [ ] No remaining references to `verdict`, `ready`, `what_would_help`, or `must_fix` anywhere in `SKILL.md`.

### US-008: Update the CLAUDE.md snippet (rules/seshat.md)
**Description:** As a maintainer, I want the injected CLAUDE.md snippet to match the
new contract, so `seshat install` propagates correct guidance.

**File:** `rules/seshat.md` (canonical source; symlinked as `crates/seshat-cli/embedded/seshat.md`)

**Acceptance Criteria:**
- [ ] The `validate_approach` trigger row in the "Before Any Code Action" table and the "NEVER write code without `validate_approach`" rule are retained (the before-code mandate stays).
- [ ] Any wording that implies waiting for an approval/verdict is reframed to "check your plan against the returned rules and conventions".
- [ ] No references to `verdict`, `ready`, `what_would_help`, or `must_fix`.
- [ ] `crates/seshat-cli/embedded/seshat.md` still resolves (symlink intact) so `include_str!` picks up the change at build time.

## Functional Requirements

- FR-1: `validate_approach` MUST NOT return `verdict`, `ready`, `what_would_help`, or any `severity`/`must_fix` field.
- FR-2: The response MUST include the sections `relevant_rules`, `conventions`, `decisions`, `observations`, `duplicates`, `contradictions`, and the `truncated` flag.
- FR-3: Each `relevant_rules[]` entry MUST carry `description`, optional `evidence` (one snippet), and `description_hash`.
- FR-4: The response MUST include a neutral `summary` string: counts when matches exist; an explicit "nothing found, proceed using your own judgment" message when empty.
- FR-5: `next_steps` MUST be static and procedural (identical for all responses), framing the output as retrieval and instructing read → judge applicability → apply → query-more → update-stale.
- FR-6: The retrieval algorithm (tokenization, stop-words, `MIN_RULE_RELEVANCE_TOKENS` gate, partitioning, duplicate/contradiction search, caps, `truncated`) MUST be unchanged.
- FR-7: The tool description, `SKILL.md`, and `rules/seshat.md` MUST contain no evaluative verdict/ready/must_fix language.
- FR-8: Telemetry MUST log section counts in place of `verdict`/`ready`.

## Non-Goals (Out of Scope)

- No change to the retrieval/ranking logic, scoring, stop-word list, or relevance gate. (If it is found to surface noise, that is a separate ticket.)
- No backward compatibility / deprecation shims — pre-1.0; fields are removed outright.
- No changes to other tools (`query_convention`, `query_code_pattern`, etc.) beyond what cross-references demand.
- No new MCP tools and no new parameters (the unused `approach_type` stays as-is — removing it is a separate decision).
- No migration of historical `.jsonl` call-logs.
- No edit to the user's personal global `~/.claude/CLAUDE.md` (it is regenerated by `seshat install` from `rules/seshat.md`).

## Technical Considerations

- **Single source of truth for snippets:** `crates/seshat-cli/embedded/{seshat.md,SKILL.md}` are symlinks to `rules/seshat.md` and `skills/seshat/SKILL.md`; `instructions.rs` pulls them via `include_str!`. Edit the canonical files only.
- **Breaking change marking:** removing response fields is a breaking change to the MCP contract. Per project convention, mark the commit breaking in the message itself (`fix!:` / `feat!:` or a `BREAKING CHANGE:` footer), not just in CHANGELOG — release-plz only inspects commit messages.
- **`description_hash` source:** rule entries originate from `ConventionResult` (the `rule`-weighted bucket), which already carries `description_hash`; thread it into `RelevantRule` rather than recomputing.
- **Test surface:** the existing tests in `validate_approach.rs` and `tools/validate_approach.rs` assert `verdict`/`ready`/`must_fix` heavily — they must be rewritten to assert the new shape (presence of `relevant_rules`, correct counts in `summary`, empty-case message, static `next_steps`). Keep the regression tests for the relevance gate (phantom-rule, single-token, code-prose) — that logic is unchanged and still valuable.

## Success Metrics

- After redesign, `validate_approach` responses contain zero evaluative fields (verified by schema/test).
- In subsequent real usage, the call-log no longer carries `ready`/`verdict`; section-count distribution is visible instead.
- Qualitative: the agent reads and reasons about returned records rather than reacting to a red/green light (no more ~90% `ready:false` to ignore).

## Open Questions

- Commit/release classification: treat as `fix!` (correcting misleading behavior) vs `feat!` (contract change)? Both bump appropriately if marked breaking; leaning `fix!`. Confirm at commit time.
- Should the unused `approach_type` parameter be removed in a follow-up, now that we're touching the contract? (Out of scope here; flagged.)
