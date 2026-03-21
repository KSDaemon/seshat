---
stepsCompleted: [step-01-init, step-02-discovery, step-02b-vision, step-02c-executive-summary, step-03-success, step-04-journeys]
inputDocuments: [product-brief-seshat-2026-03-16.md]
workflowType: 'prd'
documentCounts:
  briefs: 1
  research: 0
  brainstorming: 0
  projectDocs: 0
classification:
  projectType: developer_tool
  subType: dual-interface (Agent-facing MCP server + Developer-facing CLI)
  domain: Developer Infrastructure / AI Agent Tooling
  complexity: upper-medium
  complexityNote: Concentrated in convention detection and validate_approach
  projectContext: greenfield
  dualRequirements: true
---

# Product Requirements Document - Seshat

**Author:** Kostik
**Date:** 2026-03-16

## Executive Summary

Seshat is the operating manual for your codebase — written for AI agents.

It is an open-source MCP server, distributed as a single Rust binary, that automatically builds a persistent knowledge graph of a software project and exposes it through precisely defined tools that any MCP-compatible AI coding agent can call. Seshat provides shift-left convention enforcement: AI agents receive project-aware guidance before generating code, not after — eliminating the review-and-fix cycle that plagues AI-assisted development today.

**The problem:** AI coding agents (Claude Code, Cursor, Codex) operate within limited context windows and cannot internalize large codebases. They violate project conventions in virtually every non-trivial task — wrong imports, inconsistent naming, duplicated logic, dead code left behind. Developers spend as much time reviewing and fixing AI output as the AI spent generating it. Every session starts from scratch; the agent never truly "knows" the project.

**The insight:** The problem is not the agent's intelligence — it is the absence of persistent, queryable project knowledge. Seshat fills this gap with a three-layer knowledge graph (code intelligence, convention detection, explicit knowledge) queryable through 5 core MCP tools. The graph uses two-dimensional typing (Knowledge Nature x Knowledge Weight) with typed edges, enabling graduated responses that distinguish hard rules from conventions from observations.

**Target users:** Developers working with AI agents on projects with tens to hundreds of thousands of lines of code — in small teams (3-7 people), as solo developers maintaining multiple repos, or as juniors onboarding to established codebases. Secondary users include non-engineering stakeholders querying project architecture through their own AI assistants.

**Dual interface:** Seshat serves two consumers with distinct requirements. AI agents consume structured MCP tool responses optimized for parseability, latency, and actionable guidance. Developers interact through a CLI optimized for readability — scan reports, status output, configuration.

**Adoption model:** Bottom-up. Single binary, zero dependencies, zero configuration. `seshat scan /path/to/repo` produces an immediate analysis report. Connect as MCP server. Start coding. The agent gets smarter with every session.

### What Makes This Special

1. **Shift-left convention enforcement** — the only tool that guides AI agents *before* code generation, not during review. The `validate_approach` tool provides graduated pre-flight checks: rules (must fix) → conventions (should fix) → decisions (context) → observations (consider) → contradictions.

2. **Two-dimensional knowledge graph** — every node typed by Nature (Fact, Convention, Decision, Preference, Observation) and Weight (Rule, Strong, Moderate, Weak, Info), connected by typed edges (RelatedTo, Updates, Contradicts, PartOf, DependsOn, Implements). No existing tool has this structure for codebase knowledge.

3. **Decision reasoning** — Seshat doesn't just know *what* the convention is, it knows *why* it was decided. When an agent asks "which HTTP client?", Seshat responds with the canonical choice, adoption rate, the decision that established it, and the reasoning behind it. This eliminates agent uncertainty.

4. **Zero-config immediate value** — first scan auto-detects languages, modules, dependencies, and conventions with confidence scores. No template filling, no manual setup. Value in under 5 minutes.

5. **Compounding intelligence flywheel** — the knowledge graph persists and evolves. The more you use Seshat, the fewer mistakes your agent makes. Return to a project after months — the knowledge is still there.

## Project Classification

- **Project Type:** Developer Tool — dual-interface (Agent-facing MCP server + Developer-facing CLI)
- **Domain:** Developer Infrastructure / AI Agent Tooling
- **Complexity:** Upper-Medium — complexity concentrated in convention detection engine (8 heuristic detectors, multi-language AST analysis) and `validate_approach` graduated response generation
- **Project Context:** Greenfield — new open-source project, Rust, single binary, SQLite embedded storage
- **Dual Requirements:** Agent-facing (structured MCP responses, <1s latency, parseable output schema) + Developer-facing (CLI UX, analysis reports, configuration, terminal formatting)

---

## Success Criteria

### User Success

**Primary: 2x reduction in post-AI review iterations**

Today's baseline: a non-trivial AI-assisted coding task requires 3-4 review-and-fix iterations before the code matches project conventions. With Seshat, this should drop to 1-2 iterations or fewer.

**Proxy metric: "First clean PR"** — developer's first pull request after connecting Seshat passes review without convention-related comments. This is a measurable, binary event that doesn't require self-reporting.

**Aha moment:** Developer requests a feature implementation. The AI agent calls `validate_approach`, gets convention guidance, generates code. Developer reviews — nothing to fix. "It wrote code like someone who actually knows this project."

**Onboarding acceleration:** A junior developer's first PR on a new project gets approved without convention-related review comments. Time from joining to first clean PR drops from weeks to days.

### Business Success

Since Seshat is open-source with no monetization plans, business success = project health:

| Timeframe | Success looks like |
|-----------|-------------------|
| **1 month** | End-to-end flow works on Seshat's own codebase (self-hosted). Creator uses it daily. |
| **3 months** | Public release on GitHub. Used on 2-3 real projects (Rust + TS/Python). First external users try it. |
| **6 months** | Organic community adoption. GitHub issues show real-world usage. Convention detection accuracy validated on diverse projects. |
| **12 months** | Mentioned in 3+ independent articles/reviews about AI-coding tools. Listed in MCP tool marketplaces/directories. 5+ languages supported. |

**Key signal:** Unprompted recommendations — developers tell teammates "you should try this" without being asked.

### Technical Success

| Metric | Target | Rationale |
|--------|--------|-----------|
| First scan completion | < 60s for 100k LOC | Must feel fast enough to not break flow |
| MCP tool response (P95) | < 1 second | Agents already have multi-second latency; 1s for Seshat is imperceptible |
| Convention detection precision | > 80%, measured via built-in validation wizard | Interactive `seshat review` — user confirms/rejects each detected convention. Precision = confirmed / (confirmed + rejected) |
| First scan success rate | > 95% of projects | "It just works" — failures on first contact kill adoption |
| Crash rate | < 1 per 1000 scans | Rust helps here, but edge cases in AST parsing are real |
| Memory usage | < 500MB peak for 100k LOC | Must run alongside IDE + AI agent without pressure |

### Measurable Outcomes

| Outcome | How to measure | MVP target |
|---------|---------------|------------|
| Review iteration reduction | Proxy: "first clean PR after Seshat" — PR passes without convention comments | Achievable within first week of usage |
| Convention violations in AI output | Manual audit of generated code with/without Seshat | 50%+ fewer violations |
| Convention detection precision | Built-in: `seshat review` wizard tallies confirmed vs rejected | > 80% precision |
| Time to first value | From download to first useful MCP response | < 5 minutes |
| Token savings | Proxy: count Seshat MCP tool calls that prevented codebase exploration by agent | Logged automatically, trend tracked |
| Knowledge graph completeness | % of project modules represented after full scan | > 90% |

---

## Product Scope

### Architectural Principle

**Scan pipeline is full from day one.** The indexing pipeline (AST parsing, call graph, dependency graph, import analysis, convention detection) runs completely on every scan regardless of which MCP tools are exposed. Tools are thin query layers over collected data — adding a tool later is trivial. Re-scanning because data wasn't collected is not.

### MVP — Minimum Viable Product

**Core principle:** Every component has a minimal but functional implementation. Cut breadth, not depth.

| Component | MVP scope | Could be cut to... |
|-----------|-----------|---------------------|
| **MCP Tools** | 5 tools (query_project_context, query_convention, query_code_pattern, validate_approach, query_dependencies) | 3 tools (query_project_context, query_convention, validate_approach) |
| **Convention Detectors** | 8 detectors | 4-5 detectors (dependency usage, imports, error handling, naming, file structure) |
| **Languages** | 4 (Rust, TypeScript, JavaScript, Python) | 2 (TypeScript, Rust) |
| **Knowledge Graph** | Two-dimensional typing (Nature x Weight) + typed edges | Same — architecturally non-negotiable |
| **Transport** | stdio + SSE + HTTP | Same — comes from MCP library for free |
| **Search** | FTS5 default + optional vector | FTS5 only |
| **CLI** | scan, serve, status, review | scan, serve |
| **Storage** | SQLite, single file per repo | Same |

**Interactive validation wizard:** `seshat review` — TUI-based convention review after first scan. Arrow keys to navigate, right/left to confirm/reject. Simultaneously serves as onboarding, wow-moment, and built-in measurement protocol for convention detection precision.

**MVP is done when:** Seshat scans its own Rust codebase, a developer connects it to Claude Code or Cursor, the agent calls `validate_approach` before generating code, and the generated code follows project conventions without manual correction.

### Growth Features (Post-MVP)

- **`update_knowledge` tool** — explicit knowledge updates with Decision and Preference types
- **Identity knowledge type** — module-level "what is this and what does it do"
- **Additional languages** — Go, Java, C#, Ruby based on demand
- **Onboarding template** — structured MD document for declaring preferences
- **CI/CD integration** — convention checks in pull request workflows
- **Team shared knowledge** — shared graphs across team members
- **Adaptive learning prototype** — log developer corrections, suggest knowledge graph updates with confirmation prompts

### Vision (Future)

- **Full adaptive learning** — automatic knowledge graph evolution from observed developer corrections, pattern drift detection, periodic retrospective reports
- **Web dashboard** — knowledge graph visualization, convention browser, team analytics
- **IDE plugins** — inline convention hints in VS Code and JetBrains
- **Convention packs marketplace** — community-shared convention templates for popular frameworks (Next.js, Django, Actix, FastAPI)
- **Embeddable library mode** — other tools integrate Seshat as a Rust dependency
- **Multi-repo cross-referencing** — shared conventions across monorepo or related projects
- **Seshat becomes infrastructure** — as essential as a linter or formatter in every AI-assisted development setup

---

## User Journeys

### Journey 1: Andrei — "The Frustrated Senior" (Primary User, Success Path)

**Opening Scene:** Andrei, senior developer in a team of 5, opens his morning task: implement a new data export service. The project is 80k lines of Python. There's a coding guidelines document — 15 pages long — linked in AGENTS.md. He knows the guidelines well. His AI agent doesn't.

**Rising Action:** Andrei describes the task to his AI agent. The agent starts generating code. Andrei watches — but this time, the agent calls `query_convention("imports")` and learns imports go at the top of the file, grouped by stdlib/external/internal. It calls `query_code_pattern("service class")` and gets the exact pattern from `src/services/auth_service.py`. It calls `validate_approach` with its planned implementation — Seshat responds with a graduated check including: "DUPLICATES: utility function `escape_quotes()` already exists in `src/shared/string_utils.py` (used by 14 modules). Do not recreate."

**Climax:** The generated service follows every convention. Imports at top. Uses the existing shared utility instead of creating a duplicate. Error handling matches the project pattern. Constructor DI, not inheritance.

**Resolution:** Andrei reviews the PR. Zero convention comments. He merges it in 2 minutes instead of spending 20 minutes fixing imports, killing duplicates, and restructuring error handling. He messages his teammate: "You need to try this thing."

---

### Journey 2: Andrei — "The Copy-Paste Killer" (Primary User, Edge Case / Pain Point)

**Opening Scene:** Same Andrei, different day. A complex task: refactor the concurrency handling across three modules. The project has a `src/shared/concurrency.py` module with thread pool management, async wrappers, and rate limiters — built over months. The AI agent doesn't know it exists.

**Without Seshat:** The agent creates a new `asyncio.Semaphore` wrapper in the first module. Then another one in the second module — slightly different. Then a third variant in the third module. Andrei now has 3 duplicate concurrency utilities plus the original shared one. He spends 45 minutes cleaning up, extracting the duplicates, and pointing everything back to the shared module. This has happened before. It will happen again.

**With Seshat:** The agent calls `query_code_pattern("async rate limiting")`. Seshat performs semantic search across the knowledge graph and returns: "Existing implementation: `src/shared/concurrency.py` provides `ThreadPoolManager`, `async_rate_limiter()`, `bounded_gather()`. Used by 12 modules. CONVENTION [Strong]: All concurrency utilities centralized in shared/concurrency.py." The agent calls `validate_approach` — Seshat proactively warns: "DUPLICATES: do not recreate concurrency utilities. Use `bounded_gather()` from shared module." The agent uses the existing utilities. No duplication. No cleanup.

**Resolution:** The refactor is clean. All three modules use the shared concurrency utilities consistently. Andrei didn't have to explain "we have a shared module for this" — Seshat already knew.

**Capability revealed:** `query_code_pattern` with semantic search by functionality description (not just pattern name). `validate_approach` with **proactive duplicate detection** — Seshat warns about existing code without being asked. This is what no competitor does.

---

### Journey 3: Lena — "The New Junior" (Primary User, Onboarding)

**Opening Scene:** Lena, junior developer, joined the team a week ago. Her first real task: add a new API endpoint for user preferences. She's used AI coding agents before — on personal projects. But this codebase is 60k lines of TypeScript, and she doesn't know how anything is organized.

**Rising Action:** Lena tells her AI agent: "Create a new API endpoint for user preferences." The agent, connected to Seshat, calls `query_project_context`. It learns: Fastify framework, zod validation, constructor DI, specific directory structure. It calls `query_code_pattern("API endpoint")` and gets a complete example from `src/routes/auth.ts`. It calls `validate_approach` with its plan — Seshat confirms alignment but notes: "DECISION [2026-01]: migrated from Express to Fastify. Use Fastify route pattern, not Express."

**Climax:** The generated endpoint looks exactly like it was written by someone who's been on the project for months. Correct directory, correct pattern, correct validation library, correct error handling.

**Resolution:** Lena submits her first PR. The senior reviewer's comment: "Looks great, nothing to change." Lena didn't need to ask anyone how things are done — the AI agent asked Seshat for her. She learned the project's patterns by seeing correct code generated, not by making mistakes.

---

### Journey 4: Kostik — "The Context Switcher" (Primary User, Multi-Repo)

**Opening Scene:** Kostik maintains 4 open-source projects. He hasn't touched his Rust CLI tool in 6 weeks — been deep in a TypeScript project. Today he needs to add a feature to the Rust project. He vaguely remembers the error handling pattern but not the specifics.

**Rising Action:** Kostik opens the Rust project. His AI agent connects to Seshat, which already has the knowledge graph from last month's scan. The agent calls `query_project_context` — full picture in milliseconds. Then `query_convention("error handling")` — "Convention [Strong, 94%]: all errors use `thiserror` derive macro with `#[error()]` attribute. Custom error types per module in `errors.rs`. Propagation via `?` operator."

**Climax:** Kostik describes the feature. The agent generates code that perfectly matches the patterns Kostik established 6 months ago — patterns he can barely remember himself.

**Resolution:** "It remembers better than I do." Kostik didn't re-explain anything. The knowledge graph outlived his own memory. Context switch cost: zero.

---

### Journey 5: Marina — "The Curious PM" (Secondary User)

**Opening Scene:** Marina, product manager, needs to write a technical section for a PRD. She needs to understand how the notification system works — what triggers notifications, what delivery channels exist, how preferences are stored. Normally she'd interrupt a developer for 30 minutes.

**Rising Action:** Marina opens her AI assistant (Claude) with Seshat connected as MCP. She asks: "How does our notification system work?" The AI calls `query_project_context("notifications")` and `query_code_pattern("notification")`. Seshat returns module structure, key classes, delivery channels, and how user preferences are stored.

**Climax:** Marina's AI explains the architecture in plain language: "Notifications are triggered by events in `src/events/`, routed through `NotificationRouter` to three channels: email, push, in-app. User preferences stored in `notification_preferences` table, checked before each delivery."

**Resolution:** Marina writes the technical section of her PRD. It's accurate. The next day, a developer reads it and says "this is exactly right, how did you know?" She didn't ask anyone. She asked Seshat.

---

### Journey 6: AI Agent — "The Informed Assistant" (Machine User, Agent Perspective)

**Opening Scene:** An AI coding agent (Claude Code / Cursor) receives a task from a developer: "Add retry logic to the API client." The agent has access to Seshat via MCP tools.

**Step 1 — Context Gathering:** Agent calls `query_project_context`. Receives: project stack, module structure, key dependencies. Agent now knows this is a TypeScript/Fastify project with a specific architecture.

**Step 2 — Convention & Existing Code Check:** Agent calls `query_convention("error handling")` and `query_code_pattern("retry logic")`. The latter returns not just patterns but **existing implementations**: "`RetryWithBackoff` already exists in `src/shared/http/retry.ts`, used by 8 modules." Agent knows not to recreate what already exists.

**Step 3 — Pre-flight Validation:** Agent formulates approach: "I'll extend the existing retry module to support the API client." Calls `validate_approach`. Seshat responds: "APPROVED. Aligns with Convention [Strong]: centralized retry logic in shared/http/. DUPLICATES: none — extending existing module is correct. Note: existing `RetryConfig` type supports custom backoff strategies — extend, don't replace."

**Step 4 — Dependency Check:** Agent calls `query_dependencies("src/shared/http/retry.ts")`. Sees: used by 8 modules. Any changes must be backward-compatible.

**Step 5 — Code Generation:** Agent generates code that extends the existing retry module, uses existing types, follows naming conventions, and is backward-compatible with all 8 consumers.

**Resolution:** The agent made 4 Seshat calls in ~3 seconds total. Without Seshat, it would have spent 30+ seconds exploring the codebase, possibly missing the existing retry module entirely, and creating a duplicate implementation.

---

### Journey 7: First-Time Setup — "The Skeptic" (Onboarding Journey)

**Opening Scene:** A developer hears about Seshat from a colleague. Skeptical — "another tool that promises AI magic." Downloads the binary.

**Step 1 — First Scan:**
```
$ seshat scan ./my-project
Scanning... ████████████████████ 100% (47,203 files)

Project: my-project
Languages: TypeScript (78%), Python (19%), Shell (3%)
Modules: 34 detected
Dependencies: 127 packages

Conventions detected: 23
  High confidence (>85%): 15
  Medium confidence (50-85%): 6
  Low confidence (<50%): 2

Run `seshat review` to validate detected conventions.
```

**Step 2 — Interactive Review:**
```
$ seshat review
Found 23 conventions. Review with ←→ to confirm/reject, ↑↓ to navigate:

→ Import grouping: stdlib → external → internal    (93% adoption)  [confirm/reject]
```

Developer flies through 23 items in 40 seconds. 19 confirmed, 3 rejected, 1 partial.

**Step 3 — Connect to Agent:**
Adds Seshat to MCP config. Starts coding. First task — the agent calls `validate_approach` and catches a convention violation before writing a single line.

**Climax:** "Wait, it actually works. It knew about our barrel export convention that I didn't even mention."

**Resolution:** Skeptic becomes evangelist. Shares with team the next day.

**Note:** This journey serves as a **ready-made E2E acceptance test suite** — each step is independently testable.

---

### Journey 8: "The False Positive" (Failure & Recovery Path)

**Opening Scene:** Andrei has been using Seshat for a week. Today his agent calls `validate_approach` and gets: "WARNING: Convention [Strong, 87%] — use `dayjs` for date manipulation." But Andrei knows — the team decided to migrate to the native `Temporal API` two weeks ago. Seshat doesn't know — the Decision hasn't been recorded yet.

**The Problem:** The agent uses `dayjs` based on Seshat's recommendation. Andrei catches the error in review. Seshat gave incorrect guidance because its knowledge was outdated.

**Recovery:** Andrei runs `seshat review`. Finds the convention "use dayjs for dates" — rejects it. The knowledge graph updates: convention demoted from Strong to Observation. In a future version with `update_knowledge`, Andrei will record: "Decision: migrated from dayjs to Temporal API, reason: native browser support, no dependency." The graph will show the Decision superseding the old Fact via an `Updates` edge.

**Resolution:** One mistake. Quick recovery through `seshat review`. Seshat becomes more accurate. Trust is maintained because the recovery path is simple and transparent — the developer stays in control.

**Capability revealed:** `seshat review` as recovery mechanism. Graceful degradation when knowledge is outdated. Trust repair through transparency and developer control.

---

### Journey Requirements Summary

| Journey | Key Capabilities Revealed |
|---------|--------------------------|
| **Andrei — Success Path** | query_convention, query_code_pattern, validate_approach with proactive duplicate detection, structured MCP responses with real code examples |
| **Andrei — Copy-Paste Killer** | query_code_pattern with **semantic search by functionality**, proactive duplicate detection in validate_approach, shared module awareness |
| **Lena — Onboarding** | query_project_context, query_code_pattern, Decision type with reasoning ("why Fastify not Express"), zero-knowledge-required flow |
| **Kostik — Context Switcher** | Persistent knowledge graph across sessions, multi-repo namespace isolation, instant project recall after weeks away |
| **Marina — Curious PM** | Non-technical query support, architecture explanation via query_project_context + query_code_pattern, no-code interaction |
| **AI Agent — Informed Assistant** | Full MCP tool flow (<1s per call), structured parseable output, graduated validate_approach, backward-compatibility awareness via query_dependencies |
| **Skeptic — Onboarding** | CLI scan report with stats, interactive `seshat review` wizard, zero-config MCP setup, immediate wow-moment. **Doubles as E2E test suite.** |
| **False Positive — Recovery** | `seshat review` as recovery mechanism, graceful degradation with outdated knowledge, trust repair through developer control |

### Critical Insights from Journeys

1. **Proactive duplicate detection** in `validate_approach` — Seshat warns about existing code without being asked. No competitor does this.
2. **`query_code_pattern` must support semantic search** — finding existing implementations by describing functionality ("async rate limiting"), not just by pattern name.
3. **`seshat review` serves triple duty** — onboarding wow-moment + measurement protocol + failure recovery mechanism.
4. **Decision type with reasoning** is critical for onboarding journeys — juniors need to know not just "what" but "why".
