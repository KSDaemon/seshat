# PRD: Epic 3.5 — Competitive Analysis Retrofit (Stories 3.5.1–3.5.8)

## Introduction

**Type:** Chore

Retrofit existing implemented code (Epics 1–3) with improvements discovered from competitive analysis of 8 analogous projects and a hardcode review of all 8 detectors (`docs/research/epic3-hardcode-analysis-2026-03-30.md`). Unifies the duplicated dependency taxonomy, replaces hardcoded package-to-domain mappings with package registry metadata, adds git-based convention trend detection, introduces structural wrapper/facade convention detection, adds heuristic fallbacks to all detectors for unknown libraries, and extends the IR with function parameter names for naming convention analysis.

**Context:** Seshat is a Rust CLI + MCP server. Epics 1–3 are fully implemented: 9-crate workspace, Tree-sitter parsing (4 languages), manifest analysis, documentation ingestion, knowledge graph persistence, 8 convention detectors with confidence scoring, and cross-reference logic. However, the competitive analysis (2026-03-30, see `docs/research/competitive-analysis-2026-03-30.md`) and detector hardcode review identified critical gaps: (1) two parallel, inconsistent dependency taxonomy enums, (2) ~400 hardcoded package names that won't scale, (3) no convention trend detection, (4) no wrapper/facade pattern detection, (5) detectors miss unknown libraries entirely — no heuristic fallbacks, (6) `Function` struct lacks parameter names — naming detector can't analyze them, (7) `JavaScriptIR::module_system` field never read — ESM/CJS detection dead code. See ADRs 24–29 in `architecture.md`.

## Goals

- Eliminate code duplication: merge `DependencyDomain` (8 variants, seshat-detectors) and `DependencyCategory` (12 variants, seshat-scanner) into one unified enum in seshat-core (Story 3.5.1)
- Replace ~400 hardcoded package-name-to-domain mappings with live package registry metadata (crates.io, npm, PyPI) cached in SQLite with offline fallback (Story 3.5.2)
- Collect last git commit date per file during scan for trend analysis (Story 3.5.3)
- Compute convention trends (Rising/Stable/Declining/Unknown) using P90 percentile of file modification dates (Story 3.5.4)
- Detect wrapper/facade conventions structurally via import graph analysis — no hardcoded directory names (Story 3.5.5)
- Add function parameter extraction to all 4 Tree-sitter parsers and parameter naming analysis to the naming detector (Story 3.5.6)
- Add heuristic fallbacks to error handling, logging, testing, and dependency detectors for unknown libraries (Story 3.5.7)
- Fix dead code: use `JavaScriptIR::module_system` field in export detector, flag mixed ESM/CJS (Story 3.5.7)

## User Stories

### US-001: Unify Dependency Domain Taxonomy (Story 3.5.1)

**Description:** As a developer, I want a single dependency domain taxonomy across scanner and detectors so that domain classification has one source of truth with no duplication or contradictions.

**Acceptance Criteria:**
- [ ] Single `DependencyDomain` enum defined in `crates/seshat-core/src/dependency.rs` with 12 variants: `Http`, `WebFramework`, `Logging`, `Testing`, `Validation`, `Serialization`, `Database`, `Cli`, `AsyncRuntime`, `Crypto`, `Utilities`, `Unknown`
- [ ] Derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize` with `#[serde(rename_all = "snake_case")]`
- [ ] Old `DependencyDomain` enum deleted from `crates/seshat-detectors/src/dependency_usage.rs` (was lines 27–36, 8 variants)
- [ ] Old `DependencyCategory` enum deleted from `crates/seshat-scanner/src/manifest.rs` (was lines 46–59, 12 variants)
- [ ] `DeclaredDependency.category` field in manifest.rs uses unified type
- [ ] All `classify_*()` functions in dependency_usage.rs and `categorize_*_dep()` functions in manifest.rs return unified `DependencyDomain`
- [ ] Mapping conflicts resolved: web frameworks (`actix-web`, `express`, `flask`, `django`, `axum`, `rocket`) → `WebFramework`; HTTP clients (`reqwest`, `axios`, `httpx`) → `Http`
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-002: Package Registry Metadata Integration (Story 3.5.2)

**Description:** As a developer, I want dependency domain classification to use package registry metadata instead of hardcoded name lists so that new packages are correctly categorized without code changes.

**Acceptance Criteria:**
- [ ] New migration `V3__package_metadata.sql` creates `package_metadata` table: `(name TEXT, registry TEXT, categories TEXT, keywords TEXT, description TEXT, fetched_at INTEGER, PRIMARY KEY (name, registry))`
- [ ] `PackageRegistryClient` trait defined in `crates/seshat-scanner/src/registry.rs` with `fn fetch_metadata(&self, package_name: &str) -> Result<PackageMetadata, RegistryError>`
- [ ] `CratesIoClient` implementation: GET `https://crates.io/api/v1/crates/{name}` → extract `categories[].slug` and `keywords[]`
- [ ] `NpmClient` implementation: GET `https://registry.npmjs.org/{name}` → extract `keywords[]`
- [ ] `PyPIClient` implementation: GET `https://pypi.org/pypi/{name}/json` → extract `classifiers[]` and `keywords`
- [ ] Category/classifier → `DependencyDomain` mapping rules (~30 total) in `registry_mapping.rs`
- [ ] Three-tier lookup: (1) SQLite cache → (2) registry API fetch + cache → (3) hardcoded fallback with lower confidence
- [ ] Cache TTL: 30 days — stale entries re-fetched on next scan
- [ ] HTTP client: `ureq` (blocking) added to seshat-scanner dependencies
- [ ] User-Agent header set per API policies: `seshat/{version}`
- [ ] Timeout: 5 seconds per request; network errors → graceful fallback to hardcoded mapping
- [ ] Existing `classify_*()` / `categorize_*_dep()` functions preserved as fallback tier
- [ ] Unit tests with mock HTTP responses (no real network calls in tests)
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-003: Git File Dates Collection (Story 3.5.3)

**Description:** As a developer, I want Seshat to collect last git commit date for each file during scan so that convention trend detection can determine Rising/Stable/Declining.

**Acceptance Criteria:**
- [ ] `gix` crate added to `seshat-scanner` with `max-performance-safe` feature
- [ ] New function `collect_git_file_dates(repo_root: &Path) -> Result<HashMap<PathBuf, i64>, ScanError>` in `crates/seshat-scanner/src/git_dates.rs`
- [ ] Single commit walk from HEAD — O(commits), NOT per-file git log
- [ ] Each file mapped to its most recent commit's Unix timestamp
- [ ] New migration `V2__add_file_dates.sql`: `ALTER TABLE files_ir ADD COLUMN last_commit_date INTEGER`
- [ ] `FileIRRepository::upsert()` signature updated to accept `last_commit_date: Option<i64>` as separate parameter (ProjectFile struct NOT modified — IR stays pure code structure)
- [ ] New trait method: `fn get_file_dates_by_branch(&self, branch_id: &BranchId) -> Result<HashMap<String, Option<i64>>, StorageError>`
- [ ] Non-git directories: return empty HashMap, no error — all dates stored as NULL
- [ ] Scan orchestrator calls `collect_git_file_dates()` after `discover_files()` and passes dates to upsert
- [ ] Unit test: temp git repo with commits → verify correct dates collected
- [ ] Unit test: non-git directory → empty HashMap
- [ ] Unit test: empty repo → empty HashMap
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-004: Convention Trend Computation (Story 3.5.4)

**Description:** As a developer, I want each detected convention to have a trend indicator so that AI agents know whether to adopt or avoid a pattern.

**Acceptance Criteria:**
- [ ] `Trend` enum in `seshat-core/src/knowledge.rs`: `Rising`, `Stable`, `Declining`, `Unknown` with `#[serde(rename_all = "snake_case")]`
- [ ] `DetectionConfig` extended with `trend_rising_days: u32` (default 90) and `trend_stable_days: u32` (default 365)
- [ ] New function `compute_trend(file_dates: &[Option<i64>], config: &DetectionConfig) -> Trend` in `seshat-detectors/src/confidence.rs`
- [ ] P90 percentile calculation: sort valid dates ascending, take index `ceil(N * 0.9) - 1`
- [ ] Mapping: P90 < `trend_rising_days` days ago → Rising, < `trend_stable_days` → Stable, else → Declining, no valid dates → Unknown
- [ ] `AggregatedConvention` struct extended with `trend: Trend` field
- [ ] `aggregate_findings()` accepts new parameter `file_dates: &HashMap<String, Option<i64>>` — for each convention group, collects dates of files where `follows_convention == true` and computes trend
- [ ] Trend stored in `KnowledgeNode.ext_data` as `{"trend": "rising"|"stable"|"declining"|"unknown"}` — merged with existing ext_data, not overwritten
- [ ] Unit tests at threshold boundaries: 89 days (Rising), 90 days (Stable), 364 days (Stable), 365 days (Declining), 366 days (Declining)
- [ ] Unit test: empty dates → Unknown; all None → Unknown; single date
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-005: Wrapper/Facade Convention Detection (Story 3.5.5)

**Description:** As a developer, I want the dependency usage detector to detect wrapper/facade patterns structurally so that direct usage of wrapped external dependencies is flagged as a convention violation.

**Acceptance Criteria:**
- [ ] `ConventionDetector` trait extended with default method: `fn detect_cross_file(&self, files: &[ProjectFile]) -> Vec<ConventionFinding>` (default: empty Vec)
- [ ] `run_all_detectors()` in pipeline updated: after per-file detection, runs `detect_cross_file()` for each detector and merges findings
- [ ] `DependencyUsageDetector` overrides `detect_cross_file()` with wrapper detection algorithm:
  - For each external dependency D: find files importing D directly (direct importers)
  - Identify internal modules that import D AND are imported by other project files (wrapper candidates)
  - If `consumer_count > direct_importer_count` (majority uses wrapper): establish wrapper convention
  - Direct importers when wrapper exists: `follows_convention = false`, description: "Use `{wrapper_module}` for `{external_dep}` operations"
  - Wrapper consumers: `follows_convention = true`
- [ ] No hardcoded directory names — purely import graph structural analysis
- [ ] Import resolution heuristics per language:
  - Rust: `crate::`/`super::`/`self::` = internal; bare crate name = external
  - TS/JS: starts with `.`/`./`/`../` = internal; bare specifier = external
  - Python: module path matches a project file = internal; otherwise stdlib/external
- [ ] Wrapper file itself NOT flagged as violating its own convention
- [ ] Works for all 4 supported languages
- [ ] Unit test: Python wrapper pattern (wrapper used by 5 files, 2 direct users → 2 violations)
- [ ] Unit test: TypeScript wrapper pattern (wrapper used by 4 files, 1 direct user → 1 violation)
- [ ] Unit test: no wrapper exists → no convention detected
- [ ] Unit test: wrapper used by minority (<50%) → no convention established
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-006: Function Parameter Extraction & Naming Analysis (Story 3.5.6)

**Description:** As a developer, I want the naming detector to analyze function parameter naming conventions so that AI agents follow consistent parameter naming across the project.

**Acceptance Criteria:**
- [ ] `Function` struct in `seshat-core/src/ir.rs` extended with `parameters: Vec<String>` field
- [ ] Rust tree-sitter parser extracts function parameter names from `function_item` and `method_definition` nodes
- [ ] TypeScript tree-sitter parser extracts parameter names from `function_declaration`, `arrow_function`, `method_definition` nodes
- [ ] JavaScript tree-sitter parser extracts parameter names (same node types as TS)
- [ ] Python tree-sitter parser extracts parameter names from `function_definition` nodes (excluding `self`/`cls`)
- [ ] Naming detector in `naming.rs` analyzes parameter name case patterns (snake_case, camelCase) per language
- [ ] Language-aware weighting: Rust parameter naming weighted lower (compiler/clippy convention), JS/TS/Python weighted higher
- [ ] Existing tests updated — `make_project_file()` test helper produces `parameters: vec![]` by default
- [ ] New tests: parse fixture files, verify parameter names extracted correctly for each language
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-007: Heuristic Fallbacks for Unknown Libraries (Story 3.5.7)

**Description:** As a developer, I want detectors to identify unknown libraries via heuristics so that new or uncommon packages are still classified rather than silently ignored.

**Acceptance Criteria:**

_Error handling detector:_
- [ ] Rust: heuristic — if crate is imported AND code contains `derive(Error)` or `impl std::error::Error` → error handling library (in addition to known thiserror/anyhow)
- [ ] Add known libraries: `eyre`, `color-eyre`, `miette`, `snafu`, `error-stack`, `displaydoc`
- [ ] Python: heuristic — class inheriting from name containing `Error`/`Exception` → custom exception (regardless of parent being in builtin list)

_Logging detector:_
- [ ] Name-based heuristic: dependency name contains `log`/`logger`/`logging`/`trace`/`tracing`/`observ` → likely logging library at lower confidence
- [ ] API shape heuristic: imported module used with `.info()`/`.debug()`/`.warn()`/`.error()`/`.fatal()`/`.trace()` calls → structured logging indicator

_Test patterns detector:_
- [ ] Config file detection: `jest.config.*` → Jest, `vitest.config.*` → Vitest, `[tool.pytest]` in pyproject.toml → pytest
- [ ] Unknown framework fallback: file in test directory AND has test-prefixed functions but framework unidentified → "uses testing (framework unknown)"
- [ ] Dependency name heuristic: name contains `test`/`mock`/`assert`/`spec` → testing-related

_Dependency usage detector:_
- [ ] Name-based heuristic for unrecognized packages: `test`/`mock` → Testing, `log`/`trace` → Logging, `http`/`web`/`api`/`rest` → Http, `sql`/`db`/`orm` → Database, `cli`/`command`/`arg` → Cli, `serial`/`json`/`yaml`/`proto` → Serialization, `valid`/`schema` → Validation
- [ ] Known-library matches remain high confidence; heuristic matches flagged as low confidence

_Export patterns detector:_
- [ ] Read `JavaScriptIR::module_system` field (currently unused dead code) to emit "project uses ESM/CommonJS/mixed" finding
- [ ] Flag mixed ESM + CJS in same file as Observation finding

_All detectors:_
- [ ] Heuristic findings have `KnowledgeWeight::Weak` or `KnowledgeWeight::Info` — never `Strong` or `Rule`
- [ ] Known-library findings always take priority over heuristic findings for the same package
- [ ] Unit tests for each heuristic: known library → high confidence, unknown-but-matched → low confidence, no match → no finding
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

### US-008: Branch Code Review & Quality Gate (Final Step)

**Description:** As a developer, I want a comprehensive code review of all changes in the feature branch before merging so that the code meets professional Rust quality standards.

**Acceptance Criteria:**

**Automated checks — all must pass:**
- [ ] `cargo fmt --all --check` — no formatting violations
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — no lint warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] `cargo doc --no-deps --document-private-items` — documentation builds without warnings
- [ ] No new `unsafe` blocks added without justification
- [ ] No `.unwrap()` or `.expect()` in library code (only in tests)

**Manual diff review — full `git diff main...HEAD` analysis:**

_Idiomatic Rust:_
- [ ] Error handling uses `thiserror` for library errors, `?` for propagation — no manual match-and-rewrap
- [ ] Types leverage Rust's type system: enums for variants, `Option` for nullable, `Result` for fallible
- [ ] Iterator chains preferred over imperative loops where they improve clarity
- [ ] `#[must_use]` on functions returning important values
- [ ] Doc comments (`///`) on all public items; module-level `//!` comments on each new module

_Performance:_
- [ ] No unnecessary allocations: `&str` over `String` in parameters, `&[T]` over `Vec<T>` where ownership not needed
- [ ] No needless `.clone()` — each clone justified
- [ ] Collections pre-allocated with `with_capacity()` where size known (especially `HashMap` for git dates, package metadata)
- [ ] Registry HTTP calls: timeout enforced (5s), errors don't crash scan pipeline

_Memory management:_
- [ ] `gix` repo handle properly scoped — dropped after commit walk completes
- [ ] `ureq` responses consumed and dropped — no leaked connections
- [ ] Package metadata cache bounded by number of actual dependencies (not unbounded)
- [ ] P90 computation does not hold full file list in memory longer than needed

_Code duplication:_
- [ ] Single `DependencyDomain` enum — no remnants of old `DependencyCategory` or duplicate `DependencyDomain`
- [ ] Registry client implementations share common HTTP/parsing logic via helper functions
- [ ] Category mapping rules consolidated in `registry_mapping.rs` — not scattered
- [ ] `classify_*()` (detector) and `categorize_*_dep()` (scanner) refactored to share classification logic through `PackageClassifier`

_Architecture compliance:_
- [ ] New code respects crate boundaries: `seshat-core` (types), `seshat-scanner` (data pipeline + registry), `seshat-storage` (persistence), `seshat-detectors` (detection logic)
- [ ] No SQL in scanner or detectors; no HTTP in detectors or graph
- [ ] `#[tracing::instrument]` on new public functions
- [ ] Serde annotations: `#[serde(rename_all = "snake_case")]` on new structs, `#[serde(skip_serializing_if = "Option::is_none")]` on optional fields
- [ ] Migration numbering consistent: V2 (file dates), V3 (package metadata)

_Test quality:_
- [ ] Tests are deterministic — no real HTTP calls, no reliance on system clock for trend tests (use fixed timestamps)
- [ ] Test names descriptive: `test_p90_trend_at_90_day_boundary_returns_stable`
- [ ] Edge cases covered: empty repos, no git, no internet, empty dependency list, single file, all files same date
- [ ] Mock implementations for `PackageRegistryClient` in tests
- [ ] No test code in production modules

## Functional Requirements

- FR-1: Single unified `DependencyDomain` enum in `seshat-core` replaces both parallel enums with all 12 domain variants
- FR-2: All dependency classification functions across scanner and detectors return the unified enum type
- FR-3: Package registry metadata fetched from crates.io, npm, PyPI and cached in SQLite with 30-day TTL
- FR-4: Three-tier classification fallback: cache → registry API → hardcoded mapping (with lower confidence)
- FR-5: ~30 category-to-domain mapping rules replace ~400 hardcoded package names
- FR-6: `gix` commit walk collects `last_commit_date` per file in a single O(commits) pass
- FR-7: `last_commit_date` stored in `files_ir` table as nullable INTEGER, passed separately from `ProjectFile` IR
- FR-8: Convention trends computed via P90 percentile with configurable thresholds (90/365 days default)
- FR-9: Trend stored in `KnowledgeNode.ext_data` and returned in all MCP convention responses
- FR-10: Wrapper/facade conventions detected via import graph structural analysis — no hardcoded directory names
- FR-11: `ConventionDetector` trait extended with `detect_cross_file()` method for cross-file detection patterns
- FR-12: A failing registry fetch or git date collection does not crash the scan — graceful degradation with warnings
- FR-13: Existing hardcoded classification preserved as offline fallback tier
- FR-14: `Function` struct extended with `parameters: Vec<String>` field, populated by all 4 Tree-sitter parsers
- FR-15: Naming detector analyzes parameter name case patterns per language with language-aware weighting
- FR-16: Error handling detector uses derive/impl heuristics for unknown Rust error crates + inheritance heuristic for Python exceptions
- FR-17: Logging detector uses name-based and API-shape heuristics for unknown logging libraries
- FR-18: Test patterns detector uses config file detection and name-based heuristics for unknown test frameworks
- FR-19: Dependency usage detector uses name-based heuristics for unrecognized packages at lower confidence
- FR-20: Export patterns detector reads `JavaScriptIR::module_system` field and flags mixed ESM/CJS
- FR-21: Heuristic findings always marked lower confidence than known-library findings; known matches take priority

## Non-Goals

- No MCP server changes in this epic — MCP tools consume trends and wrapper findings in Epics 5+
- No CLI report changes — report formatting is Epic 4
- No embedding/vector search — deferred to M2+ per ADR-26
- No `record_decision` tool — that's Epic 5, Story 5.5
- No call graph analysis — wrapper detection uses import graph only, not function-level call resolution
- No custom/third-party registry support — only crates.io, npm, PyPI
- No full semantic import resolution — heuristic-based (relative path = internal, bare specifier = external)
- No local variable naming analysis — too much noise, not enough signal (only function params)
- No config-file externalization of known-library lists — deferred to future iteration
- No type-aware parameter analysis — extract names only, not types (minimize parse overhead)

## Developer Context

The implementing agent is retrofitting **existing, working code**. This means:

- Existing tests MUST continue to pass — this is a refactor, not a rewrite
- Changes are additive: new columns, new modules, new trait methods with default impls
- The unified `DependencyDomain` must be a drop-in replacement — same semantics, broader variant set
- Registry integration is opt-in during scan — if network fails, behavior is identical to pre-retrofit
- `ProjectFile` struct is NOT modified — git dates are passed separately to avoid polluting the IR
- All code follows patterns established in Epics 1–3: `thiserror` errors, `#[tracing::instrument]`, `#[cfg(test)] mod tests`, `serde` derives

## Technical Considerations

- **ADR-24**: Convention trend detection — P90 percentile, gix commit walk, configurable thresholds
- **ADR-25**: Package registry metadata — three-tier fallback, ureq HTTP, SQLite cache, unified taxonomy
- **ADR-26**: Embeddings deferred — FTS5 sufficient for M0-M1
- **ADR-28**: Wrapper detection — structural import graph analysis, no hardcoded names
- **Migration ordering**: V2 (file dates) before V3 (package metadata) — `refinery` runs migrations in order
- **gix feature flags**: Use `max-performance-safe` — enables parallel object access without unsafe code. Avoid `max-performance` (uses `unsafe` for mmap).
- **ureq vs reqwest**: `ureq` chosen for blocking HTTP — simpler, smaller binary, no async runtime needed during scan. `reqwest` would require `tokio` runtime.
- **Cross-file detection**: The `ConventionDetector` trait gets a new default method `detect_cross_file()` — backward compatible, only `DependencyUsageDetector` overrides it initially

## Success Metrics

- Zero `DependencyCategory` references remaining in codebase — unified to `DependencyDomain`
- Package registry metadata cached for all dependencies after first scan with network
- Convention trends correct at P90 threshold boundaries in unit tests
- Wrapper detection finds patterns in fixture projects matching the PR #589 real-world scenario
- `cargo test --workspace` passes
- `cargo clippy --all-targets --all-features -- -D warnings` passes
- `cargo fmt --all --check` passes
- Full diff review against `main` passes the quality gate defined in US-006

## Open Questions

- **gix API complexity**: The exact `gix` API for commit walking + tree diffing may require iteration. If too complex, the story allows falling back to shelling out to `git log` with documented tradeoff.
- **npm keyword quality**: npm `keywords` are free-form and may be inconsistent. The mapping rules should be conservative — only map high-confidence keywords, default to `Unknown` otherwise.
- **Python import resolution**: Distinguishing `from datetime import datetime` (stdlib) from `from myproject.utils import datetime_helper` (internal) without full module resolution is heuristic-based. Edge cases are acceptable — `record_decision` (Epic 5) covers what automation misses.
