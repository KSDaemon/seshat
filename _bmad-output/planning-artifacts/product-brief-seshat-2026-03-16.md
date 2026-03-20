---
stepsCompleted: [1, 2, 3, 4, 5]
inputDocuments: []
date: 2026-03-16
author: Kostik
---

# Product Brief: Seshat

<!-- Content will be appended sequentially through collaborative workflow steps -->

## Executive Summary

Seshat is an open-source, shift-left intelligence layer for AI coding agents. Distributed as a single Rust binary with zero dependencies, Seshat builds and maintains a multi-layered knowledge graph of software projects, enabling AI agents to understand project conventions, patterns, and architecture before generating a single line of code.

Named after the Egyptian goddess of knowledge and writing, Seshat solves the fundamental problem that AI coding agents face today: they operate within limited context windows, cannot grasp the full scope of large codebases, and repeatedly violate project conventions — producing code that requires constant manual review and correction.

Unlike existing tools that focus on either structural code graphs or semantic search, Seshat provides a unified query interface across three knowledge layers — code intelligence, convention detection, and explicit developer knowledge — exposed through precisely defined MCP tools. AI agents call these tools naturally during development, receiving project-aware guidance at the moment of code generation, not after the fact during code review.

Seshat delivers immediate value with zero configuration: scan a project, see what it discovered, start coding better. Over time, the knowledge graph compounds — the more you use it, the fewer mistakes your AI agent makes.

---

## Core Vision

### Problem Statement

Modern AI coding agents operate within limited context windows that cannot encompass large codebases (tens to hundreds of thousands of lines of code). Even with careful task formulation, planning modes, and structured methodologies, these agents consistently produce code that violates project conventions — incorrect imports, inconsistent naming, duplicated logic, forgotten cleanup of dead code, and ignored coding guidelines. Virtually every non-trivial AI-assisted task requires manual review and corrections, creating a paradox: the tools meant to accelerate development introduce their own overhead.

The root cause is not the agent's intelligence — it's the absence of persistent, queryable project knowledge. Every session starts from scratch. Every convention must be re-explained. The agent never truly "knows" the project.

### Problem Impact

- **Universal occurrence**: Developers find convention violations in virtually every AI-generated change that touches multiple modules
- **Repetitive instruction overhead**: The same conventions and preferences must be re-explained in every session — the agent never remembers
- **Compounding technical debt**: Each AI-generated inconsistency increases codebase entropy, making future AI-assisted work less reliable
- **Token waste at scale**: Agents spend significant tokens exploring code structure that could be answered instantly from a knowledge base
- **Review burden negates productivity gains**: Time saved by AI generation is consumed by reviewing and fixing convention violations

### Why Existing Solutions Fall Short

The current landscape offers fragmented solutions:

- **Structural code graph tools** (code-graph-rag, codebase-memory-mcp, Axon) map functions, classes, and call graphs but have zero understanding of conventions or developer preferences
- **Semantic search tools** (claude-context, Context+) provide better code retrieval but no persistent knowledge about how code *should* be written
- **Convention-aware tools** (codebase-context) auto-detect patterns but store them in flat JSON, not a queryable graph, and lack integration with code structure
- **Memory tools** (MegaMemory) require manual population by the agent with no auto-indexing
- **All existing tools** enforce conventions at review time (Greptile) or not at all — none provide shift-left enforcement at the moment of code generation

No existing tool combines code structure, conventions, and developer preferences in a unified queryable interface. No tool delivers zero-config immediate value while building compounding intelligence over time.

### Proposed Solution

Seshat is a shift-left intelligence layer for AI coding agents, implemented as an MCP server:

**Architecture — Three Knowledge Layers, One Interface:**

1. **Code Intelligence Layer** (automatic, zero-config): Tree-sitter AST parsing, call graphs, module structure, dependencies. Works immediately on first scan.
2. **Convention Detection Layer** (automatic + heuristic): Import patterns, naming conventions, error handling styles, file structure patterns. Inferred from code with confidence scores.
3. **Explicit Knowledge Layer** (user-driven): Developer preferences, architectural decisions, technology choices. Updated through MCP tools or onboarding templates.

AI agents interact through a single set of MCP tools — they never see the layers, only the answers.

**Core MCP Tools:**

1. `query_project_context` — project overview: stack, structure, modules, dependencies
2. `query_convention` — "how is X done in this project?" with examples
3. `query_code_pattern` — "show me how pattern Y is implemented here"
4. `validate_approach` — pre-flight check: "I plan to do Z — does this match conventions?"
5. `update_knowledge` — explicit knowledge updates: "we now use library A instead of B"
6. `query_dependencies` — impact analysis: "what depends on module X?"

**Delivery:** Single Rust binary. No Docker, no package managers, no external databases. SQLite embedded storage. Cross-platform. `seshat scan /path/to/repo` and it works.

**Value Flywheel:** Immediate value on first scan (project analysis report with discovered patterns and confidence scores) → compounding value over time as the knowledge graph grows through usage and explicit updates.

### Key Differentiators

1. **Shift-Left Convention Enforcement**: The only tool that provides convention-aware guidance to AI agents *before* code generation, not during review
2. **Agent Intelligence Layer**: Positioned not as a developer tool, but as an intelligence layer that makes any AI coding agent smarter about your specific project
3. **Three-Layer Knowledge Graph**: Unified query interface over code structure + auto-detected conventions + explicit developer knowledge — no existing tool combines all three
4. **Zero-Config Immediate Value**: Single binary, no dependencies, first scan produces actionable project intelligence with confidence-scored convention detection
5. **Compounding Intelligence Flywheel**: The knowledge graph evolves with the project — the more you use Seshat, the fewer mistakes your agent makes
6. **Embeddable Architecture**: Rust + SQLite + MCP = ideal candidate for integration into IDEs, CI/CD pipelines, and other developer tools
7. **Open Source, Local-First**: No SaaS dependency, no data leaving your machine, community-driven development
8. **Dog-Fooded from Day One**: Built by a developer who experiences this problem daily

---

## Target Users

### Primary Users

#### Persona 1: "Andrei" — Mid/Senior Developer in a Small Team

**Profile:** Mid-level or senior developer, 3-8 years of experience, works in a small team (3-7 people) on a project with tens to hundreds of thousands of lines of code. Uses AI coding agents daily (Claude Code, Cursor, Codex). Technically proficient, understands the project well, but frustrated by the constant need to review and fix AI-generated code.

**Day in the Life:** Andrei picks up a feature task from the sprint board. He opens his AI agent, describes what he needs, and the agent starts generating code. The agent creates a new service but uses a different error handling pattern than the rest of the project. It imports from barrel exports instead of direct paths. It adds a utility function that already exists elsewhere. Andrei spends 20 minutes reviewing and fixing what should have been a 5-minute generation. He's done this hundreds of times. He knows the conventions by heart — the agent doesn't.

**With Seshat:** Andrei runs `seshat scan` once. His AI agent now calls `validate_approach` before generating code and `query_convention` when unsure. The agent asks Seshat "how are errors handled in this project?" and gets back the exact pattern with examples. Code comes out right the first time. Review time drops dramatically.

**Success Moment:** The first time Andrei reviews AI-generated code and finds nothing to fix. "It finally wrote code like someone who actually knows this project."

---

#### Persona 2: "Lena" — Junior Developer Onboarding to a Project

**Profile:** Junior developer, 0-2 years of experience, recently joined a team working on an established codebase. Eager to contribute but overwhelmed by the project's size and unwritten conventions. Uses AI agents but doesn't know how to prompt them effectively because she doesn't yet understand the project's patterns.

**Day in the Life:** Lena is assigned her first real task — add a new API endpoint. She asks her AI agent to scaffold it, but the generated code looks nothing like the existing endpoints. She doesn't know enough to tell what's wrong. She asks a senior developer, who spends 30 minutes explaining "we do it like this, look at the auth service as an example." This happens multiple times a day. Lena feels like a burden; seniors feel interrupted.

**With Seshat:** Lena's AI agent has Seshat connected. When generating the endpoint, the agent calls `query_code_pattern` for "API endpoint" and gets back the exact pattern used in this project, with a real example from the auth service. The code matches project conventions from the start. Lena learns the project's patterns by seeing correct code generated, not by making mistakes and being corrected.

**Success Moment:** Lena's first PR gets approved without convention-related comments. Her senior says "this looks like you've been on the project for months."

---

#### Persona 3: "Kostik" — Solo Developer / Open Source Maintainer

**Profile:** Experienced developer working solo or maintaining an open source project. Juggles multiple repositories. Knows his projects deeply but context-switches between them frequently. After weeks away from a project, details fade — how was caching done? What was the logging convention?

**Day in the Life:** Kostik returns to a side project after a month focused on another one. He needs to add a feature. He vaguely remembers how things were structured but not the specifics. His AI agent doesn't remember either — it explores the codebase from scratch, burning tokens and making assumptions. Kostik spends time re-explaining patterns he established himself months ago.

**With Seshat:** Seshat's knowledge graph persists between sessions. When Kostik returns, the graph is already there. His AI agent calls `query_project_context` and instantly has the full picture. No re-exploration, no re-explanation. The project's knowledge outlives any single coding session.

**Success Moment:** Returning to a project after a month and having his AI agent generate code that perfectly matches conventions he can barely remember. "It remembers better than I do."

---

### Secondary Users

#### Persona 4: "Marina" — Product Manager / Non-Engineering Stakeholder

**Profile:** Product manager or data analyst who needs to understand how features are implemented without reading code. Frequently asks developers "how does X work?" or "what would it take to change Y?" — questions that pull engineers out of flow.

**Use Case:** Marina connects Seshat as an MCP server to her AI assistant. Instead of interrupting developers, she asks her AI: "How is user authentication implemented in our project?" The AI calls `query_project_context` and `query_code_pattern` and explains the architecture in plain language. Marina gets her answers without waiting for developer availability.

**Success Moment:** Marina writes a technical section of a PRD that accurately describes the current implementation, without asking a single developer.

---

### User Journey

**Discovery:** Bottom-up adoption. A developer finds Seshat on GitHub, sees the promise of "make your AI agent actually understand your project," and decides to try it. No organizational approval needed — it's a local tool.

**Onboarding (< 5 minutes):**
1. Download single binary (or `cargo install seshat`)
2. Run `seshat scan /path/to/repo`
3. See the initial analysis report — languages detected, modules mapped, patterns discovered with confidence scores
4. Add Seshat as MCP server to their AI agent's configuration
5. Start coding — the agent now calls Seshat tools automatically

**Core Usage:** Invisible to the developer. The AI agent calls Seshat tools behind the scenes. The developer notices the effect: fewer convention violations, less review time, better code. Occasionally the developer uses `update_knowledge` to teach Seshat something new ("we migrated from library X to Y").

**Aha Moment:** When the AI agent generates code that correctly follows an obscure project convention that the developer didn't explicitly mention in their prompt. "How did it know to do that?"

**Viral Spread:** Developer shows a teammate: "Look, my agent finally writes code that matches our project." Teammate installs Seshat. Within weeks, the whole team uses it. The knowledge graph becomes a shared team asset.

**Long-term:** Seshat becomes infrastructure — as essential as a linter or formatter. The knowledge graph is a living document of how the project works, maintained automatically and enriched over time. New team members get the benefit from day one.

---

## Success Metrics

### User Success Metrics

**Primary Metric: Reduction in Post-AI Review & Fix Cycles**

The core measure of Seshat's value. Today a typical AI-assisted coding workflow looks like:
- ~20% crafting the prompt
- ~40% AI generating code
- ~40% reviewing, iterating, and fixing AI output

Success means compressing that last 40% significantly. Measured qualitatively by user perception ("I'm fixing less") and quantitatively where possible by tracking the number of review iterations per task.

**Secondary Metrics:**

- **Convention violation rate**: How often does AI-generated code violate project conventions with Seshat vs. without? Fewer violations = Seshat is working.
- **Token efficiency**: For users on API-based pricing, reduced token consumption per task. The AI agent explores less, asks Seshat instead, and gets it right faster. For subscription users — fewer rate limit hits.
- **Time to first correct generation**: How quickly does the AI agent produce code that needs no convention-related fixes? Shorter time = higher value.
- **Onboarding acceleration**: For junior developers (Persona "Lena") — time from joining a project to first PR approved without convention comments. With Seshat, this should be days, not weeks.

### Business Objectives

Since Seshat is an open-source, community-driven project with no current monetization plans, "business objectives" translate to **project health and community traction**:

**3-Month Goals (Post-Launch):**
- Seshat is usable end-to-end: scan a project, expose MCP tools, AI agent successfully queries and gets useful responses
- Dog-fooded daily by the creator and team on real projects
- Initial public release on GitHub with clear documentation

**6-Month Goals:**
- Community adoption begins: developers outside the core team are using Seshat on their own projects
- Feedback loop established: GitHub issues and discussions show real-world usage patterns and feature requests
- Knowledge graph proves value: measurable reduction in AI-generated convention violations on at least 3 different projects

**12-Month Goals:**
- Seshat recognized as a category-defining tool in the "AI coding agent intelligence" space
- Healthy open-source community: active issue discussions, external bug reports, potential first external contributors
- Multi-language support covers the most popular languages (TypeScript, Python, Rust, Go, Java at minimum)

### Key Performance Indicators

**Product KPIs:**

| KPI | Measurement | Target |
|-----|-------------|--------|
| First scan success rate | % of projects that complete initial scan without errors | > 95% |
| Convention detection accuracy | User-confirmed conventions vs. false positives | > 80% precision |
| MCP tool response time | P95 latency for tool calls | < 500ms |
| Knowledge graph completeness | % of project modules/patterns represented in the graph | > 90% after full scan |

**Adoption KPIs:**

| KPI | Measurement | Target (12 months) |
|-----|-------------|---------------------|
| GitHub stars | Public interest signal | Organic growth, no specific target |
| Binary downloads / cargo installs | Actual adoption | Track trend, not absolute number |
| Active projects scanned | Unique repos where Seshat is actively used (opt-in telemetry or survey) | Growth month-over-month |
| User retention | Users who continue using Seshat after first week | > 60% |

**Quality KPIs:**

| KPI | Measurement | Target |
|-----|-------------|--------|
| Languages supported | Number of languages with Tree-sitter grammar integration | 5+ at launch, 10+ at 12 months |
| Crash rate | Panics or unrecoverable errors per 1000 scans | < 1 |
| Memory usage | Peak RAM during scan of 100k LOC project | < 500MB |

**Anti-Metrics (What We Explicitly Don't Optimize For):**

- GitHub stars as a vanity metric — we track them but don't optimize for them
- Number of contributors — meaningful contributions matter more than contributor count
- Feature count — fewer, well-designed MCP tools beat a long feature list

---

## MVP Scope

### Core Features

**1. Project Scanner & Indexer**
- Full codebase scanning with Tree-sitter AST parsing
- Module structure detection, dependency mapping, call graph construction
- Dependency manifest analysis (`Cargo.toml`, `package.json`, `pyproject.toml`) cross-referenced with actual usage in code
- File system watching for incremental updates
- Initial analysis report on first scan — languages, modules, detected patterns with confidence scores
- SQLite-based persistent storage (single `.db` file per project)

**2. Knowledge Graph with Two-Dimensional Typing**

Every knowledge node has two axes:

**Knowledge Nature** (what kind of knowledge):
- `Fact` — verified project information (auto-detected from scan)
- `Convention` — established coding pattern (auto-detected, high adoption)
- `Observation` — detected pattern, not yet confirmed as convention (auto-detected, lower adoption)
- `Decision` — deliberate choice with reasoning and timestamp (via `update_knowledge`)
- `Preference` — developer/team preference (via `update_knowledge`)

**Knowledge Weight** (how important):
- `Rule` (1.0) — must follow, violation = error
- `Strong` (0.8) — should follow, >85% adoption
- `Moderate` (0.5) — common practice, 50-85% adoption
- `Weak` (0.2) — sometimes seen, <50%
- `Info` (0.0) — informational only

**Graph Edges** (how knowledge connects):
- `RelatedTo` — general association
- `Updates` — supersedes previous knowledge (Decision updates Fact)
- `Contradicts` — conflicts with another node
- `PartOf` — component of larger entity
- `DependsOn` — dependency relationship
- `Implements` — realizes a convention or pattern

**3. Convention Detection Engine (8 Detectors)**

Trait-based, pluggable detector architecture. MVP ships with:

1. **Dependency usage analysis** — canonical libraries per domain (logging, HTTP, ORM, testing), conflict detection (two libraries for same purpose), dead dependencies
2. **Import organization** — grouping order, barrel vs. direct imports
3. **Error handling patterns** — error types, propagation style, Result vs. exceptions
4. **Naming conventions** — files, functions, types, variables, constants
5. **Export patterns** — default vs. named, re-exports, public API surface
6. **Logging/observability** — library choice, structured vs. unstructured, format patterns
7. **Test patterns** — framework, file placement, naming, setup/teardown
8. **File structure patterns** — module organization, directory conventions

Each detector outputs nodes with confidence scores. Language-aware prioritization — all 8 detectors run for JS/Python; for Rust/Go, focus shifts to dependency usage, patterns, and structure (formatting already enforced by tooling).

**4. MCP Server with Core Tools**

Exposed via MCP protocol (stdio + SSE + HTTP transports via Rust MCP library):

| Tool | Purpose | Response includes |
|------|---------|-------------------|
| `query_project_context` | Project overview | Stack, modules, dependencies, tech choices |
| `query_convention` | "How is X done here?" | Convention + weight + confidence + code examples + decision reasoning if available |
| `query_code_pattern` | "Show me an example" | Real code examples from the project |
| `validate_approach` | Pre-flight check | Graduated response: Rules (must fix) → Conventions (should fix) → Decisions (context) → Observations (consider) → Contradictions |
| `query_dependencies` | Impact analysis | Dependents, dependencies, blast radius |

`update_knowledge` deferred to second iteration (P1). Core 5 tools ship first.

**5. Multi-Repository Support**
- Namespace isolation per repository
- CLI: `seshat --repo /path/to/project`
- Independent knowledge graphs per repo
- Serve multiple repositories simultaneously

**6. Language Support (MVP)**
- **Rust** — self-hosted testing
- **TypeScript** — frontend and Node.js
- **JavaScript** — legacy and mixed projects
- **Python** — backend, data, scripting

All via Tree-sitter grammars compiled into the binary.

**7. Transport Layer**
- stdio, SSE, and HTTP transports out of the box (via Rust MCP library)

**8. Semantic Search (Optional)**
- Hybrid search: structural graph + FTS5 (default, zero-config) + vector similarity (optional)
- Vector search activates when user configures an embedding provider (Ollama locally or API)
- Trait-based abstraction for embedding providers — extensible without core changes

**9. CLI Interface**
- `seshat scan <path>` — initial scan with analysis report
- `seshat serve` — start MCP server
- `seshat status` — indexed projects and graph statistics

### Out of Scope for MVP

- **Identity knowledge type** — "what a module is and does" — v1.1
- **`update_knowledge` tool** — explicit knowledge updates — second iteration (P1)
- **Adaptive Learning** — auto-learning from corrections — v2.0
- **Web UI / Visualization** — graph dashboard — v2.0
- **CI/CD Integration** — convention checks in pipelines — v1.x
- **Team/Shared Knowledge** — shared graphs, conflict resolution — v1.x
- **Languages beyond MVP 4** — added based on demand
- **Monetization** — none planned

### MVP Success Criteria

1. **End-to-end flow works**: scan → connect MCP → agent uses tools → better code
2. **Self-hosting validated**: Seshat used on its own codebase + at least one TS/Python project
3. **Convention detection accuracy**: >80% of auto-detected conventions confirmed by developer
4. **`validate_approach` delivers value**: graduated responses with reasoning demonstrably prevent violations
5. **Performance**: full scan of 100k LOC < 60 seconds; MCP tool responses < 500ms
6. **Zero-config promise**: download → first useful MCP interaction in < 5 minutes

### Known Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Convention detection accuracy too low | Core value proposition fails | Start with frequency analysis on 8 specific detector types; iterate based on real-world testing on reference projects |
| Embedding provider complexity | Breaks zero-config promise | Make optional; default to FTS5 which works without any config |
| Knowledge graph query performance at scale | Slow MCP responses | SQLite with proper indexing; benchmark on 100k+ LOC projects early |
| Tree-sitter grammar coverage gaps | Missing language features | Use well-maintained grammars; contribute upstream if needed |

### Future Vision

**v1.1 — Explicit Knowledge:**
- `update_knowledge` tool with Decision and Preference types
- Identity knowledge type for module-level understanding
- Onboarding template for structured preferences input

**v2.0 — Intelligence:**
- Adaptive learning from developer corrections
- Pattern drift detection
- Retrospective analysis and periodic reports
- Convention conflict detection and resolution

**v3.0 — Ecosystem:**
- Web dashboard with graph visualization
- IDE plugins with inline convention hints
- Convention packs marketplace (community-shared templates)
- Embeddable library mode
- Multi-repo cross-referencing

**Long-term:** Seshat becomes the standard intelligence layer between AI coding agents and codebases — as essential as a linter or formatter.
