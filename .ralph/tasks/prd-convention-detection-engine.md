# PRD: Epic 3 — Convention Detection Engine (Stories 3.1–3.10)

## Introduction

**Type:** Feature

Implement the convention detection engine: a trait-based pipeline that runs 8 pluggable detectors over parsed IR to automatically discover coding conventions — import patterns, error handling, naming, dependencies, exports, logging, tests, and file structure — assigning frequency-based confidence scores and cross-referencing findings with documentation. This is the intelligence layer that transforms raw parsed data into actionable knowledge for AI agents.

**Context:** Seshat is a Rust CLI + MCP server. Epic 1 (infrastructure) and Epic 2 (scanning) are complete: 9-crate workspace, core types/IR, SQLite schema, repository CRUD, 4-language Tree-sitter parsers, manifest analysis, documentation ingestion, and knowledge graph persistence are all implemented. The `seshat-detectors` crate is scaffolded with `error.rs` and test fixtures, but has no detector implementations. All core types (`ConventionFinding`, `DetectorResults`, `CodeEvidence`, `KnowledgeNature`, `KnowledgeWeight`, `ProjectFile`, `LanguageIR`) are defined in `seshat-core`. See `_bmad-output/planning-artifacts/architecture.md` for ADRs, specifically ADR-6 (parallel scanning), ADR-7 (confidence scoring), ADR-8 (cross-language IR), ADR-17 (DetectorResult type).

## Goals

- Define a `ConventionDetector` trait and detection pipeline that orchestrates all detectors (Story 3.1)
- Implement frequency-based confidence scoring with configurable thresholds (Story 3.1)
- Detect canonical libraries per domain from dependency usage (Story 3.2)
- Detect import grouping and ordering patterns across 4 languages (Story 3.3)
- Detect error handling patterns: error types, propagation, wrapping (Story 3.4)
- Detect naming conventions for files, functions, types, constants (Story 3.5)
- Detect export patterns: default vs named, barrel exports, pub/mod (Story 3.6)
- Detect logging library and structured vs unstructured preference (Story 3.7)
- Detect testing framework, file placement, and naming conventions (Story 3.8)
- Detect file/directory organization patterns (Story 3.9)
- Cross-reference code conventions with documentation to surface contradictions (Story 3.10)

## User Stories

### US-001: ConventionDetector Trait & Detection Pipeline (Story 3.1)

**Description:** As a developer, I want a trait-based detection pipeline that runs all detectors on parsed IR so that adding new detectors requires no changes to core scanning logic.

**Acceptance Criteria:**
- [ ] `ConventionDetector` trait defined in `crates/seshat-detectors/src/trait.rs`:
  ```rust
  pub trait ConventionDetector: Send + Sync {
      fn name(&self) -> &'static str;
      fn detect(&self, file: &ProjectFile) -> Vec<ConventionFinding>;
      fn supported_languages(&self) -> &[Language];
  }
  ```
- [ ] `run_all_detectors(files: &[ProjectFile], config: &DetectionConfig) -> Vec<DetectorResults>` orchestrates all registered detectors
- [ ] Files processed in parallel via `rayon::par_iter()` (ADR-6)
- [ ] Detectors run sequentially per file (parallelizing within a file adds overhead without benefit)
- [ ] Only detectors whose `supported_languages()` includes the file's language are executed
- [ ] A failing detector logs `tracing::warn` and is skipped for that file — does not crash the pipeline
- [ ] `confidence.rs` implements frequency-based scoring: `adoption_count / total_count` (ADR-7)
- [ ] `aggregate_findings(findings: &[ConventionFinding]) -> Vec<AggregatedConvention>` groups findings by `detector_name + description`, computes adoption counts
- [ ] Configurable weight thresholds from `DetectionConfig`: >0.85 Strong, 0.50–0.85 Moderate, 0.20–0.50 Weak, <0.20 Info
- [ ] `all_detectors()` returns `Vec<Box<dyn ConventionDetector>>` with all 8 detectors registered
- [ ] Unit test: pipeline runs on empty file list without error
- [ ] Unit test: failing detector is skipped, other detectors still run
- [ ] Unit test: confidence scoring produces correct weight mapping at boundaries
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-002: Dependency Usage Detector (Story 3.2)

**Description:** As a developer, I want Seshat to detect canonical libraries per domain so that AI agents use the right libraries.

**Acceptance Criteria:**
- [ ] `dependency_usage.rs` implements `ConventionDetector` for all 4 languages
- [ ] Groups detected `DependencyUsage` entries from IR by domain (HTTP, logging, testing, validation, serialization, database, CLI, async runtime, etc.)
- [ ] Domain classification uses a curated mapping of known crate/package names to domains (e.g., `reqwest`/`hyper` → HTTP, `tracing`/`log` → logging, `tokio`/`async-std` → async)
- [ ] Most-used library per domain identified as canonical (highest import count across files)
- [ ] Conflicting libraries for the same domain flagged as `Observation` findings (e.g., both `log` and `tracing` used)
- [ ] Dead dependencies (declared in manifest but never imported in code) flagged — requires cross-referencing manifest data passed alongside IR
- [ ] Each finding includes `CodeEvidence` with representative import lines
- [ ] Unit tests with fixture files in `tests/fixtures/rust_samples/`, `tests/fixtures/typescript_samples/`, `tests/fixtures/python_samples/`
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-003: Import Organization Detector (Story 3.3)

**Description:** As a developer, I want Seshat to detect import grouping and ordering patterns so that AI agents follow the project's import style.

**Acceptance Criteria:**
- [ ] `imports.rs` implements `ConventionDetector` for all 4 languages
- [ ] Detects grouping pattern: stdlib → external → internal (with blank line separators)
- [ ] Detects presence/absence of consistent grouping across files
- [ ] Rust-specific: `use` statement grouping (std/core → external crates → crate/super/self)
- [ ] TypeScript/JavaScript-specific: barrel vs direct import preference, type-only import separation (`import type`)
- [ ] Python-specific: `import` vs `from ... import` preference, `isort`-style grouping
- [ ] Each finding includes `CodeEvidence` with actual import blocks showing the detected pattern
- [ ] `follows_convention: bool` correctly set per file (does this file follow the majority pattern?)
- [ ] Unit tests per language with fixture sample files
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-004: Error Handling Detector (Story 3.4)

**Description:** As a developer, I want Seshat to detect error handling patterns so that AI agents use consistent error handling.

**Acceptance Criteria:**
- [ ] `error_handling.rs` implements `ConventionDetector` for all 4 languages
- [ ] Rust: detects `thiserror` vs `anyhow` vs custom error enums; detects `?` propagation; detects error wrapping patterns (`map_err`, `context`)
- [ ] TypeScript: detects custom error classes vs plain `Error`; detects try-catch patterns; detects Result/Either patterns if used
- [ ] JavaScript: detects error handling style (try-catch, callback errors, Promise rejection)
- [ ] Python: detects exception hierarchy (custom vs built-in); detects try-except patterns; detects error wrapping
- [ ] Findings include code examples of the dominant error handling pattern
- [ ] Uses `RustIR.error_types` field from parsed IR where available
- [ ] Unit tests per language using existing fixture files (`thiserror_errors.rs`, `custom_errors.ts`)
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-005: Naming Conventions Detector (Story 3.5)

**Description:** As a developer, I want Seshat to detect naming conventions so that AI agents follow consistent naming.

**Acceptance Criteria:**
- [ ] `naming.rs` implements `ConventionDetector` for all 4 languages
- [ ] Detects naming conventions for: files, functions, types/classes, constants, variables (where extractable from IR)
- [ ] Analyzes function names from `ProjectFile.functions`, type names from `ProjectFile.types`
- [ ] Detects case patterns: snake_case, camelCase, PascalCase, SCREAMING_SNAKE_CASE, kebab-case (files)
- [ ] Language-aware weighting: Rust naming conventions weighted lower (compiler/clippy already enforce them); JS/Python/TS weighted higher (more variation in practice)
- [ ] Findings describe the dominant pattern with adoption percentage
- [ ] Unit tests with mixed-style fixture files
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-006: Export Patterns Detector (Story 3.6)

**Description:** As a developer, I want Seshat to detect export patterns so that AI agents create consistent module boundaries.

**Acceptance Criteria:**
- [ ] `exports.rs` implements `ConventionDetector` for all 4 languages
- [ ] TypeScript/JavaScript: detects default vs named export preference with adoption rate
- [ ] TypeScript/JavaScript: detects barrel export pattern (re-exports from index.ts/index.js) via `TypeScriptIR.has_barrel_exports` / file path check
- [ ] Rust: detects `pub` usage patterns, `mod` re-export patterns
- [ ] Python: detects `__all__` usage pattern via `PythonIR.has_all_export`
- [ ] Each finding includes representative code evidence
- [ ] Unit tests using existing fixture files (`barrel_exports.ts`)
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-007: Logging & Observability Detector (Story 3.7)

**Description:** As a developer, I want Seshat to detect logging patterns so that AI agents use the right logging library and format.

**Acceptance Criteria:**
- [ ] `logging.rs` implements `ConventionDetector` for all 4 languages
- [ ] Identifies canonical logging library (e.g., `tracing` vs `log` for Rust, `winston` vs `pino` for JS/TS, `logging` vs `loguru` for Python)
- [ ] Detects structured vs unstructured logging preference (structured = fields/key-value, unstructured = string interpolation)
- [ ] Conflicting logging libraries flagged as `Observation`
- [ ] Uses `DependencyUsage` from IR to identify which logging library is imported in each file
- [ ] Unit tests using existing fixture files (`tracing_logging.rs`, `logging_patterns.py`)
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-008: Test Patterns Detector (Story 3.8)

**Description:** As a developer, I want Seshat to detect testing conventions so that AI agents write tests matching project style.

**Acceptance Criteria:**
- [ ] `tests_pattern.rs` implements `ConventionDetector` for all 4 languages
- [ ] Identifies testing framework (Rust built-in `#[test]`, Jest, Mocha, pytest, unittest)
- [ ] Detects test file placement: co-located (`mod tests` in same file for Rust, `*.test.ts` next to source) vs separate (`tests/` directory)
- [ ] Detects test naming convention (e.g., `test_*`, `it('should ...')`, `describe/it`, `def test_*`)
- [ ] Detects setup/teardown patterns (before/after hooks, fixtures, test builders)
- [ ] Uses `DependencyUsage` to identify test framework imports
- [ ] Unit tests using existing fixture files (`jest_tests.ts`, `pytest_patterns.py`)
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-009: File Structure Detector (Story 3.9)

**Description:** As a developer, I want Seshat to detect file organization patterns so that AI agents place new files correctly.

**Acceptance Criteria:**
- [ ] `file_structure.rs` implements `ConventionDetector` for all 4 languages
- [ ] Detects directory organization pattern: by feature (e.g., `users/`, `orders/`), by type (e.g., `models/`, `controllers/`, `services/`), by layer (e.g., `domain/`, `infrastructure/`, `application/`)
- [ ] Identifies common directory conventions (e.g., `src/`, `lib/`, `tests/`, `utils/`, `helpers/`, `types/`)
- [ ] Detects configuration file placement patterns (root vs config directory)
- [ ] Analysis based on `ProjectFile.path` patterns across the entire file set, not individual file content
- [ ] Note: this detector operates on the collection of files, not per-file. The trait's `detect` method receives one file at a time, so this detector may need to accumulate state or use a separate aggregation pass. Document the chosen approach.
- [ ] Unit tests with fixture directory structures
- [ ] `cargo test -p seshat-detectors` passes
- [ ] `cargo clippy -p seshat-detectors -- -D warnings` passes

### US-010: Cross-Reference Code vs Documentation (Story 3.10)

**Description:** As a developer, I want Seshat to compare code conventions with documentation so that contradictions are surfaced.

**Acceptance Criteria:**
- [ ] `cross_reference.rs` in `seshat-detectors` (or `seshat-graph` per architecture — see Technical Considerations)
- [ ] Loads code-detected conventions (Nature = Convention/Observation) and documentation-sourced knowledge nodes (Nature = Fact/Rule)
- [ ] Compares via keyword/topic matching: doc node description vs convention description (no semantic/NLP — ADR-23)
- [ ] Matching conventions: confidence boosted (reinforcement)
- [ ] Contradictions: `Contradicts` edge created between the doc node and code convention node
- [ ] Contradictions will be surfaced in future `validate_approach` responses (Epic 7)
- [ ] Unit test: doc says "use X", code convention says "use Y" → Contradicts edge created
- [ ] Unit test: doc says "use X", code convention confirms X → confidence boosted
- [ ] `cargo test` passes for affected crate(s)
- [ ] `cargo clippy -- -D warnings` passes for affected crate(s)

### US-011: Branch Code Review & Quality Gate (Final Step)

**Description:** As a developer, I want a comprehensive code review of all changes in the feature branch before merging so that the code meets professional Rust quality standards.

**Acceptance Criteria:**

**Automated checks — all must pass:**
- [ ] `cargo fmt --all --check` — no formatting violations
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — no lint warnings
- [ ] `cargo test --workspace` — all tests pass (including integration tests)
- [ ] `cargo doc --no-deps --document-private-items` — documentation builds without warnings
- [ ] No new `unsafe` blocks added without justification
- [ ] No `.unwrap()` or `.expect()` in library code (only in tests)

**Manual diff review — full `git diff main...HEAD` analysis:**

_Idiomatic Rust:_
- [ ] Error handling uses `thiserror` for error types, `?` for propagation — no manual match-and-rewrap where `?` suffices
- [ ] Types leverage Rust's type system: enums for variants, newtypes for domain IDs, `Option` for nullable, `Result` for fallible
- [ ] Generics and `impl Trait` preferred over dynamic dispatch (`Box<dyn>`) unless trait objects are architecturally required (e.g., detector registry)
- [ ] Iterator chains preferred over imperative loops where they improve clarity
- [ ] `#[must_use]` on functions returning important values that callers should not silently discard
- [ ] Doc comments (`///`) on all public items; module-level `//!` doc comments on each module

_Performance:_
- [ ] No unnecessary allocations: prefer `&str` over `String` in function parameters, `&[T]` over `Vec<T>` where ownership is not needed
- [ ] No needless `.clone()` — use references or move semantics; verify each clone is necessary
- [ ] No redundant closures (clippy `redundant_closure`): prefer function references where applicable
- [ ] Collections pre-allocated with `Vec::with_capacity()` or `HashMap::with_capacity()` where size is known or estimable
- [ ] Hot paths avoid allocations: string formatting, evidence collection, confidence calculation

_Memory management:_
- [ ] No leaked resources: all file handles, DB connections, Tree-sitter parsers properly scoped with RAII
- [ ] No unbounded growth: verify collections that accumulate results (e.g., `Vec<ConventionFinding>`) are bounded by input size, not by processing iteration
- [ ] String data uses `Cow<'_, str>` or `&str` where appropriate to avoid unnecessary copying
- [ ] Large structures not passed by value where a reference suffices

_Code duplication:_
- [ ] No copy-pasted logic between detectors — shared patterns extracted into helper functions or common modules
- [ ] Per-language dispatch within a detector uses a match arm with shared logic, not duplicated code blocks
- [ ] Confidence scoring logic centralized in `confidence.rs`, not reimplemented per detector
- [ ] Evidence collection (snippet extraction, line range computation) reuses shared utilities

_Architecture compliance:_
- [ ] All code within `seshat-detectors` crate boundary — no SQL, no MCP, no CLI code leaked in
- [ ] Only depends on `seshat-core` (and `rayon`, `tracing`, `thiserror` as external deps)
- [ ] Follows established patterns from `seshat-core` and `seshat-scanner`: error types per crate, `#[tracing::instrument]` on public functions, `#[cfg(test)] mod tests` at file bottom
- [ ] Serde annotations: `#[serde(rename_all = "snake_case")]` on all new structs, `#[serde(skip_serializing_if = "Option::is_none")]` on optional fields
- [ ] Naming follows workspace conventions: `snake_case` files, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants

_Test quality:_
- [ ] Tests are deterministic — no reliance on file system ordering, timing, or external state
- [ ] Test names are descriptive: `test_import_detector_groups_stdlib_then_external_then_internal`
- [ ] Edge cases covered: empty file, file with no imports, file with only comments, file with syntax error (graceful degradation)
- [ ] Fixture files used for realistic testing, not just synthetic data
- [ ] No test code in production modules (everything behind `#[cfg(test)]`)

## Functional Requirements

- FR-1: `ConventionDetector` trait defines a pluggable detection interface with name, detect, and supported_languages
- FR-2: Detection pipeline orchestrates all detectors in parallel over files (rayon) with sequential detector execution per file
- FR-3: Confidence scoring uses `adoption_count / total_count` with configurable weight thresholds (ADR-7)
- FR-4: Dependency usage detector identifies canonical libraries per domain, flags conflicts and dead deps
- FR-5: Import organization detector detects grouping/ordering patterns across 4 languages
- FR-6: Error handling detector identifies error type patterns, propagation style, wrapping conventions
- FR-7: Naming conventions detector detects case patterns with language-aware weighting
- FR-8: Export patterns detector identifies default vs named, barrel exports, pub/mod patterns
- FR-9: Logging detector identifies canonical logging library and structured vs unstructured preference
- FR-10: Test patterns detector identifies framework, file placement, naming, and setup/teardown conventions
- FR-11: File structure detector identifies directory organization patterns (by feature/type/layer)
- FR-12: Cross-reference logic compares code conventions with documentation, creates Contradicts edges for conflicts
- FR-13: Failing detector does not crash the pipeline — logged and skipped (graceful degradation)

## Non-Goals

- No MCP server integration (Epic 5) — detectors produce `ConventionFinding` structs, not JSON responses
- No CLI report formatting (Epic 4) — no colored output or progress bars
- No file watching or incremental updates (Epic 9) — detection runs as a batch over a set of IR
- No semantic/NLP analysis — cross-reference uses keyword/topic matching only (ADR-23)
- No call graph analysis — detectors operate on per-file IR, not inter-file relationships (deferred to M2+)
- No TUI review interface (Epic 11) — detectors don't interact with users
- No vector/embedding-based matching — FTS5 keyword matching only for cross-reference

## Developer Context

The implementing agent is a **professional Rust developer** writing **idiomatic Rust code**. This means:

- Use Rust's type system fully: enums for variants, newtypes for domain IDs, `Option` for nullable, `Result` for fallible operations
- Prefer `impl Trait` and generics over dynamic dispatch unless trait objects are architecturally required (detector registry uses `Box<dyn ConventionDetector>`)
- Use `?` operator for error propagation, never `.unwrap()` in library code (only in tests)
- Derive macros where appropriate: `Debug`, `Clone`, `Serialize`, `Deserialize`, `thiserror::Error`
- Follow Rust naming conventions: `snake_case` functions, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants
- Use `#[must_use]` on functions returning important values
- Prefer `&str` over `String` in function parameters, `PathBuf`/`&Path` for file paths
- Write doc comments (`///`) on all public items
- Use `#[cfg(test)]` modules at the bottom of each file
- Respect the existing code patterns in `seshat-core` and `seshat-scanner`
- Run `cargo clippy` mentally — no needless clones, no redundant closures, no unused imports
- Use `rayon` for CPU-bound parallelism (file-level), `tokio` for async I/O — never mix (ADR concurrency pattern)
- Graceful degradation: one bad file or failed detector must not crash the pipeline

## Technical Considerations

- **ADR-6**: Parallel scanning by file, sequential detectors per file. `files.par_iter().map(|f| run_all_detectors(f))`.
- **ADR-7**: Confidence = `adoption_count / total_count`. Thresholds configurable in `DetectionConfig`.
- **ADR-8**: `ProjectFile` has common fields + `LanguageIR` enum. Detectors use both.
- **ADR-17**: `ConventionFinding`, `CodeEvidence`, `DetectorResults` defined in `seshat-core/src/detector_result.rs`.
- **Existing fixtures**: 12 sample files already exist in `crates/seshat-detectors/tests/fixtures/` — use them for unit tests.
- **Dependency on `seshat-core` only**: Detectors must not depend on `seshat-storage` or any other crate. Cross-reference (Story 3.10) may live in `seshat-graph` if it needs DB access.
- **File structure detector**: The `detect()` method receives one file at a time, but file structure analysis requires seeing the whole set. Options: (a) accumulate state in the detector with interior mutability, (b) implement as a separate aggregation step outside the per-file pipeline, (c) pre-compute directory structure and pass as context. Choose the cleanest approach and document it.
- **`rayon` dependency**: Add `rayon` to `seshat-detectors/Cargo.toml` for `par_iter()` in the pipeline.

## Success Metrics

- All 8 detectors produce correct findings for fixture projects across 4 languages
- Confidence scoring produces correct weight mappings at threshold boundaries
- Cross-reference creates Contradicts edges for known conflicts in test data
- Detection pipeline handles empty file lists, files with no detectable patterns, and failing detectors gracefully
- `cargo test --workspace` passes
- `cargo clippy --all-targets --all-features -- -D warnings` passes
- `cargo fmt --all --check` passes
- No code duplication between detectors — shared logic extracted
- Full diff review against `main` passes the quality gate defined in US-011

## Open Questions

- **File structure detector approach**: The `ConventionDetector` trait's `detect` method is per-file, but file structure analysis requires a project-wide view. The implementing agent should choose between interior mutability, separate aggregation pass, or context injection — and document the decision.
- **Cross-reference crate placement**: ADR-23 places cross-reference in `seshat-graph/src/cross_reference.rs`. If it needs storage access (loading doc nodes from DB), it should indeed live in `seshat-graph`. If it can operate purely on in-memory convention and doc node lists, it could live in `seshat-detectors`. The implementing agent should decide based on data access needs.
