# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/KSDaemon/seshat/compare/seshat-mcp-v0.2.1...seshat-mcp-v0.3.0) - 2026-05-17

### <!-- 0 -->Features

- US-007 - Update tool description + envelope metadata
- US-006 - Aggregate call_sites by file in query_code_pattern
- US-005 - Return blast_radius per symbol match
- US-004 - Return dependent_files per symbol match in query_code_pattern
- US-009 - Route symbol matching through symbol_definitions index

### <!-- 1 -->Bug Fixes

- address second adversarial review findings
- *(graph)* bound validate_approach payload and harden focus_area semantics

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-mcp-v0.1.1...seshat-mcp-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- [US-010] Surface content-level granularity in map_diff_impact MCP wiring and logs
- [US-009] Rewrite compute_affected_symbols to hunk-level granularity
- [US-006] Add end_line to Export and TypeDef IR structs (schema v8)
- [US-004] Surface transitive count and depth in call-logger summary
- [US-003] Expose depth parameter on query_dependencies MCP tool
- [US-002] Add depth parameter to query_dependencies graph-layer API
- US-004 - Migrate MCP record/update/remove_decision to DecisionRepository
- make repo_path optional in map_diff_impact, fallback to server project_root
- US-007 - Add edge case and correctness tests
- US-005 - Add MCP handler and server registration for map_diff_impact
- inject detached_head:true into MCP tool response metadata
- US-006 - Add syncing and snapshot_based metadata to MCP responses
- implement auto-scan feature
- *(ir)* add doc_comment to Function/TypeDef and file_doc to ProjectFile
- *(mcp)* wire embedding provider through MCP server for semantic search
- [US-010] - Tool registration, cross-tool references, and next_steps updates
- US-006 - validate_approach MCP tool handler
- [US-004] - query_dependencies MCP tool handler
- [US-002] - query_code_pattern MCP tool handler
- US-005 - Integration tests and verification
- US-003 - Integrate CallLogger into McpServer
- US-002 - CallLogger struct — file writer with session tracking
- [US-001] - CallLogEntry and CallLogResult types
- [US-012] - repo parameter activation
- [US-009] - file_path parameter in all 5 MCP tool schemas
- [US-008] - McpServer redesign: root + submodules HashMap
- [US-007] - scope.rs module + ErrorCode variants
- [Story 5.8] smart DB discovery for seshat serve + repo/scope in tool schemas
- [US-012] - Agent protocol documentation in MCP tool descriptions
- US-011 - update_decision and remove_decision MCP tools
- US-010 - record_decision MCP tool
- [US-009] - query_convention MCP tool
- [US-008] - query_project_context MCP tool
- [US-004] - Response envelope and error handling structs
- [US-003] - Implement seshat serve CLI command with DB discovery and startup/shutdown UX
- [US-002] Implement McpServer with rmcp stdio transport and ping tool
- [US-001] - Add rmcp + tokio workspace dependencies and extend ServerConfig
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
