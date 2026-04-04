# PRD: Epic 6.5 — MCP Call Logging for Dogfooding

## Introduction

**Type:** Feature

Add a dedicated JSONL call logger to the MCP server that records every tool call with full input parameters, response summary metrics, duration, and status. This enables analysis of tool usage patterns, call sequences, error rates, and API surface validation during dogfooding. The logger is a purpose-built telemetry component (ADR-30) — separate from the existing `tracing` debug infrastructure — activated via an opt-in `--call-log` CLI flag or `[server] call_log` config option.

**Context:** Seshat is a Rust MCP server used as its own dogfooding target during development. The current `tracing::info!` calls in `server.rs` go to stderr and are suppressed at the default `warn` level. There is no file-based logging, no `tracing-appender` dependency, and the `config.server.log_level` field is loaded but never wired to the tracing subscriber. A dedicated call log is needed to answer: which tools are used, how often, are call sequences correct, what's the error rate, are responses useful.

**References:**
- Architecture: ADR-30 in `_bmad-output/planning-artifacts/architecture.md`
- Functional Requirements: FR71, FR72 in `_bmad-output/planning-artifacts/prd.md`
- Story spec: `_bmad-output/implementation-artifacts/6.5-1-mcp-call-logging.md`

## Goals

- Enable opt-in JSONL logging of all MCP tool calls with full input and response summary metrics
- Provide session-level and per-call ordering (session ID + monotonic sequence counter)
- Produce machine-parsable output suitable for `jq`, `grep`, or simple analysis scripts
- Zero overhead when call logging is disabled (no file creation, no I/O)
- Graceful degradation: log write failures warn via tracing but never crash the server
- Append-only file semantics: multiple `seshat serve` sessions accumulate in the same log file

## User Stories

### US-001: CallLogEntry and CallLogResult Types

**Description:** As a developer, I need well-defined serializable types for call log entries so that every tool call produces a consistent, machine-parsable JSONL record.

**Acceptance Criteria:**
- [ ] New file `crates/seshat-mcp/src/call_logger.rs` created
- [ ] `CallLogEntry` struct with fields: `ts` (String, ISO 8601 UTC), `session` (String, 8-char alphanumeric), `seq` (u64, monotonic), `tool` (String), `input` (serde_json::Value), `duration_ms` (u64), `status` (String: "ok" or "error")
- [ ] `result` field: `Option<serde_json::Value>` — present on success with tool-specific summary scalars
- [ ] `error_code` field: `Option<String>` — present on error with `ErrorCode` string representation
- [ ] `#[serde(skip_serializing_if = "Option::is_none")]` on both optional fields for clean JSON
- [ ] Derives: `Debug, Serialize`
- [ ] Tool-specific result schemas: `query_project_context` → `{language_count, convention_count, golden_file_count}`, `query_convention` → `{convention_count, decision_count}`, `record_decision` → `{node_id}`, `update_decision` → `{node_id}`, `remove_decision` → `{node_id}`
- [ ] Helper functions or methods to construct result summaries from tool response data
- [ ] Unit test: `CallLogEntry` success case serializes to expected JSON schema (matching the example in story spec AC #4)
- [ ] Unit test: `CallLogEntry` error case serializes to expected JSON schema (matching story spec AC #5)
- [ ] Unit test: optional fields omitted when `None` (no `"result": null` in output)
- [ ] `cargo test -p seshat-mcp` passes
- [ ] `cargo clippy -p seshat-mcp -- -D warnings` passes

### US-002: CallLogger Struct — File Writer with Session Tracking

**Description:** As a developer, I need a `CallLogger` struct that manages an append-only file writer with session identification and sequence numbering so that log entries are correctly attributed and ordered.

**Acceptance Criteria:**
- [ ] `CallLogger` struct with fields: `writer: Mutex<BufWriter<File>>`, `session_id: String`, `seq: AtomicU64`
- [ ] `CallLogger::new(path: &Path) -> io::Result<Self>`: creates parent directories via `fs::create_dir_all`, opens file with `OpenOptions::new().create(true).append(true)`, generates 8-char alphanumeric session ID
- [ ] Session ID generated from system time hash or random bytes (no external `rand` dependency — use `std::collections::hash_map::DefaultHasher` on `SystemTime::now()` or similar)
- [ ] `fn log_call(&self, entry: &CallLogEntry) -> io::Result<()>`: acquires mutex, serializes entry to JSON via `serde_json::to_string`, writes line + newline, flushes buffer
- [ ] `fn next_seq(&self) -> u64`: `self.seq.fetch_add(1, Ordering::Relaxed)` — starts at 0
- [ ] Existing file is appended to, never truncated (verify with test: write, drop, recreate at same path, write again — both lines present)
- [ ] Unit test: `CallLogger::new` creates file at specified path
- [ ] Unit test: `CallLogger::new` creates parent directories that don't exist
- [ ] Unit test: `log_call` writes valid JSONL (one JSON object per line)
- [ ] Unit test: `next_seq` returns monotonically increasing values (0, 1, 2, ...)
- [ ] Unit test: session ID is 8 alphanumeric characters
- [ ] Unit test: append behavior — two `CallLogger` instances on same file → both sessions' entries present
- [ ] `cargo test -p seshat-mcp` passes
- [ ] `cargo clippy -p seshat-mcp -- -D warnings` passes

### US-003: Integrate CallLogger into McpServer

**Description:** As a developer, I need the `McpServer` to invoke the `CallLogger` after every tool call so that all MCP interactions are recorded when call logging is enabled.

**Acceptance Criteria:**
- [ ] `McpServer` struct gains field `call_logger: Option<CallLogger>` (in `crates/seshat-mcp/src/server.rs`)
- [ ] `McpServer::new()` signature updated to accept `call_log_path: Option<PathBuf>` — constructs `CallLogger` when `Some`, stores `None` otherwise
- [ ] After each of the 5 tool calls completes (both success and error paths), a `CallLogEntry` is constructed and logged
- [ ] Input captured: serialize the tool's request struct to `serde_json::Value` before calling the handler
- [ ] Duration captured: `Instant::now()` before handler, `elapsed().as_millis()` after
- [ ] On success: extract tool-specific result summary from handler response (see US-001 schemas)
- [ ] On error: capture `ErrorCode` variant as string in `error_code` field
- [ ] Log write failures: `tracing::warn!("call log write failed: {err}")` — error is NOT propagated, tool response is returned normally
- [ ] When `call_logger` is `None`: no overhead — no `Instant::now()`, no serialization, just the normal tool call
- [ ] `pub mod call_logger;` added to `crates/seshat-mcp/src/lib.rs`
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes

### US-004: CLI Flag and Config for Call Log Path

**Description:** As a developer running `seshat serve`, I want a `--call-log` flag to enable call logging to a file so that I can opt in to telemetry during development sessions.

**Acceptance Criteria:**
- [ ] New field in `Command::Serve` enum (in `crates/seshat-cli/src/args.rs`): `#[arg(long, value_name = "PATH")] call_log: Option<Option<PathBuf>>` — clap pattern for optional value
- [ ] When `--call-log` passed without value (`Some(None)`): resolve default path `dirs::data_dir().unwrap() / "seshat" / "call-log.jsonl"`
- [ ] When `--call-log /path/file.jsonl` passed (`Some(Some(path))`): use specified path
- [ ] When `--call-log` not passed (`None`): check `config.server.call_log`; if non-empty, use that path; otherwise `None` (no logging)
- [ ] CLI flag overrides config value
- [ ] New field in `ServerConfig` (in `crates/seshat-core/src/config.rs`): `pub call_log: String` with default `""` (empty = disabled)
- [ ] Resolved path passed to `McpServer::new()` as `Option<PathBuf>`
- [ ] `run_serve()` signature updated in `crates/seshat-cli/src/serve.rs` to accept `call_log: Option<Option<PathBuf>>`
- [ ] `lib.rs` dispatch updated to pass `call_log` field from `Command::Serve`
- [ ] Help text for `--call-log`: `"Log MCP tool calls to JSONL file for analysis. Default: $XDG_DATA_HOME/seshat/call-log.jsonl"`
- [ ] Startup banner (in `serve.rs`) prints `Call log: /path/to/file.jsonl` when active, omits line when disabled
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes

### US-005: Integration Tests and Verification

**Description:** As a developer, I need integration tests that verify end-to-end call logging behavior so that I can trust the telemetry pipeline.

**Acceptance Criteria:**
- [ ] Integration test: create `McpServer` with `call_log_path: Some(temp_file)`, invoke a tool (e.g. `query_convention`), verify JSONL line written to file with correct schema (all fields present, types correct)
- [ ] Integration test: create `McpServer` with `call_log_path: None`, invoke a tool, verify no log file created
- [ ] Integration test: multiple tool calls in sequence → verify all JSONL lines present, `seq` values are 0, 1, 2, ...
- [ ] Integration test: error case (e.g. empty topic on `query_convention`) → verify log entry has `status: "error"` and `error_code` field
- [ ] Integration test: log file directory doesn't exist → verify `CallLogger::new` creates it via `create_dir_all`
- [ ] `cargo test --workspace` passes with no regressions
- [ ] `cargo clippy --all-targets -- -D warnings` passes

## Functional Requirements

- FR-1: When `--call-log` flag or `[server] call_log` config is set, every MCP tool call produces exactly one JSONL line in the specified file
- FR-2: Each log entry contains: ISO 8601 timestamp (UTC), 8-char session ID, monotonic sequence number, tool name, full input parameters, duration in milliseconds, status (ok/error)
- FR-3: Success entries include tool-specific result summary scalars; error entries include the `ErrorCode` string
- FR-4: The log file is opened in append mode — never truncated. Multiple sessions accumulate.
- FR-5: Parent directories are created automatically if they don't exist
- FR-6: Log write failures produce a `tracing::warn!` message but do not affect tool responses or crash the server
- FR-7: When call logging is disabled (no flag, no config), there is zero overhead — no file descriptor, no timing, no serialization
- FR-8: `--call-log` without a value defaults to `$XDG_DATA_HOME/seshat/call-log.jsonl`
- FR-9: `--call-log /path` uses the specified path; CLI flag overrides `[server] call_log` config
- FR-10: Session ID is unique per `seshat serve` invocation (8-char alphanumeric, derived from system time hash)
- FR-11: Sequence counter starts at 0 and increments monotonically within a session

## Non-Goals

- No log file rotation (user manages file lifecycle for V1)
- No dashboard or built-in analysis UI (use `jq`, `grep`, or scripts)
- No full response body logging (only summary scalars — full responses can be KBs)
- No caller/sub-agent identification (MCP stdio provides no caller context; session ID + timestamps suffice)
- No modification to existing `tracing` infrastructure (that's separate tech debt)
- No encryption or compression of log files

## Technical Considerations

- **No new crate dependencies:** `serde_json` (serialization), `dirs` (XDG paths) are already workspace deps. Session ID uses `std::time::SystemTime` + `DefaultHasher` to avoid adding `rand`.
- **Clap pattern for optional value:** `Option<Option<PathBuf>>` — `None` = flag absent, `Some(None)` = flag present without value, `Some(Some(path))` = flag present with path. Requires `#[arg(long, value_name = "PATH")]` with `num_args(0..=1)` or `default_missing_value`.
- **Thread safety:** `Mutex<BufWriter<File>>` is sufficient. Tool calls take 30-120ms; lock contention on the writer is negligible. `AtomicU64` for the sequence counter avoids mutex for the hot path.
- **MCP server is `Clone`:** `McpServer` derives `Clone` for rmcp compatibility. `CallLogger` uses `Mutex` and `AtomicU64` which are not `Clone`. Wrap in `Arc<CallLogger>` or use `Option<Arc<CallLogger>>` on the server struct.
- **JSONL line size:** Input params are small (topic string, description, node ID). Result summaries are a few scalars. Each line should be well under 1KB, safely under PIPE_BUF (4KB) for atomic appends.
- **Existing server.rs structure:** The 5 tool methods at `server.rs:60-127` each delegate to handler modules. Call logging wraps around these delegations — capture input before, capture result after, log entry.

## Success Metrics

- All 5 MCP tools produce correct JSONL entries when call logging is enabled
- Log file is parsable by `jq` (every line is valid JSON)
- Existing test suite passes with zero regressions
- Clippy passes with zero warnings
- After enabling `--call-log` during a real development session, the log file contains a readable sequence of tool calls that can answer: "which tools were called, in what order, with what inputs, and did they succeed?"

## Open Questions

- None — design fully resolved during Party Mode discussion (2026-04-03). All decisions documented in ADR-30.
