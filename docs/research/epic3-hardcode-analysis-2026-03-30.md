# Epic 3 Detectors — Hardcode Analysis & Improvement Plan

**Date:** 2026-03-30
**Context:** Analysis of hardcoded values across all 8 convention detectors in `seshat-detectors` crate, competitive research, and prioritized improvement plan.

---

## Executive Summary

All 8 detectors in Epic 3 rely on hardcoded library names, path patterns, and domain mappings. This is an industry-wide pattern — both `codebase-context` (PatrickSys) and `codebase-memory-mcp` (DeusData/CBM) use the same approach (~2500+ lines of hardcoded values in CBM, static `LIBRARY_CATEGORIES` map in codebase-context). **No competitor has solved this dynamically.**

Our strategy: **hybrid approach** — keep known-library lists for high-confidence matches, add heuristic fallbacks for unknown libraries, and defer config-file externalization to a future iteration.

---

## Hardcode Inventory

| Detector | File | Hardcoded Items | Severity |
|----------|------|----------------|----------|
| Dependency Usage | `dependency_usage.rs` | ~130 package names across 8 domains × 3 language groups | **High** |
| Import Organization | `import_organization.rs` | ~95 Python stdlib + ~33 Node.js builtins + Rust stdlib | Medium (justified) |
| File Structure | `file_structure.rs` | ~80 directory/config names | Low (justified) |
| Logging & Observability | `logging_observability.rs` | ~30 package names + family groupings | **High** |
| Test Patterns | `test_patterns.rs` | ~25 package names + ~15 path/function patterns | **High** |
| Error Handling | `error_handling.rs` | ~30 (exception names + library names) | **High** |
| Naming Conventions | `naming.rs` | ~15 special filenames + convention rules | Low (justified) |
| Export Patterns | `export_patterns.rs` | ~4 (barrel stems, module root files) | Low |

---

## Per-Detector Analysis & Decisions

### 1. Error Handling Detector (`error_handling.rs`)

**Problem:**
- **Rust:** Only checks `thiserror` and `anyhow`. Missing: `eyre`/`color-eyre`, `miette`, `snafu`, `error-stack`, `displaydoc`.
- **Python:** `PYTHON_BUILTIN_EXCEPTIONS` list (23 items) is incomplete — missing ~20+ standard exceptions.

**Decision — DO NOW:**
- **Rust:** Add heuristic: any crate that is imported AND from which `derive(Error)` or `impl Error` occurs = error handling library. For known libs (thiserror, anyhow, eyre, snafu, miette) → named finding. For unknown → generic "uses error derive crate: X".
- **Python:** Keep builtin exception list (language spec is fixed — hardcode justified). Also add heuristic: if a class inherits from something with `Error`/`Exception` in the name → custom exception (regardless of whether parent is in our list).

**Decision — LATER:**
- Allow extending exception/library lists via `seshat.toml` config.

---

### 2. Import Organization Detector (`import_organization.rs`)

**Problem:** ~95 Python stdlib modules, ~33 Node.js builtins hardcoded.

**Decision — LEAVE AS IS:**
- Python stdlib and Node.js builtins are **fixed per language version** — hardcode is the standard approach for static analysis tools. Both `isort` and `eslint` do the same.
- Rust stdlib roots (`std`, `core`, `alloc`) — 3 values, trivially justified.

**Decision — LATER:**
- Allow extending stdlib lists via `seshat.toml` (e.g., custom internal module prefixes).

---

### 3. Naming Conventions Detector (`naming.rs`)

**Problem:**
- Only analyzes: function names, type names, file names, constants.
- **Missing:** function parameter naming conventions.
- `Function` struct in `seshat-core` has **no `parameters` field**.
- Tree-sitter parsers do **not extract parameter names** currently.

**"single_lower_word" / "single_upper_word" clarification:**
- `single_lower_word`: a single word, all lowercase (e.g., `get`, `run`, `parse`) — ambiguous between snake_case, camelCase, and kebab-case.
- `single_upper_word`: a single word, all uppercase (e.g., `IO`, `HTTP`) — ambiguous between SCREAMING_SNAKE_CASE and PascalCase.
- These are kept separate to avoid polluting adoption statistics.

**Decision — DO NOW:**
- Add `parameters: Vec<String>` field to `Function` struct in `seshat-core/src/ir.rs`.
- Update all 4 tree-sitter parsers to extract function parameter names from AST.
- Add parameter name case analysis to naming detector.

**Decision — NOT DOING:**
- Local variable naming analysis — too much noise, not enough signal.

---

### 4. Export Patterns / ESM (`export_patterns.rs`)

**Problem:**
- `JavaScriptIR::module_system` field (ESM/CommonJS/Unknown) is **never read**.
- `JavaScriptIR::require_calls` is **never read**.
- Mixed ESM + CommonJS in one file is **not flagged**.

**Decision — DO NOW:**
- Start using `JavaScriptIR::module_system` to emit "project uses ESM/CommonJS/mixed" finding.
- Flag mixed ESM/CJS in same file as Observation.

---

### 5. Logging & Observability Detector (`logging_observability.rs`)

**Problem:** ~30 hardcoded library names. New libraries won't be detected.

**Decision — DO NOW (hybrid approach):**
1. **Keep known libraries** — high-confidence named findings for tracing, log, slog, winston, pino, bunyan, loguru, structlog, etc.
2. **Add name-based heuristic:** if dependency name contains `log`, `logger`, `logging`, `trace`, `tracing`, `observ` → likely logging library → lower-confidence finding.
3. **Add API shape heuristic:** if imported module's usage includes calls to `.info()`, `.debug()`, `.warn()`, `.error()`, `.fatal()`, `.trace()` → structured logging indicator.

---

### 6. Test Patterns Detector (`test_patterns.rs`)

**Problem:** Hardcoded Jest/Vitest/Mocha (JS), pytest/unittest (Python). Other frameworks undetected.

**Decision — DO NOW (hybrid approach):**
1. **Keep known frameworks** — high-confidence named detection.
2. **Config file detection:** if `jest.config.*` exists → Jest. If `vitest.config.*` → Vitest. If `[tool.pytest]` in pyproject.toml → pytest.
3. **Unknown framework fallback:** if file is in test directory AND has test-prefixed functions, but framework unidentified → report "uses testing (framework unknown)".
4. **Dependency name heuristic:** if dependency contains `test`, `mock`, `assert`, `spec` → likely testing-related.

---

### 7. File Structure Detector (`file_structure.rs`)

**Decision — LEAVE AS IS:**
- Directory names like `models/`, `controllers/`, `services/`, `domain/`, `infrastructure/` are **architectural patterns**, not library names. They are finite and stable.

---

### 8. Dependency Usage Detector (`dependency_usage.rs`)

**Problem:** ~130 package names across 8 domains × 3 language groups. Heaviest hardcode concentration.

**Decision — DO NOW (hybrid approach):**
1. **Keep known package mappings** — well-known libraries stay for high-confidence detection.
2. **Add name-based heuristic fallback** for unrecognized packages:
   - Contains `test`/`mock`/`assert`/`spec` → Testing domain
   - Contains `log`/`logger`/`trace` → Logging domain
   - Contains `http`/`web`/`api`/`rest`/`fetch` → HTTP domain
   - Contains `sql`/`db`/`database`/`orm` → Database domain
   - Contains `cli`/`command`/`arg` → CLI domain
   - Contains `serial`/`json`/`yaml`/`toml`/`proto` → Serialization domain
   - Contains `valid`/`schema` → Validation domain
3. **Confidence tier:** Known mapping = High confidence, Heuristic = Low confidence.

**Decision — LATER:**
- Manifest enrichment: parse Cargo.toml `categories`, package.json `keywords`, pyproject.toml `classifiers`.
- Externalize mappings to `.toml` data files.

---

## Competitive Context

### codebase-context (PatrickSys)
- Fully hardcoded `LIBRARY_CATEGORIES` map + regex-based framework analyzers.
- No heuristics for unknown libraries — unknown packages get `'other'`.
- Innovation is in statistical layer: adoption %, git trends (P90), golden file scoring.

### codebase-memory-mcp (DeusData/CBM)
- Entirely hardcoded. `CBMLangSpec` tables (~2500 lines) + `service_patterns.c` (~450 lines).
- Only 4 detection categories (HTTP, Async, Config, Route). No logging/testing/error handling detection.
- One dynamic element: decorator tag extraction via word frequency.

**Nobody has solved the general problem. Our hybrid approach will be ahead.**

---

## Architecture: Three-Level Detection Model

| Level | Source | Confidence | Example |
|-------|--------|------------|---------|
| **L1: Known** | Hardcoded known library names | High | `"tracing"` → Logging |
| **L2: Heuristic** | Package name patterns, API shape, inheritance | Medium-Low | Name contains `"log"` + calls `.info()` |
| **L3: Manifest** | Package metadata (categories, keywords, classifiers) | Medium | Cargo.toml category = `"development-tools::testing"` |

Current epic: L1 (done) + L2 (this iteration).
Future epic: L3 (manifest enrichment).

---

## Implementation Plan

### Phase 1: DO NOW — Structural Changes

| # | Task | Files | Effort |
|---|------|-------|--------|
| 1.1 | Add `parameters: Vec<String>` to `Function` struct | `seshat-core/src/ir.rs` | S |
| 1.2 | Extract parameter names in Rust tree-sitter parser | `seshat-scanner/src/parser/rust_parser.rs` | M |
| 1.3 | Extract parameter names in Python tree-sitter parser | `seshat-scanner/src/parser/python_parser.rs` | M |
| 1.4 | Extract parameter names in JS tree-sitter parser | `seshat-scanner/src/parser/javascript_parser.rs` | M |
| 1.5 | Extract parameter names in TS tree-sitter parser | `seshat-scanner/src/parser/typescript_parser.rs` | M |
| 1.6 | Add parameter naming analysis to naming detector | `seshat-detectors/src/naming.rs` | M |

### Phase 2: DO NOW — Heuristic Fallbacks

| # | Task | Files | Effort |
|---|------|-------|--------|
| 2.1 | Rust error: derive(Error)/impl Error heuristic + add eyre/snafu/miette | `error_handling.rs` | M |
| 2.2 | Python error: inheritance-based custom exception detection | `error_handling.rs` | S |
| 2.3 | Logging: name-based + API shape heuristic for unknown loggers | `logging_observability.rs` | M |
| 2.4 | Testing: config file detection + unknown framework fallback | `test_patterns.rs` | M |
| 2.5 | Dependencies: name-based domain classification fallback | `dependency_usage.rs` | M |
| 2.6 | Exports: use `module_system` field, flag mixed ESM/CJS | `export_patterns.rs` | S |

### Phase 3: LATER — Config & Manifest Enrichment

| # | Task | Effort |
|---|------|--------|
| 3.1 | Allow extending stdlib/known-library lists via `seshat.toml` | M |
| 3.2 | Parse Cargo.toml `categories` for auto domain classification | L |
| 3.3 | Parse package.json `keywords` for auto domain classification | M |
| 3.4 | Parse pyproject.toml `classifiers` for auto domain classification | M |
| 3.5 | Externalize all L1 mappings to `.toml` data files | M |

---

## Files Changed Summary (Phase 1+2)

- `crates/seshat-core/src/ir.rs` — add `parameters` field to `Function`
- `crates/seshat-scanner/src/parser/rust_parser.rs` — extract param names
- `crates/seshat-scanner/src/parser/python_parser.rs` — extract param names
- `crates/seshat-scanner/src/parser/javascript_parser.rs` — extract param names
- `crates/seshat-scanner/src/parser/typescript_parser.rs` — extract param names
- `crates/seshat-detectors/src/naming.rs` — parameter naming analysis
- `crates/seshat-detectors/src/error_handling.rs` — Rust heuristic + Python inheritance
- `crates/seshat-detectors/src/logging_observability.rs` — heuristic fallbacks
- `crates/seshat-detectors/src/test_patterns.rs` — config file + fallback
- `crates/seshat-detectors/src/dependency_usage.rs` — name-based domain heuristic
- `crates/seshat-detectors/src/export_patterns.rs` — use module_system field

---

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Heuristics produce false positives (e.g., `logformat` as logging) | Low-confidence findings; known-library findings always take priority |
| Parameter extraction increases parse time | Only extract names (strings), not types — minimal overhead |
| Python builtin list gets outdated with new Python versions | Covers 3.8–3.13; expand per release |
| Name-based heuristics are English-centric | Acceptable — package ecosystem is English-dominated |
