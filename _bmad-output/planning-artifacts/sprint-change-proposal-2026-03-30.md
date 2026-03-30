# Sprint Change Proposal — Competitive Analysis Retrofit

**Date:** 2026-03-30
**Trigger:** Strategic pause to analyze 8 competing/analogous projects before proceeding with Epic 4+
**Scope:** Moderate — requires new epic (3.5) and modifications to Epics 5 and 7
**Approved by:** Kostik (via interactive Party Mode session)

---

## Issue Summary

After implementing Epics 1-3 (infrastructure, scanning, convention detection), the team paused to research the competitive landscape. Analysis of 8 projects (codebase-context, megamemory, codebase-memory-mcp, axon, code-review-graph, socraticode, octocode-mcp, lsp-mcp) revealed that:

1. **No existing tool performs automated coding convention detection** — Seshat's core differentiator is validated
2. Several tools have complementary features worth adopting: pattern trends, evidence gating, golden files, user-recorded decisions, wrapper detection
3. Seshat's hardcoded package-to-domain mappings (~200 names per language, duplicated across 2 files) are a maintenance dead-end

Full analysis: `docs/research/competitive-analysis-2026-03-30.md`

---

## Impact Analysis

### Epic Impact

| Epic | Impact | Description |
|------|--------|-------------|
| Epic 1 | None | Already implemented, no changes needed |
| Epic 2 | Retrofit via 3.5 | Scanner needs git date collection + registry metadata |
| Epic 3 | Retrofit via 3.5 | Detectors need unified taxonomy + wrapper detection + trend computation |
| **Epic 3.5 (NEW)** | **New epic** | 5 stories to retrofit existing code with competitive analysis findings |
| Epic 4 | Indirect | Will benefit from enriched convention data (trends, golden files) |
| Epic 5 | Modified | 3 new stories (5.5-5.7) for record_decision tools; modified stories 5.2-5.4 |
| Epic 7 | Modified | Story 7.2 enhanced with evidence gating (ready/whatWouldHelp) |
| Epics 6, 8-11 | None | No impact |

### Artifact Changes

| Artifact | Changes |
|----------|---------|
| **PRD** | +8 FRs (FR63-FR70): trends, golden files, record_decision, wrapper detection, registry metadata, next-step hints, evidence gating |
| **Architecture** | +5 ADRs (24-28): trends, registry metadata, embeddings deferred, record_decision, wrapper detection |
| **Epics** | +1 new epic (3.5, 5 stories), +3 stories in Epic 5, enhanced ACs in Epics 5 and 7 |

---

## Recommended Approach: Direct Adjustment

### Rationale

- Epics 1-3 are implemented but Epic 4+ has not started — ideal time for retrofit
- Changes to existing code are targeted (new columns, new modules, refactored enums) — not architectural rewrites
- New epic (3.5) sequences correctly: after existing code, before new features that depend on enriched data
- No rollback needed — all changes are additive

### Effort Estimate

| Story | Effort | Priority |
|-------|--------|----------|
| 3.5.1: Unify dependency taxonomy | Small (1-2 days) | High (blocks 3.5.2) |
| 3.5.2: Package registry metadata | Medium (3-4 days) | High (blocks quality of 3.5.5) |
| 3.5.3: Git file dates collection | Small (1-2 days) | High (blocks 3.5.4) |
| 3.5.4: Convention trend computation | Small (1-2 days) | High |
| 3.5.5: Wrapper/facade detection | Medium (2-3 days) | High |
| 5.5: record_decision tool | Medium (2-3 days) | High |
| 5.6: update/remove_decision tools | Small (1-2 days) | Medium |
| 5.7: Agent protocol docs | Small (1 day) | Medium |
| 5.2-5.4 AC enhancements | Small (1 day each) | Medium |
| 7.2 evidence gating | Small (1-2 days) | High |

**Total estimated effort:** ~3 weeks for Epic 3.5, ~1 week for Epic 5 additions

### Risk Assessment

- **Low risk:** All changes are additive, no destructive modifications to existing data
- **Medium risk:** Package registry API integration requires HTTP calls during scan — need robust error handling and offline fallback
- **Low risk:** Git date collection via gix — we already depend on gix, this is a natural extension

---

## Implementation Handoff

### Execution Order

1. **Epic 3.5** (before Epic 4):
   - Story 3.5.1 → 3.5.2 → 3.5.3 → 3.5.4 → 3.5.5 (sequential dependencies)
2. **Epic 4** (CLI Scan Report) — now benefits from trends and enriched data
3. **Epic 5** (MCP Server) — includes new stories 5.5-5.7
4. **Epic 7** (Advanced Tools) — includes enhanced 7.2

### Success Criteria

- `cargo test --workspace` passes after each story
- `cargo clippy --all-targets -- -D warnings` passes
- Package registry lookups cached correctly (verify with `sqlite3` inspection)
- Convention trends computed correctly at threshold boundaries
- Wrapper detection works on the PR #589 pattern (utc_now wrapper)
- Unified dependency taxonomy has no duplicated enums
