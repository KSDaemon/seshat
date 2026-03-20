---
stepsCompleted: [step-01-init, step-02-discovery, step-02b-vision, step-02c-executive-summary]
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
- **Dual Requirements:** Agent-facing (structured MCP responses, <500ms latency, parseable output schema) + Developer-facing (CLI UX, analysis reports, configuration, terminal formatting)
