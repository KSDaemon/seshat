---
stepsCompleted: [step-01-init, step-02-discovery, step-02b-vision, step-02c-executive-summary, step-03-success, step-04-journeys, step-05-domain, step-06-innovation, step-07-project-type, step-08-scoping, step-09-functional, step-10-nonfunctional]
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

---

## Domain-Specific Requirements

### Local-First Security Model

Seshat operates as a local-first tool. The knowledge graph database (`.seshat.db`) resides on the same machine as the source code it analyzes.

- **No secret filtering in MVP**: The database has the same access scope as the source code — if someone has access to the DB file, they already have access to the source. Acceptable for local-first architecture.
- **Secret Hygiene detector (#9, post-MVP)**: Convention detector that identifies potential hardcoded secrets (`API_KEY`, `SECRET`, `PASSWORD` patterns) and surfaces as Observation: "potential hardcoded secret in `config.py:23` — consider environment variables per 12-factor methodology." This is a convention detector, not a security feature.
- **Future (centralized mode)**: If Seshat evolves to a remote/centralized server, secret detection, response filtering, and database encryption become mandatory. Explicitly out of scope for MVP.

### Incremental Scan & File Watching

**Critical architectural requirement.** Without incremental updates, Seshat creates the exact duplicate code problem it exists to prevent.

**Two-tier update architecture:**

| Tier | Trigger | Latency | What updates | Used by |
|------|---------|---------|--------------|---------|
| **Hot** | File save (file watcher) | < 1 second | AST nodes, dependency edges, new/deleted entities | `query_code_pattern`, `query_dependencies` |
| **Warm** | Periodic (every 30-60s) or after N accumulated changes | < 30 seconds | Convention aggregates, confidence scores, adoption rates | `query_convention`, `validate_approach` |

**Bulk change detection:** When > N files change within 2 seconds (e.g., `git checkout`, `git merge`), Seshat detects this as a bulk change and runs incremental re-scan of all affected files rather than processing them individually.

**No dependency on git commits:** Updates trigger on file save. Developers may make many changes before committing — Seshat must see all of them.

### Branch-Aware Knowledge Graph

**Key differentiator.** Seshat maintains per-branch snapshots of the knowledge graph.

**Behavior:**
- Seshat monitors the current git branch
- On branch switch: instantly loads the existing snapshot for that branch (if available), then runs background incremental diff to sync to actual file state
- On new branch: creates snapshot from current state, diverges as files change
- Branch snapshots store only delta from base state — minimal storage overhead (10 branches ≈ +5-15MB)

**Garbage collection:** Seshat periodically checks local git branches (`git branch --list`). If a branch has been deleted locally (e.g., after PR merge), its snapshot is cleaned up from the database.

**Result:** Developer switches from feature branch to main — Seshat's knowledge graph switches instantly. No re-scan, no stale data, no waiting. Agent on the new branch immediately has correct context.

### Ecosystem Dependencies

**Tree-sitter grammars:**
- Seshat depends on community-maintained Tree-sitter grammars for language support
- If a grammar lacks support for new syntax — Seshat skips the unsupported construct rather than failing. Partial analysis over no analysis.
- Seshat does not implement custom parsers. Grammar gaps are resolved upstream by the community.

**MCP protocol & library:**
- Seshat uses a third-party Rust MCP library. Protocol changes are delegated to library maintainers.
- Seshat's MCP integration layer is kept thin — if the library must be replaced, blast radius is limited to transport/handler layer, not the core knowledge graph or detection engine.

### Trust & Confidence Management

**Core principle: "Better to say nothing than to give wrong advice."**

- Conventions below confidence threshold excluded from `validate_approach` or presented as `Info` weight only
- Graduated response system (Rule → Strong → Moderate → Weak → Info) signals trustworthiness
- **Interactive validation (`seshat review`)** as trust calibration — confirmed conventions get weight boost, rejected get demoted

**Built-in precision self-diagnostic after review:**
```
Review complete: 19 confirmed, 3 rejected, 1 partial
Precision: 82.6%
Status: Seshat is calibrated and ready to use
```

If precision < 70%: warning that Seshat may not be reliable for this project. Transparency over false confidence.

---

## Innovation & Novel Patterns

### Detected Innovation Areas

**1. Shift-Left Convention Enforcement (Category-Defining)**

Seshat creates a new category: pre-generation intelligence for AI coding agents. Every existing tool in the space operates post-generation — linters check after code is written, Greptile reviews at PR time, code-graph tools provide search but not guidance. Seshat is the first tool that provides convention-aware, project-specific guidance to AI agents *before they write a single line of code*. The `validate_approach` tool is the embodiment of this — a pre-flight check that no one else offers.

**2. Two-Dimensional Knowledge Graph for Codebases**

No codebase intelligence tool uses a two-dimensional typing system (Nature x Weight) with typed graph edges. Existing tools store either flat lists (codebase-context: JSON), untyped graphs (code-graph-rag: nodes and edges without semantics), or manual entries (MegaMemory: user-filled). Seshat's approach — inspired by SpaceBot's memory system but adapted for code — enables graduated, contextual responses that distinguish hard rules from soft preferences, facts from decisions, observations from conventions.

**3. Proactive Duplicate Prevention**

`validate_approach` doesn't just validate conventions — it actively warns about existing code the agent didn't ask about. "You're about to create a rate limiter. One already exists in `shared/concurrency.py`." This shifts duplicate detection from post-hoc (found in code review) to pre-hoc (prevented before creation). No competitor does this.

**4. Branch-Aware Knowledge Graph**

Per-branch snapshots with instant switching and background incremental sync. Developer switches from feature branch to main — knowledge graph switches instantly. No re-scan, no stale context. No competing tool tracks branch-level codebase state.

**5. Triple-Duty Interactive Validation**

`seshat review` serves three purposes simultaneously: onboarding wow-moment (show the user what Seshat discovered), calibration mechanism (confirm/reject conventions to improve accuracy), and built-in measurement protocol (precision = confirmed / total). One feature, three high-value outcomes.

### Market Context & Competitive Landscape

The MCP-based codebase intelligence space has ~10 tools as of early 2026, none exceeding 6k GitHub stars. The space is fragmented:

| Category | Examples | Gap Seshat fills |
|----------|----------|-----------------|
| Structural code graphs | code-graph-rag, Axon | No convention awareness, no guidance |
| Semantic search / RAG | claude-context, Context+ | No persistent knowledge, no convention enforcement |
| Convention detection | codebase-context | Flat JSON, no graph, no pre-generation enforcement |
| Manual memory | MegaMemory | No auto-indexing, requires manual population |
| PR review | Greptile | Post-hoc, not pre-generation |

Seshat occupies an **uncontested position**: the intersection of auto-detected conventions + knowledge graph + pre-generation enforcement + MCP delivery. No tool combines all four.

### Validation Approach

| Innovation | How to validate | Success indicator |
|------------|-----------------|-------------------|
| Shift-left enforcement | Dog-food on Seshat's own codebase + 2 real projects | AI agent generates convention-correct code without explicit prompting |
| 2D knowledge graph | Compare Seshat responses vs. flat-list approach on same project | Seshat provides more nuanced, actionable guidance |
| Proactive duplicate prevention | Track duplicate code in AI output with/without Seshat | Measurable reduction in duplicated utilities/functions |
| Branch-aware graph | Switch branches during active coding session | Agent immediately has correct context for new branch |
| Triple-duty review | Run `seshat review` with 5 beta users | Users report it as valuable for onboarding, calibration, and trust |

### Risk Mitigation

| Innovation risk | Impact | Mitigation |
|----------------|--------|------------|
| Convention detection too inaccurate | Core value proposition fails | Start with frequency analysis on 8 specific detectors; `seshat review` as calibration; > 80% precision target |
| 2D graph adds complexity without value | Overly complex responses confuse agents | Default to simplified output; graduated detail only when agent requests it |
| Proactive duplicate detection false positives | Agent avoids creating legitimate new code | Only warn at High confidence; include "ignore if intentionally different" option |
| Branch snapshots add storage/complexity | Performance degradation | Delta-only storage; GC on deleted branches; lazy snapshot creation |

---

## Developer Tool Specific Requirements

### Language Support Matrix

All 8 convention detectors run for all 4 MVP languages. Language-aware prioritization adjusts *relevance weight*, not *availability*:

| Detector | Rust | TypeScript | JavaScript | Python |
|----------|------|-----------|------------|--------|
| Dependency usage analysis | High | High | High | High |
| Import organization | Medium | High | High | High |
| Error handling patterns | High | High | High | High |
| Naming conventions | Medium | High | High | High |
| Export patterns | Medium | High | High | Medium |
| Logging/observability | High | High | High | High |
| Test patterns | High | High | High | High |
| File structure patterns | High | High | High | High |

"Medium" = detector runs but findings weighted lower because language tooling (rustfmt, clippy) already enforces some aspects.

**Implementation note:** 8 detectors × 4 languages = 32 language-specific implementations. Each detector is a trait; each language provides a concrete implementation. Not all 32 are equal priority for MVP — prioritize by actual dog-fooding needs: TypeScript (all 8), Python (5-6), Rust (4-5), JavaScript (inherits from TypeScript with minimal delta).

### Distribution & Installation

**MVP distribution channels:**

| Method | Platform | Command |
|--------|----------|---------|
| Pre-built binaries | macOS (arm64, x86_64), Linux (x86_64, arm64), Windows (x86_64) | Download from GitHub Releases |
| Cargo install | All platforms with Rust toolchain | `cargo install seshat` |
| Homebrew | macOS, Linux | `brew install seshat` |

**Build pipeline:** GitHub Actions CI builds and attaches binaries to each release. Cross-compilation via `cross` or platform-specific runners.

**Future distribution (post-MVP):** `curl -fsSL https://seshat.dev/install.sh | sh` one-liner installer (standard for dev tools), apt/dnf repositories, Nix package, Docker image (for CI/CD use cases only).

### MCP Tool Interface (PRD Level)

Formal input/output schemas deferred to architecture phase. PRD-level specification:

| Tool | Input | Output | Error behavior |
|------|-------|--------|----------------|
| `query_project_context` | Optional: focus area (string) | Project overview: languages, modules, dependencies, patterns. Structured JSON. | Empty result if project not scanned |
| `query_convention` | Topic/domain (string) | Convention description, Nature, Weight, confidence %, adoption rate, code examples with file:line references | Empty result if no matching conventions |
| `query_code_pattern` | Pattern name OR functionality description (string) | Matching code examples with file:line references. Semantic search when exact match unavailable. Existing implementations surfaced. | Empty result if no matches |
| `validate_approach` | Proposed approach description (string) | Graduated response: Rules (must fix) → Conventions (should fix) → Decisions (context) → Observations (consider) → Duplicates (do not recreate) → Contradictions | "No issues found" if approach aligns |
| `query_dependencies` | Module/file/function identifier (string) | Dependents list, dependencies list, blast radius estimate, backward-compatibility notes | Empty result if identifier not found |

All responses include structured JSON suitable for agent consumption. Human-readable formatting available via CLI fallback.

**MCP tool descriptions are product surface.** The description text in MCP tool manifests determines how effectively AI agents use Seshat. These descriptions must be crafted, tested, and iterated like UX copy — not treated as throwaway documentation. Quality of agent input directly correlates with quality of Seshat output.

### CLI Interface

| Command | Purpose |
|---------|---------|
| `seshat scan <path>` | Initial project scan with analysis report |
| `seshat serve` | Start MCP server (stdio + SSE + HTTP) |
| `seshat status` | Show indexed projects and graph statistics |
| `seshat review` | Interactive convention validation wizard (TUI) |
| `seshat init <client>` | Generate MCP configuration for a specific client (claude-code, opencode, cursor) |

`seshat init` embeds current client configurations in the binary — updated with each release to stay current with latest client versions.

### Documentation Strategy

**User-facing documentation:**

| Document | Purpose | Priority |
|----------|---------|----------|
| `README.md` | Project overview, quick start, architecture summary, MCP tools overview | MVP |
| `--help` / per-command help | Complete CLI reference with all commands, flags, options | MVP |
| Quick Start in README | Install → scan → `seshat init <client>` → connect → verify | MVP |
| Client setup docs | Links to official MCP configuration docs for each supported client | MVP |

**MCP client configuration:** `seshat init <client>` generates a ready-to-use configuration snippet. README links to client-specific documentation rather than embedding configurations that may become outdated. Actual configs live in the binary and are updated with each release.

**Developer/contributor documentation (post-MVP):**
- `CONTRIBUTING.md` — how to contribute, code style, PR process
- `ARCHITECTURE.md` — internal architecture, module structure, how to add a new detector

### Implementation Considerations

**CLI UX principles:**
- Rich terminal output with colors and formatting (via `colored` or `owo-colors` crate)
- Progress bars for scanning (`indicatif` crate)
- Structured output option (`--json`) for programmatic consumption
- Sensible defaults — zero required flags for basic usage
- Helpful error messages with suggested fixes
- Respect `NO_COLOR` environment variable

**Cross-platform considerations:**
- File paths: `std::path::Path` consistently, handle Windows backslashes
- File watcher: `notify` crate handles platform differences
- Terminal: respect `NO_COLOR`, handle terminal width gracefully
- Git detection: `git2` crate for branch detection

---

## Project Scoping & Phased Development

### MVP Strategy & Philosophy

**MVP Approach:** Problem-Solving MVP — solve one problem exceptionally well. Seshat makes AI coding agents understand your project's conventions. Everything in MVP serves this single purpose.

**Resource reality:** Solo developer, evenings and weekends. No deadline pressure — quality over speed. Architecture decisions (branch-aware graph, incremental scan, two-tier updates) are harder to retrofit than to build correctly from the start.

**Key principle:** Full MVP scope, no cuts. Building right the first time saves total effort for a solo developer who can't afford rework.

### Development Milestones

Each milestone is independently useful and dog-foodable. Natural dependency chain: Storage → Scanner → Graph → Detectors → MCP Server → Tools → File Watcher → Branch Awareness → TUI.

| Milestone | Name | Scope | Dog-food value |
|-----------|------|-------|----------------|
| **M0** | "It scans" | Scan pipeline + SQLite schema + 2-3 detectors for Rust + `seshat scan` report | See what Seshat finds in your own project |
| **M1** | "It serves" | MCP server + `query_project_context` + `query_convention` + connect to Claude Code | Agent starts asking Seshat about conventions |
| **M2** | "It validates" | `validate_approach` + proactive duplicate detection + `query_code_pattern` + `query_dependencies` | Killer feature works. Agent generates convention-correct code. **First external show: README, blog post, logo, domain.** |
| **M3** | "Full MVP" | `seshat review` wizard + branch-aware graph + incremental scan + file watcher + `seshat init` + all 4 languages + all 8 detectors | Complete MVP. Public release. |

### MVP Feature Set (Phase 1 — across M0-M3)

**Core User Journeys Supported:**
- Journey 1 (Andrei — Success Path): Full convention-aware code generation
- Journey 2 (Andrei — Copy-Paste Killer): Proactive duplicate detection
- Journey 3 (Lena — Onboarding): Zero-knowledge project context
- Journey 4 (Kostik — Context Switcher): Persistent multi-repo knowledge
- Journey 6 (AI Agent — Informed Assistant): Complete MCP tool flow
- Journey 7 (Skeptic — Onboarding): Scan → review → connect → value
- Journey 8 (False Positive — Recovery): `seshat review` as recovery path

**Must-Have Capabilities:**

| Capability | Justification |
|------------|---------------|
| Full scan pipeline (AST, call graph, dependencies, conventions) | Architectural principle: scan pipeline is full from day one |
| 5 MCP tools | Core value proposition — each tool serves distinct user journey |
| 8 convention detectors × 4 languages | Prioritized by dog-fooding needs, all architecturally present |
| Two-dimensional knowledge graph (Nature × Weight + typed edges) | Non-negotiable differentiator |
| Branch-aware knowledge graph with per-branch snapshots | Harder to retrofit than build correctly |
| Incremental scan with file watcher (two-tier: hot + warm) | Without this, Seshat causes the same problem it solves |
| `seshat review` interactive wizard | Triple-duty: onboarding + calibration + measurement |
| `seshat init <client>` | Eliminates onboarding friction |
| FTS5 search (default) + optional vector search | Zero-config search; vector for power users |
| stdio + SSE + HTTP transport | Comes from MCP library |
| SQLite embedded storage | Single file, debuggable, zero dependencies |

### Post-MVP Features

**Phase 2 — Explicit Knowledge & Refinement:**
- `update_knowledge` tool — Decision and Preference types via MCP
- Identity knowledge type — module-level understanding
- Onboarding preferences template (MD document)
- Secret Hygiene detector (#9)
- Additional languages (Go, Java, C#, Ruby)
- `curl | sh` installer script
- Adaptive learning prototype — log corrections, suggest updates

**Phase 3 — Ecosystem & Scale:**
- Full adaptive learning with automatic graph evolution
- CI/CD integration — convention checks in PR workflows
- Team shared knowledge — shared graphs, conflict resolution
- Web dashboard with knowledge graph visualization
- IDE plugins (VS Code, JetBrains)
- Convention packs marketplace
- Embeddable library mode
- Multi-repo cross-referencing

### Testing Strategy

| Layer | Approach | From milestone |
|-------|----------|----------------|
| Unit tests | Standard Rust `#[test]`, all modules | M0 |
| Integration tests | Scan known repos, compare detected conventions against manual markup | M0 |
| Snapshot tests | Fix MCP response format, catch regressions | M1 |
| Self-scan CI | Seshat scans its own codebase in CI — failure = build failure | M0 |
| Precision measurement | `seshat review` on reference projects, track precision over releases | M3 |

### Idea Management

**`BACKLOG.md`** in repo root — all ideas from brainstorming, Party Mode, user feedback. Not lost, not distracting. Current work = current milestone only.

### Risk Mitigation Strategy

**Technical Risks:**

| Risk | Mitigation | Contingency |
|------|------------|-------------|
| Convention detection accuracy < 80% | Frequency analysis on 8 detectors; `seshat review` calibration; test on 3+ real projects | Reduce to highest-confidence detectors only |
| 32 detector implementations too much for solo dev | Prioritize: TS (all 8), Python (5-6), Rust (4-5), JS (inherit from TS). Ship incrementally. | Launch with 2 languages, add others as releases |
| Branch-aware graph adds complexity | Build branch detection early as spike; validate before full implementation | Fall back to single-branch mode, add in Phase 2 |
| Tree-sitter grammar gaps | Graceful degradation — skip unparseable constructs | Document known limitations per language |

**Market Risks:**

| Risk | Mitigation |
|------|------------|
| MCP protocol changes | Thin integration layer; library handles protocol evolution |
| Competing tool emerges | Ship fast, build community; 2D graph + proactive duplicates = deep moat |
| AI agents get larger context windows | Conventions remain valuable regardless of context size |

**Resource Risks:**

| Risk | Mitigation |
|------|------------|
| Solo dev, slow progress | No deadline; incremental milestones; each milestone delivers personal value |
| Motivation loss | Dog-food daily; immediate personal value sustains motivation |
| Scope creep | This PRD defines scope. BACKLOG.md for ideas. Resist temptation. |

### Branding & Web Presence (Target: Milestone 2)

- Logo design
- Domain: `seshat.dev` (preferred) or subdomain on personal domain
- Blog post: "Introducing Seshat — the operating manual for your codebase, written for AI agents"
- README with architecture overview, quick start, MCP tool descriptions

---

## Functional Requirements

**Design principle:** MCP tool responses serve non-technical users equally when mediated by an AI assistant — no developer-specific jargon required to interpret.

### Project Scanning & Indexing

- **FR1** [M0]: Developer can scan a project directory to build a knowledge graph of the codebase
- **FR2** [M0]: Seshat can parse source code files using Tree-sitter AST for supported languages (Rust, TypeScript, JavaScript, Python)
- **FR3** [M0]: Seshat can detect and analyze dependency manifests (`Cargo.toml`, `package.json`, `pyproject.toml`) and cross-reference with actual usage in code
- **FR4** [M0]: Seshat can build call graphs and dependency graphs from parsed AST
- **FR5** [M0]: Seshat can detect module structure and file organization patterns
- **FR6** [M0]: Developer can see an analysis report after scanning showing: languages detected, modules found, dependencies mapped, conventions detected with confidence scores
- **FR7** [M3]: Seshat can perform incremental updates when files change — code structure and dependency edges update immediately (hot tier), convention aggregates and confidence scores update shortly after (warm tier)
- **FR8** [M3]: Seshat can watch the project directory for file changes in real-time while serving as MCP server
- **FR9** [M3]: Seshat can detect bulk file changes (e.g., git checkout) and handle them as a batch
- **FR10** [M0]: Seshat can store all knowledge graph data in a SQLite database (single file per project)
- **FR11** [M0]: Seshat can parse and ingest project documentation files (Markdown, JSON schemas, OpenAPI specs) as additional knowledge sources — extracting conventions, rules, and guidance described in prose
- **FR12** [M0]: Seshat can gracefully skip unparseable or unsupported files during scan and report them without failing the entire scan

### Knowledge Graph

- **FR13** [M0]: Seshat can represent knowledge nodes with two-dimensional typing: Nature (Fact, Convention, Observation, Decision, Preference) and Weight (Rule, Strong, Moderate, Weak, Info)
- **FR14** [M0]: Seshat can represent typed edges between knowledge nodes (RelatedTo, Updates, Contradicts, PartOf, DependsOn, Implements)
- **FR15** [M0]: Seshat can automatically assign confidence scores and adoption rates to detected conventions based on frequency analysis
- **FR16** [M3]: Developer can confirm, reject, or partially confirm detected conventions through interactive review, updating their weight in the graph
- **FR17** [M3]: Seshat can maintain per-branch snapshots of the knowledge graph
- **FR18** [M3]: Seshat can instantly switch knowledge graph context when the developer changes git branches
- **FR19** [M3]: Seshat can perform background incremental sync after branch switch to update snapshot to current file state
- **FR20** [M3]: Seshat can garbage-collect snapshots for locally deleted git branches

### Convention Detection

- **FR21** [M0]: Seshat can detect dependency usage patterns — canonical libraries per domain, conflicting libraries for same purpose, dead dependencies
- **FR22** [M0]: Seshat can detect import organization patterns — grouping order, barrel vs. direct imports
- **FR23** [M0]: Seshat can detect error handling patterns — error types, propagation style
- **FR24** [M1]: Seshat can detect naming conventions — files, functions, types, variables, constants
- **FR25** [M1]: Seshat can detect export patterns — default vs. named, re-exports, public API surface
- **FR26** [M1]: Seshat can detect logging and observability patterns — library choice, structured vs. unstructured
- **FR27** [M2]: Seshat can detect test patterns — framework, file placement, naming, setup/teardown
- **FR28** [M2]: Seshat can detect file structure patterns — module organization, directory conventions
- **FR29** [M0]: Each detector can run for all supported languages with language-aware relevance weighting
- **FR30** [M0]: Seshat can cross-reference conventions detected from code with conventions described in project documentation (e.g., coding guidelines) and flag contradictions

### MCP Server & Tools

- **FR31** [M1]: Seshat can serve as an MCP server via stdio, SSE, and HTTP transports
- **FR32** [M1]: AI agent can query project context — receiving project overview with stack, modules, dependencies, and patterns
- **FR33** [M1]: AI agent can query conventions — receiving convention description, Nature, Weight, confidence score, adoption rate, and code examples with file:line references
- **FR34** [M2]: AI agent can query code patterns by name or by functionality description — receiving matching code examples and existing implementations via semantic search
- **FR35** [M2]: AI agent can validate a proposed approach — receiving graduated response: Rules (must fix) → Conventions (should fix) → Decisions (context) → Observations (consider) → Duplicates (do not recreate) → Contradictions
- **FR36** [M2]: AI agent can query dependencies for a module/file/function — receiving dependents, dependencies, blast radius estimate
- **FR37** [M2]: Seshat can proactively detect and warn about existing code that matches the agent's proposed approach in validate_approach responses (duplicate prevention)
- **FR38** [M1]: All MCP tool responses can be returned as structured JSON suitable for agent consumption
- **FR39** [M1]: Seshat can return informative error when MCP tool is called for an unscanned or unknown repository

### CLI Interface

- **FR40** [M0]: Developer can run `seshat scan <path>` to scan a project and see an analysis report
- **FR41** [M1]: Developer can run `seshat serve` to start the MCP server
- **FR42** [M2]: Developer can run `seshat status` to see indexed projects and graph statistics
- **FR43** [M3]: Developer can run `seshat review` to interactively validate detected conventions via TUI (navigate, confirm, reject, partial confirm)
- **FR44** [M3]: Developer can search/filter conventions within `seshat review` by keyword (slash-search, like IDE conventions)
- **FR45** [M3]: Seshat can display precision self-diagnostic after review completion (confirmed/rejected/partial counts, precision percentage, readiness status)
- **FR46** [M3]: Developer can run `seshat init <client>` to generate a ready-to-use MCP configuration snippet for a specific AI coding client

### Multi-Repository Support

- **FR47** [M1]: Developer can scan and serve multiple repositories simultaneously with namespace isolation
- **FR48** [M1]: Seshat can maintain independent knowledge graphs per repository

### Search & Data Management

- **FR49** [M1]: Seshat can perform full-text search across the knowledge graph using FTS5 (default, zero-config)
- **FR50** [M2]: Seshat can optionally perform vector similarity search when an embedding provider is configured
- **FR51** [M0]: Seshat can automatically create periodic backups of the database (sensible default: daily, keep last 3 backups)
- **FR52** [M0]: Developer can configure backup frequency and retention through configuration file

### Configuration

- **FR53** [M0]: Developer can configure Seshat behavior through optional configuration file (scan exclusions, language priorities, embedding provider, backup settings)
- **FR54** [M0]: Seshat can operate with sensible defaults when no configuration file exists (zero-config promise)

**Milestone distribution:** M0: 18 FRs (foundation) | M1: 13 FRs (MCP server) | M2: 9 FRs (killer features) | M3: 14 FRs (polish)

---

## Non-Functional Requirements

### Performance

| Metric | Target | Context |
|--------|--------|---------|
| Initial scan speed | < 60 seconds for 100k LOC | First impression. Developer runs `seshat scan` and waits. >60s feels broken. |
| Initial scan speed (large) | < 5 minutes for 500k LOC | Large projects scannable during a coffee break, not a lunch break. |
| Parallel scanning | Utilize all available CPU cores | Work-stealing thread pool (rayon) for parallel file parsing and detector execution |
| MCP tool response (P95) | < 1 second | Agents already have multi-second latency. 1s for Seshat is imperceptible. |
| MCP tool response (P50) | < 300ms | Typical responses should feel instant. |
| Incremental update (hot tier) | < 1 second after file save | Developer saves file → agent's next query reflects the change. |
| Incremental update (warm tier) | < 30 seconds for convention recalculation | Convention aggregates don't need real-time accuracy. |
| Branch switch | < 2 seconds to load existing snapshot | Must feel instant. Background sync catches up. |
| Memory usage (scanning) | < 500MB peak for 100k LOC | Must run alongside IDE + AI agent + browser without memory pressure. |
| Memory usage (serving) | < 100MB steady state | MCP server should be lightweight when idle between queries. |
| Database size | < 50MB per 100k LOC | Single SQLite file should not dominate disk usage. |

### Reliability

| Requirement | Target | Rationale |
|-------------|--------|-----------|
| Crash rate | < 1 panic per 1000 scans | Rust's type system helps, but Tree-sitter edge cases and malformed files are real risks |
| Graceful degradation | Unparseable files skipped, not fatal | One bad file should not prevent scanning 99,999 good files |
| Data integrity | All database writes transactional (SQLite WAL mode) — crash at any point leaves database in last consistent state | Corrupted graph = wrong advice = worse than no advice |
| Interrupted scan recovery | Scan writes results in batches per file/module — interrupted scan preserves completed portions | `Ctrl+C` mid-scan should not lose all progress |
| Database backup | Automatic daily backup, keep last 3 | Recovery path from corruption without losing more than 24h of data |
| File watcher stability | No resource leaks over extended runtime | Seshat may run for days as MCP server. Memory/handles must not grow unbounded. |

### Observability

| Requirement | Target | Rationale |
|-------------|--------|-----------|
| Structured logging | All code instrumented with `tracing` at appropriate levels (error/warn/info/debug/trace) | MCP server runs as background process — logs are the primary debugging interface |
| Log verbosity | Configurable via CLI flag or environment variable. Default: info | Developer can enable debug/trace for troubleshooting |
| Tool call logging | Every MCP tool call logged with: tool name, repo, duration, result summary | Essential for understanding agent behavior and diagnosing slow responses |

### Integration

| Requirement | Target | Rationale |
|-------------|--------|-----------|
| MCP protocol compliance | Full compliance with MCP specification | Must work with any MCP-compatible client without workarounds |
| MCP response consistency | All tools follow consistent JSON envelope — same structure, same error format, same metadata fields | Agent parses one format, not five |
| Cross-platform binary | macOS (arm64, x86_64), Linux (x86_64, arm64), Windows (x86_64) | Developer tools must work on all major platforms |
| Git compatibility | Works with any standard git repository | No dependency on specific git hosting (GitHub, GitLab, etc.) |
| Tree-sitter grammar compatibility | Uses upstream grammars without modification | No custom forks that create maintenance burden |
| Shell/terminal compatibility | Works in any POSIX terminal + Windows Terminal + PowerShell | CLI output degrades gracefully (NO_COLOR, narrow terminals) |
| SQLite compatibility | Standard SQLite without custom extensions (FTS5 is built-in) | Database viewable with any SQLite client for debugging |

### Compatibility

| Requirement | Target | Rationale |
|-------------|--------|-----------|
| Database migration | Automatic schema upgrade from any previous version on startup | Every `cargo install seshat` update must not require re-scanning projects |
| Migration chain | Latest Seshat migrates from v0.1.0 to current — sequential migrations | Oldest database must be upgradeable to newest schema |

### Maintainability

| Requirement | Target | Rationale |
|-------------|--------|-----------|
| Modular detector architecture | New detector addable without modifying core | Solo dev needs to add languages/detectors without risk of regression |
| Thin MCP integration layer | MCP library replaceable without core changes | Protocol/library evolution should not require rewrite |
| Test coverage | Unit tests for all detectors, integration tests on reference projects | Solo dev cannot afford manual regression testing |
| Self-scanning CI | Seshat scans its own codebase on every commit | Dogfood as automated quality gate |

### Developer Experience (Soft Targets)

| Metric | Target | Notes |
|--------|--------|-------|
| Incremental `cargo build` | < 30 seconds | Multi-crate workspace — only changed crates rebuild |
| Full `cargo test` | < 60 seconds (soft) | May be exceeded by integration tests requiring external resources |
| Cold `cargo build` | Reasonable for Rust project | No hard target — Rust compilation is what it is |
