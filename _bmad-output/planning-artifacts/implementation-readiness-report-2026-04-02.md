# Implementation Readiness Assessment Report

**Date:** 2026-04-02
**Project:** Seshat
**Scope:** Epic 4 (post-polish) through Epic 5, with forward-look at Epic 6

---

## Document Inventory

| Document | Location | Lines | Status |
|----------|----------|-------|--------|
| PRD | `_bmad-output/planning-artifacts/prd.md` | 805 | Current |
| Architecture | `_bmad-output/planning-artifacts/architecture.md` | 1248 | Minor drift (see C1-C3) |
| Epics & Stories | `_bmad-output/planning-artifacts/epics.md` | 1288 | Needs update (see below) |
| UX Design | `_bmad-output/planning-artifacts/ux-design-specification.md` | 684 | Current |
| Epic 5 PRD | `.ralph/tasks/prd-mcp-server-core-tools.md` | 395 | Needs update (see D2) |
| DB Discovery Spec | `_bmad-output/implementation-artifacts/tech-spec-serve-db-discovery.md` | 224 | Ready for implementation |

---

## PRD Analysis: M1 FR Coverage (Epic 5)

### Covered (13/19)

| FR# | Description | Covered By | Evidence |
|-----|-------------|------------|---------|
| FR31 | MCP server via stdio/SSE/HTTP | Epic 5 US-001,002 | `seshat-mcp/src/server.rs` — stdio working. SSE/HTTP declared but not wired (see Gap 2). |
| FR32 | query_project_context | Epic 5 US-008 | `seshat-graph/src/project_context.rs` (728 lines), 15+ tests |
| FR33 | query_convention | Epic 5 US-009 | `seshat-graph/src/conventions.rs` (505 lines), FTS5, 17 tests |
| FR38 | Structured JSON responses | Epic 5 US-004 | `seshat-mcp/src/envelope.rs`, ResponseEnvelope<T>, 12 tests |
| FR39 | Informative error unscanned repo | Epic 5 US-003 | REPO_NOT_SCANNED error code |
| FR41 | seshat serve command | Epic 5 US-003 | `seshat-cli/src/serve.rs` — working but DB discovery broken (see Gap 1) |
| FR49 | FTS5 full-text search | Epic 5 US-006 | `seshat-graph/src/fts.rs` (356 lines), 8 tests |
| FR61 | Default scope = root | Epic 5 US-004 | `scope: "root"` hard-coded in envelope |
| FR64 | Golden files | Epic 5 US-007 | `seshat-graph/src/golden_files.rs` (187 lines), 7 tests |
| FR65 | record_decision | Epic 5 US-010 | `seshat-graph/src/decisions.rs`, 16 tests |
| FR66 | update/remove_decision | Epic 5 US-011 | Same file, soft-delete, 34 tests |
| FR67 | Wrapper/facade detection | Epic 3.5.5 | Implemented in dependency_usage detector |
| FR69 | metadata.next_steps | Epic 5 US-004 | All tools populate next_steps |

### Partially Covered (3/19)

| FR# | Description | Status | Notes |
|-----|-------------|--------|-------|
| FR47 | Multiple repos simultaneously | ⚠️ PARTIAL | Separate .db files exist per project. No multi-DB routing. Single-repo mode only. Deferred to Epic 6. |
| FR48 | Independent knowledge graphs per repo | ⚠️ PARTIAL | Architecture supports it. Routing not implemented. |
| FR57 | Repo identification by physical path | ⚠️ PARTIAL | DB named by project. Smart discovery in tech spec. |

### Deferred (3/19)

| FR# | Description | Deferred To | Forward Compat |
|-----|-------------|-------------|----------------|
| FR58 | Submodule child knowledge graphs | Epic 6 | `submodules: []` placeholder |
| FR59 | Auto-scope from file path | Epic 6 | `scope` param in tech spec |
| FR62 | Submodule relationship metadata | Epic 6 | Empty array in responses |

---

## Critical Gaps

### Gap 1: `seshat serve` DB Discovery Broken (HIGH)

**Current:** `discover_db()` picks most recently modified `.db` — silently serves wrong project when multiple exist.

**Fix:** Tech spec `tech-spec-serve-db-discovery.md` — smart resolution: explicit `repo` arg → cwd → git root → single DB → error with list. **Ready for implementation.**

### Gap 2: SSE/HTTP Transport Not Wired (MEDIUM)

**PRD FR31** requires stdio + SSE + HTTP. Currently only `start_stdio_with_shutdown()` is implemented. `ServerConfig.transports` and `host`/`port` fields exist but nothing creates SSE/HTTP listeners.

**Recommendation:** For M1, document stdio-only as delivered. SSE/HTTP requires rmcp transport setup (likely `rmcp::transport::sse::SseServer` or similar). Add as a follow-up story or defer to Epic 6 alongside multi-repo daemon mode.

### Gap 3: Tool Schemas Missing `repo`/`scope` Parameters (MEDIUM)

**Current:** All 5 tool request structs lack `repo` and `scope` fields. When Epic 6 ships, tool schemas will change — breaking cached schemas.

**Fix:** Tech spec `tech-spec-serve-db-discovery.md` Task 4 adds optional, ignored `repo`/`scope` to all 5 structs. **Ready for implementation.**

---

## Discrepancies (Architecture vs Implementation)

### C1: File Structure Drift (LOW)

Architecture specifies `seshat-graph/src/queries/` subdirectory and `seshat-mcp/src/tools/convention.rs`. Actual: flat structure in graph, `query_convention.rs` in tools. Crate boundary principle correctly maintained.

### C2: Migration Numbering (LOW)

Architecture says `V2__add_fts5.sql`. Actual: `V4__add_conventions_fts.sql` (V2=package_metadata, V3=file_dates added by Epic 3.5). No functional impact.

### C3: Port Number Updated (RESOLVED)

Architecture had `39271`. Updated to `6174` (Kaprekar's constant) in code + docs. Architecture doc still has old value in UX examples.

---

## Epic 4 Post-Implementation Notes

Epic 4 is marked **[COMPLETED]** in epics.md. Post-implementation UX polish was done in the current session:

- **Progress indicators:** All scan phases now use uniform braille spinners (discovery, git history, scanning, module graph, manifests/docs, analysis). No progress bars.
- **Convention aggregation:** 35+ descriptions across 6 detectors fixed to use generalized descriptions for proper grouping.
- **Submodule exclusion:** `.gitmodules` parsing, `--include-submodules` flag, exclusion info in summary.
- **Report alignment:** Dynamic column width for conventions, alphabetical secondary sort, UTF-8 safe truncation.

These changes are committed but **not reflected in epics.md** — should be added as a note under Epic 4.

---

## Recommendations

### R1: Implement tech-spec-serve-db-discovery.md (HIGH, ~200 LOC)
Fixes Gap 1 and Gap 3. Smart DB discovery + forward-compatible tool schemas. This is the highest-priority remaining work for Epic 5.

### R2: Document SSE/HTTP as deferred (MEDIUM)
Add a note to Epic 5 in epics.md: "SSE/HTTP transports declared in config but not wired in M1. stdio transport operational. SSE/HTTP activation deferred to Epic 6 (daemon mode)."

### R3: Update epics.md (MEDIUM)
- Add Epic 4 UX polish note
- Mark Epic 5 stories 5.1-5.7 as **[COMPLETED]**
- Add Story 5.8 for tech-spec-serve-db-discovery.md work
- Note SSE/HTTP deferral

### R4: Update architecture.md (LOW)
Fix migration numbering, file structure, port number. Non-blocking.

---

## Overall Readiness Assessment

### **CONDITIONAL GO**

**Epic 5 core value is delivered:** AI agents can connect via stdio, query project context, search conventions, record/update/remove decisions. 94+ unit tests pass. Response envelope, FTS5, golden files, convention persistence — all operational.

**Conditions for full GO:**
1. Implement tech-spec-serve-db-discovery.md (fixes broken serve + adds forward-compat)
2. Document SSE/HTTP transport as deferred

**Does NOT block GO:**
- Multi-repo routing (Epic 6)
- Submodule scoping (Epic 6)
- Architecture doc drift
- SSE/HTTP transport (not needed for typical MCP usage)
