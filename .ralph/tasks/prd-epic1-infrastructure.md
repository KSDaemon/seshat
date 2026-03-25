# PRD: Epic 1 — Development Infrastructure & Project Bootstrap (Stories 1.3–1.7)

## Introduction

**Type:** Chore

Complete the Seshat development infrastructure by implementing SQLite schema with migrations, repository traits with CRUD operations, configuration system, CI/CD pipeline, and test fixtures. Stories 1.1 (workspace scaffolding) and 1.2 (core types) are already implemented. This PRD covers the remaining five stories that finish Epic 1 and unblock all subsequent feature work.

**Context:** Seshat is a Rust CLI tool + MCP server that scans codebases, builds knowledge graphs, and serves project intelligence to AI agents. Architecture is a 9-crate Rust workspace. See `_bmad-output/planning-artifacts/architecture.md` for full ADRs.

## Goals

- Establish SQLite persistence layer with auto-applied migrations (Story 1.3)
- Provide repository traits and SQLite CRUD implementations for all entity types (Story 1.4)
- Implement zero-config configuration system with optional `seshat.toml` override (Story 1.5)
- Set up CI/CD pipelines, pre-commit hooks, and conventional commit enforcement (Story 1.6)
- Create reference test fixture projects for integration testing (Story 1.7)

## User Stories

### US-001: SQLite Schema & Database Migrations (Story 1.3)

**Description:** As a Seshat developer, I want the initial SQLite schema and migration infrastructure so that knowledge graph data can be persisted reliably.

**Acceptance Criteria:**
- [ ] `seshat-storage` crate depends on `rusqlite` (with `bundled` and `modern_sqlite` features) and `refinery` (with `rusqlite` feature)
- [ ] `Database::open(path)` creates/opens SQLite database and auto-applies migrations via `embed_migrations!`
- [ ] `V1__initial_schema.sql` migration creates table `nodes` with columns: `id` INTEGER PRIMARY KEY, `branch_id` TEXT NOT NULL, `nature` TEXT NOT NULL, `weight` TEXT NOT NULL, `confidence` REAL NOT NULL, `adoption_count` INTEGER NOT NULL, `total_count` INTEGER NOT NULL, `description` TEXT NOT NULL, `ext_data` TEXT (JSON)
- [ ] `V1__initial_schema.sql` creates table `edges` with columns: `id` INTEGER PRIMARY KEY, `source_id` INTEGER REFERENCES nodes(id), `target_id` INTEGER REFERENCES nodes(id), `edge_type` TEXT NOT NULL, `branch_id` TEXT NOT NULL, `weight` REAL DEFAULT 1.0, `metadata` TEXT (JSON)
- [ ] `V1__initial_schema.sql` creates table `files_ir` with columns: `id` INTEGER PRIMARY KEY, `branch_id` TEXT NOT NULL, `file_path` TEXT NOT NULL, `language` TEXT NOT NULL, `content_hash` TEXT NOT NULL, `ir_data` BLOB NOT NULL, `updated_at` INTEGER NOT NULL, UNIQUE(`branch_id`, `file_path`)
- [ ] `V1__initial_schema.sql` creates table `metadata` with columns: `key` TEXT PRIMARY KEY, `value` TEXT NOT NULL
- [ ] Indexes created: `idx_nodes_branch_id`, `idx_nodes_nature`, `idx_edges_source_id`, `idx_edges_target_id`, `idx_files_ir_branch_path`
- [ ] `Database` struct uses `Arc<Mutex<Connection>>` for writes
- [ ] WAL mode enabled on connection open (`PRAGMA journal_mode=WAL`)
- [ ] All writes use explicit transactions
- [ ] Unit test: migration applies cleanly on fresh in-memory DB
- [ ] Unit test: re-opening existing DB is idempotent (migrations not re-applied)
- [ ] `cargo test -p seshat-storage` passes
- [ ] `cargo build` passes for entire workspace

### US-002: Repository Traits & Basic CRUD (Story 1.4)

**Description:** As a Seshat developer, I want repository traits and SQLite implementations for nodes, edges, files_ir, and branches so that other crates can persist and query data through a clean interface.

**Acceptance Criteria:**
- [ ] `NodeRepository` trait defined with methods: `insert(&KnowledgeNode) -> Result<NodeId>`, `get_by_id(NodeId, &BranchId) -> Result<Option<KnowledgeNode>>`, `find_by_nature(KnowledgeNature, &BranchId) -> Result<Vec<KnowledgeNode>>`, `find_by_branch(&BranchId) -> Result<Vec<KnowledgeNode>>`, `update(&KnowledgeNode) -> Result<()>`, `delete(NodeId, &BranchId) -> Result<()>`
- [ ] `EdgeRepository` trait defined with methods: `insert(&Edge) -> Result<EdgeId>`, `find_by_source(NodeId, &BranchId) -> Result<Vec<Edge>>`, `find_by_target(NodeId, &BranchId) -> Result<Vec<Edge>>`, `find_by_type(EdgeType, &BranchId) -> Result<Vec<Edge>>`, `delete(EdgeId) -> Result<()>`
- [ ] `FileIRRepository` trait defined with methods: `upsert(branch_id, file_path, language, content_hash, ir_data) -> Result<()>`, `get_by_path(file_path, &BranchId) -> Result<Option<FileIRRecord>>`, `get_by_branch(&BranchId) -> Result<Vec<FileIRRecord>>`, `delete_by_path(file_path, &BranchId) -> Result<()>`, `check_content_hash(file_path, &BranchId) -> Result<Option<String>>`
- [ ] `BranchRepository` trait defined with methods: `create_snapshot(from_branch, to_branch) -> Result<()>`, `switch_branch(branch_id) -> Result<()>`, `delete_branch(branch_id) -> Result<()>`, `list_branches() -> Result<Vec<BranchId>>`, `get_current_branch() -> Result<BranchId>`
- [ ] SQLite implementations for all four repository traits in `repository/` submodule
- [ ] `create_snapshot` copies all nodes + edges + files_ir rows with new `branch_id`
- [ ] Multi-row operations use transactions
- [ ] All CRUD operations covered by unit tests (insert, read back, update, delete, not-found)
- [ ] `cargo test -p seshat-storage` passes

### US-003: Configuration System (Story 1.5)

**Description:** As a Seshat developer, I want a configuration loading system that reads `seshat.toml` with sensible defaults so that Seshat works zero-config out of the box but is customizable.

**Acceptance Criteria:**
- [ ] `seshat-bin` crate depends on `toml` for config parsing
- [ ] `AppConfig` struct in `seshat-bin/src/config.rs` contains sections: `scan: ScanConfig`, `detection: DetectionConfig`, `server: ServerConfig`, `watcher: WatcherConfig`, `backup: BackupConfig`, `cache: CacheConfig`, `embedding: Option<EmbeddingConfig>`
- [ ] `AppConfig` implements `Default` (all sub-configs use their defaults)
- [ ] `AppConfig::load()` searches for `seshat.toml` in: current directory, then `$XDG_CONFIG_HOME/seshat/` (or `~/.config/seshat/`)
- [ ] If no config file found, defaults are used (zero-config works)
- [ ] If config file found, values from file override defaults (partial config is valid — missing keys use defaults)
- [ ] Environment variable `SESHAT_LOG` overrides `server.log_level`
- [ ] `seshat.toml.example` file created in repo root with all options commented out and default values documented
- [ ] Unit tests: default config, config from TOML string, partial config merge, env var override
- [ ] `cargo test -p seshat-bin` passes
- [ ] `cargo build` passes for entire workspace

### US-004: CI/CD Pipeline & Developer Tooling (Story 1.6)

**Description:** As a Seshat developer, I want CI/CD pipelines, pre-commit hooks, and conventional commit enforcement so that code quality is automated and releases are consistent.

**Acceptance Criteria:**
- [ ] `.github/workflows/ci.yml` runs on PRs: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo doc --no-deps`, conventional commit validation via `commitlint` or equivalent action
- [ ] `.github/workflows/lint-workflows.yml` runs `actionlint` only on changes to `.github/workflows/`
- [ ] `.github/workflows/release.yml` uses `release-plz` for: version bump, CHANGELOG.md generation, git tag, GitHub Release with cross-compiled binaries (at minimum: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`)
- [ ] `.pre-commit-config.yaml` configures hooks: `trailing-whitespace`, `end-of-file-fixer`, `check-yaml`, `check-toml`, `check-merge-conflict`, `conventional-pre-commit` (commit-msg stage), `cargo fmt` (local hook), `cargo clippy` (local hook)
- [ ] `build.rs` in `seshat-bin` captures git commit hash at compile time into `GIT_HASH` env var
- [ ] `seshat --version` prints `seshat {version} ({hash})` using `CARGO_PKG_VERSION` and `GIT_HASH`
- [ ] All workflow YAML files are valid (parseable)
- [ ] `cargo build` passes for entire workspace

### US-005: Test Fixtures & Reference Projects (Story 1.7)

**Description:** As a Seshat developer, I want reference test projects with known conventions so that integration tests can verify scanning and detection against expected results.

**Acceptance Criteria:**
- [ ] `tests/fixtures/rust_project/` contains a small Rust project (~10-15 files) with known patterns: `thiserror` error types, `tracing` logging, grouped imports (stdlib/external/internal), test files with `#[test]`, pub/private visibility, derive macros
- [ ] `tests/fixtures/rust_project/Cargo.toml` is a valid manifest with dependencies matching the code patterns
- [ ] `tests/fixtures/typescript_project/` contains a small TS project (~10-15 files) with known patterns: barrel exports (`index.ts`), ESM imports, Jest tests, custom error classes, type-only imports
- [ ] `tests/fixtures/typescript_project/package.json` is a valid manifest
- [ ] `tests/fixtures/python_project/` contains a small Python project (~10-15 files) with known patterns: stdlib `logging`, grouped imports, pytest tests, type hints, `__all__` exports, `__init__.py` files
- [ ] `tests/fixtures/python_project/pyproject.toml` is a valid manifest
- [ ] Each fixture project has `expected_conventions.json` documenting what detectors should find (detector name, expected nature, expected confidence range, description)
- [ ] Fixture projects are small (<50 files each) but representative of real patterns
- [ ] `crates/seshat-detectors/tests/fixtures/` contains individual sample files for unit-level detector testing: at least 3 Rust samples, 3 TypeScript samples, 3 Python samples
- [ ] All fixture files are syntactically valid in their respective languages

## Functional Requirements

- FR-1: `Database::open(path)` creates SQLite DB with WAL mode and auto-applies refinery migrations
- FR-2: `V1__initial_schema.sql` creates `nodes`, `edges`, `files_ir`, `metadata` tables with proper types, constraints, and indexes
- FR-3: Repository traits (`NodeRepository`, `EdgeRepository`, `FileIRRepository`, `BranchRepository`) define the data access contract
- FR-4: SQLite repository implementations perform all CRUD operations via parameterized queries within transactions
- FR-5: `BranchRepository::create_snapshot` atomically copies all data for a branch with a new `branch_id`
- FR-6: `AppConfig::load()` implements config file discovery with fallback to defaults
- FR-7: TOML deserialization uses `#[serde(default)]` so partial config files are valid
- FR-8: Environment variables (`SESHAT_LOG`) take highest precedence over file config
- FR-9: CI pipeline enforces fmt, clippy, test, doc, and conventional commits on every PR
- FR-10: Release pipeline automates version bump, changelog, tag, and cross-compiled binary publication
- FR-11: Pre-commit hooks catch formatting and lint issues before commit
- FR-12: `build.rs` embeds git hash for `--version` output
- FR-13: Test fixture projects contain realistic code patterns for all 4 supported languages
- FR-14: `expected_conventions.json` per fixture provides machine-readable expected detection results

## Non-Goals

- No FTS5 setup in V1 migration (deferred to Epic 5, Story 5.4)
- No vector search / embedding infrastructure (deferred to Epic 7, Story 7.6)
- No actual convention detectors implementation (Epic 3)
- No CLI command implementations (Epic 4)
- No MCP server implementation (Epic 5)
- No file watcher implementation (Epic 9)
- Pre-commit hooks: no enforcement that developers install them (documented in README only)

## Technical Considerations

- **rusqlite**: Use `bundled` feature to compile SQLite from source (no system dependency). Enable `modern_sqlite` for FTS5 readiness.
- **refinery**: Use `embed_migrations!` macro to bundle SQL files into the binary. Migration files live in `crates/seshat-storage/migrations/`.
- **Connection management (ADR-19)**: Single `Arc<Mutex<Connection>>` for writes. Read-only connections via WAL mode for concurrent reads. All DB access from async context must use `tokio::task::spawn_blocking`.
- **IR serialization (ADR-16)**: `files_ir.ir_data` is BLOB. Future stories will use bincode with version prefix. For now, the column exists but serialization format is not yet implemented.
- **Config file format**: TOML with `#[serde(default)]` on all config structs. This means any subset of config is valid.
- **Git hash in build.rs**: Use `std::process::Command` to run `git rev-parse --short HEAD`. Fallback to `"unknown"` if git is unavailable (e.g., in release tarballs).
- **Fixture projects**: Must be syntactically valid but don't need to compile/run. They are inputs for Tree-sitter parsing, not for execution.

## Success Metrics

- `cargo build` compiles the entire workspace with zero warnings
- `cargo test` passes all tests across all crates
- `Database::open()` + migration takes <100ms on fresh DB
- All repository CRUD operations covered by tests
- Config system works with no file, partial file, and full file
- CI workflows are valid YAML (parseable by actionlint)
- Fixture projects provide enough patterns for detector testing in Epic 3

## Open Questions

- None — all architectural decisions are documented in ADRs 1–23.
