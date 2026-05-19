# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/KSDaemon/seshat/compare/seshat-mcp-v0.2.1...seshat-mcp-v0.3.1) - 2026-05-17

### <!-- 0 -->Features

- Update tool description + envelope metadata
- Aggregate call_sites by file in query_code_pattern
- Return blast_radius per symbol match
- Return dependent_files per symbol match in query_code_pattern
- Route symbol matching through symbol_definitions index

### <!-- 1 -->Bug Fixes

- address second adversarial review findings
- *(graph)* bound validate_approach payload and harden focus_area semantics

## [0.3.0](https://github.com/KSDaemon/seshat/compare/seshat-mcp-v0.2.1...seshat-mcp-v0.3.0) - 2026-05-17

### <!-- 0 -->Features

- Update tool description + envelope metadata
- Aggregate call_sites by file in query_code_pattern
- Return blast_radius per symbol match
- Return dependent_files per symbol match in query_code_pattern
- Route symbol matching through symbol_definitions index

### <!-- 1 -->Bug Fixes

- address second adversarial review findings
- *(graph)* bound validate_approach payload and harden focus_area semantics

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-mcp-v0.1.1...seshat-mcp-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- Surface content-level granularity in map_diff_impact MCP wiring and logs
- Rewrite compute_affected_symbols to hunk-level granularity
- Add end_line to Export and TypeDef IR structs (schema v8)
- Surface transitive count and depth in call-logger summary
- Expose depth parameter on query_dependencies MCP tool
- Add depth parameter to query_dependencies graph-layer API
- Migrate MCP record/update/remove_decision to DecisionRepository
- make repo_path optional in map_diff_impact, fallback to server project_root
- Add edge case and correctness tests
- Add MCP handler and server registration for map_diff_impact
- inject detached_head:true into MCP tool response metadata
- Add syncing and snapshot_based metadata to MCP responses
- implement auto-scan feature
- *(ir)* add doc_comment to Function/TypeDef and file_doc to ProjectFile
- *(mcp)* wire embedding provider through MCP server for semantic search
- Tool registration, cross-tool references, and next_steps updates
- validate_approach MCP tool handler
- query_dependencies MCP tool handler
- query_code_pattern MCP tool handler
- Integration tests and verification
- Integrate CallLogger into McpServer
- CallLogger struct — file writer with session tracking
- CallLogEntry and CallLogResult types
- repo parameter activation
- file_path parameter in all 5 MCP tool schemas
- McpServer redesign: root + submodules HashMap
- scope.rs module + ErrorCode variants
- [Story 5.8] smart DB discovery for seshat serve + repo/scope in tool schemas
- Agent protocol documentation in MCP tool descriptions
- update_decision and remove_decision MCP tools
- record_decision MCP tool
- query_convention MCP tool
- query_project_context MCP tool
- Response envelope and error handling structs
- Implement seshat serve CLI command with DB discovery and startup/shutdown UX
- Implement McpServer with rmcp stdio transport and ping tool
- Add rmcp + tokio workspace dependencies and extend ServerConfig
- scaffold Rust workspace with 9 crates

### <!-- 1 -->Bug Fixes

- *(mcp)* collapse single-line ranges in format_changed_lines
- *(mcp)* drop "(N direct)" parenthetical when transitive == direct
- *(mcp)* use MAX_TRANSITIVE_DEPTH constant in depth-validation suggestion
- *(mcp)* backwards-compat shim for legacy id/node_id envelope (H3)
- *(graph,mcp)* state-gate update/remove_decision against TUI rows (H2)
- *(graph,storage)* recompute description_hash on update_decision (H1)
- accept '.' and './' as root scope synonyms
- fix lint warnings and additional edge case fixes
- *(mcp)* validate kind filter, normalize paths, exhaustive error mapping
- use ErrorCode::InvalidInput instead of EmptyTopic for non-topic params
- use map_graph_error instead of internal_error in MCP tool handlers
- correct session_id docs (hex not alphanumeric), zero-cost when disabled
- add #[tool_handler] to ServerHandler for MCP tool routing

### <!-- 3 -->Dependencies

- *(deps)* bump semver-compatible dependencies (lockfile)

### <!-- 4 -->Refactor

- *(graph)* rename AffectedSymbol.dependent_count to transitive_dependent_count
- *(mcp,graph)* proper DECISION_NOT_FOUND code, drop H3 legacy id/node_id shim (P35)
- *(mcp)* tidy decision-tool boundary validation and tests (P34, P36, P37, P38)
- move next_steps generation to MCP handler
- KSD code review fixes — dedup, idioms, bugs, spec compliance, edge cases
- *(mcp)* Phase 1 response cleanup — remove noise, tighten schema
- *(graph,detectors)* clean up dependency detection and project context output
- extract duplicated test helpers (insert_ir, insert_convention_node)
- extract duplicated logging boilerplate into execute_tool helper
- deduplicate decision_result helpers and remove dead error_code_string
- replace manual days_to_ymd timestamp with chrono::Utc::now()
- deduplicate code, fix bugs, improve Rust idiomatics across submodule support
- extract handler error helpers + shared test_conn (findings 6,7,9,10)
- deduplicate code across graph/mcp crates (code review findings)

### <!-- 5 -->Documentation

- *(mcp)* clarify scope parameter — mount path, not submodule name

### <!-- 6 -->Tests

- MCP idempotency, non-git→git transition, scan_records_head edges (T17, T18, T19)
- *(mcp/tools/diff_impact)* cover generate_next_steps advice branches
- *(mcp/call_logger)* add tests for result extractors and missing-key fallbacks
- add coverage for envelope, fts, instructions, and uninstall edge cases
