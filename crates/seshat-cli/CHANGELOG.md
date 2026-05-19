# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/KSDaemon/seshat/compare/seshat-cli-v0.3.1...seshat-cli-v0.3.2) - 2026-05-19

### <!-- 0 -->Features

- Cross-branch regression integration test

## [0.3.1](https://github.com/KSDaemon/seshat/compare/seshat-cli-v0.2.1...seshat-cli-v0.3.1) - 2026-05-17

### <!-- 0 -->Features

- Adversarial code review pass + cleanup
- Maintain symbol index incrementally via watcher hot tier

### <!-- 1 -->Bug Fixes

- *(serve)* separate project_root from sync_root in incremental sync
- address second adversarial review findings

### <!-- 6 -->Tests

- *(cli)* use tempdir for XDG_CONFIG_HOME so Windows CI passes

## [0.3.0](https://github.com/KSDaemon/seshat/compare/seshat-cli-v0.2.1...seshat-cli-v0.3.0) - 2026-05-17

### <!-- 0 -->Features

- Adversarial code review pass + cleanup
- Maintain symbol index incrementally via watcher hot tier

### <!-- 1 -->Bug Fixes

- *(serve)* separate project_root from sync_root in incremental sync
- address second adversarial review findings

### <!-- 6 -->Tests

- *(cli)* use tempdir for XDG_CONFIG_HOME so Windows CI passes

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-cli-v0.1.1...seshat-cli-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- cfg(windows) integration-style tests for self-update flow
- cleanup stale .exe.old at startup (safe wrapper)
- Replace binary via self_replace and drop Windows early-return guard
- Zip archive extraction in extract_binary
- Windows target detection, asset matcher, checksum lookup, install method
- Add workspace deps `self_replace` and `zip`
- *(cli)* add `seshat completions` subcommand with shell auto-detect
- Freshness integration tests
- Cross-branch decisions integration test
- seshat decisions export and import CLI
- seshat decisions forget CLI
- seshat decisions list CLI
- Git-unavailable single-branch fallback verification
- Blocking incremental sync in run_review
- HEAD-change detection in run_serve
- Wire last_scanned_commit updates in scan paths
- Migrate MCP record/update/remove_decision to DecisionRepository
- count_confirmed_conventions reads from decisions table
- query_conventions_for_review LEFT JOIN against decisions
- TUI confirm/reject/partial migrate to DecisionRepository
- Add integration tests for end-to-end serve guardrails
- Watcher does not start when auto-scan failed
- Block AutoScan when cwd is dangerous and not in git repo
- Add is_dangerous_cwd() helper with per-OS denylist
- Extract Rust crate names from Cargo.toml in manifest.rs
- make repo_path optional in map_diff_impact, fallback to server project_root
- Background update notice on CLI commands
- Self-update — gatekeeper check and atomic replace
- Self-update — detection, download and verification
- seshat update --check command
- Version cache system
- Precision Diagnostic Message
- Search/filter with fuzzy matching in TUI
- inject detached_head:true into MCP tool response metadata
- Add 7 integration tests for branch switch orchestration
- Add syncing and snapshot_based metadata to MCP responses
- Implement diff-based background sync after branch switch
- Replace watcher bulk-rescan with ADR-14 snapshot switch
- Replace BranchId::from("main") hardcodes with instrumented fallbacks
- Fix ExistingDb branch detection to use project_root
- Unify detect_branch into single implementation
- Update TUI rendering to use snippet_start_line
- Comprehensive fix for the seshat review TUI review wizard: UI layout matching design spec, left-right example navigation, convention dedup via description hash, rich summary with total/pending/precision/coverage, non-blocking event loop to prevent hang on exit, consistent branch ID, and snapshot hash for reject concurrency.
- add PRD for Fix critical issues with the seshat review TUI review wizard: UI layout problems (overlapping text, cramped spacing, nested borders), terminal corruption on exit (control characters remain), application hangs (unresponsive state after confirming/rejecting conventions), and data persistence issues (code snippets disappearing after saving decisions).
- tui review wizard
- Add GC unit tests
- Add periodic GC background task
- Call GC on serve startup
- add worktree integration tests
- wire detected branch into serve flow and add branch snapshot on switch
- implement auto-scan feature
- add uninstall comand
- *(init)* wire agent instructions into run_init (Story 9.2 Task 5)
- *(init)* add instructions module + embedded agent content (Story 9.2 Task 1)
- *(call-sites)* extend call-site collection to TypeScript, JavaScript, and Python (IR v7)
- *(call-sites)* query_code_pattern returns real call-site snippets (IR v6)
- *(ir)* ModDeclaration/MacroCall in RustIR + call-site evidence for conventions
- *(detectors)* Phase 2 — real source snippets in convention evidence
- *(serve)* wire file watcher into seshat serve command (Story 10.1)
- *(watcher)* add notify-debouncer-full dep and extend WatcherConfig
- *(cli)* implement seshat init command
- *(ir)* add doc_comment to Function/TypeDef and file_doc to ProjectFile
- *(epic8)* richer embedding text — signature, body snippet, file imports
- *(epic8)* replace HTTP embedding providers with built-in fastembed-rs
- *(mcp)* wire embedding provider through MCP server for semantic search
- Code embeddings migration, storage, and scan integration
- seshat-embedding crate with Ollama and OpenAI providers
- CLI flag and config for call log path
- Integrate CallLogger into McpServer
- seshat status command
- seshat serve with submodule connections
- McpServer redesign: root + submodules HashMap
- Parallel submodule scanning
- Submodule change detection (commit_hash compare)
- Submodule scan flow in scan.rs (N+1 orchestrator calls)
- ScanProgress submodule variants + get_submodule_commit_hash()
- Submodule DB path resolution + ScanConfig field rename
- [Story 5.8] smart DB discovery for seshat serve + repo/scope in tool schemas
- query_project_context MCP tool
- Golden files computation and per-file convention compliance
- FTS5 migration and full-text search index management
- Persist convention detector results to nodes table after scan
- Implement seshat serve CLI command with DB discovery and startup/shutdown UX
- Branch code review and quality gate
- Scan report — Conventions Detected and Next Steps sections
- Scan report — Project Overview section
- Output formatting utilities, owo-colors, verbosity and NO_COLOR support
- Basic seshat scan command with clap and two-phase progress
- scaffold Rust workspace with 9 crates

### <!-- 1 -->Bug Fixes

- *(cli)* use sort_by_key with Reverse in report (clippy 1.95)
- *(docs)* resolve broken and private intra-doc links
- *(update)* match release asset by canonical name (P6)
- *(update)* match asset extensions case-insensitively (P5)
- *(update)* cap per-entry decompressed size in extract_zip (P4)
- *(update)* skip symlink entries in extract_zip (P3)
- *(update)* close canonicalize-bypass in extract_zip path-traversal guard (P2)
- *(update)* derive download filename from asset extension (P1, BLOCKER)
- *(cli)* pin literal bin_name and tolerate BrokenPipe in completions
- *(cli)* drop unsafe sh→Bash mapping in completion auto-detect
- *(cli)* harden $SHELL parsing against real-world environment quirks
- *(scanner)* store files_ir paths relative to project_root (Bug #3)
- *(sync)* propagate source_map through incremental detection (Bug #2)
- *(cli)* unify project resolver so worktrees share one DB (Bug #1)
- harden autoscan/watcher guardrails per code review
- *(cli/tui)* distinguish out-of-bounds example index from no-examples state
- *(cli/tui)* render composite (file-level summary) evidence in review
- *(detectors,cli)* snippet quality round 1 — TUI, FQN matching, heuristic word boundary, debug command
- use cwd as project_root for map_diff_impact in worktree setups
- address KSD code review findings — tar safety, HTTP checks, cache asset awareness
- *(debt)* always run stale embedding cleanup regardless of embedding success
- move precision diagnostic to separate line after metrics
- align summary numbers to single column with left-pad
- replace keybindings with filter bar + hint in search mode
- treat all chars as filter input in search mode, show ad in help
- treat y/n/p/s/q as filter chars in search mode, reset locked filter on /
- preserve current position in filtered view during incremental search
- make fuzzy_match UTF-8 safe — iterate by chars instead of bytes
- worktree detached HEAD accepts abbreviated hashes, add iteration limit to find_git_dir
- golden files dedup, dependency pollution, FTS5 search, and status branch display
- *(review)* make apply_review_actions resilient to individual failures
- *(review)* advance to next un-reviewed convention after action
- remove duplicated review summary report
- fix review issues
- fix lint warnings and additional edge case fixes
- improve seshat init and dry-run
- *(init)* use XDG path for OpenCode instructions on macOS
- *(init)* address code review findings from Story 9.2
- *(review)* address BMAD code review findings (P-1 through P-6)
- *(snippets)* populate real multi-line source snippets in convention evidence
- *(serve,watcher)* eliminate startup latency by offloading watcher init to background
- *(watcher)* address code review findings (P1–P9, P11, P12)
- *(scan)* force re-scan submodules when IR_SCHEMA_VERSION is outdated
- *(status)* read file_count/convention_count from repo_metadata
- *(status)* submodule display matches root project format
- *(cli/init)* use claude mcp add CLI for Claude Code, fix scope mapping
- *(cli/init)* use XDG ~/.config/opencode for OpenCode global config
- *(cli/init)* code review fixes + smart scope logic
- *(epic8)* code review findings — body snippet, type labels, imports, dimension
- *(cli)* remove pre-delete of embeddings to prevent data loss on partial failure
- *(storage/cli)* batch_size guard, bytes alignment check, stale cleanup, branch_id param
- clean up remaining XDG pollution from serve.rs and status.rs tests
- clean up XDG data dir in serve.rs and db.rs tests
- improve submodule scan UX — hide misleading message, show detailed progress
- add #[tool_handler] to ServerHandler for MCP tool routing
- cli about sentence

### <!-- 3 -->Dependencies

- *(deps)* bump rusqlite 0.37 → 0.38

### <!-- 4 -->Refactor

- *(cli)* unicode-width for `decisions list` table alignment (P30)
- *(cli)* per-action savepoints, project-scope label, atomic export (P28, P29, P32)
- *(cli)* bulk-fetch existing rows in import (P26)
- *(cli)* single freshness check + skip detection on no-op (P23, P24)
- *(scanner)* move sentinel write into orchestrator (P19, P21)
- *(cli)* tidy review banner, decisions forget/import (P22, P25, P27, P31, P33)
- *(detectors,graph,cli)* introduce ProjectContext, compute internal-name set once
- *(cli/debug)* typed deserialise + TryFrom narrowing + bad-row recovery
- *(cli/tui)* extract example_title() out of render
- remove dead exit_search_mode, guard lock_filter against empty results
- *(watcher,scan)* eliminate detection pipeline duplication
- *(config)* rename exclude_patterns to exclude_paths in ScanConfig
- improve embedding_repository idioms and scan.rs cleanup
- replace manual SystemTime epoch math with chrono in CLI crate
- replace manual Hinnant calendar in backup.rs with chrono
- change config.server.call_log from String to Option<String>
- deduplicate code, fix bugs, improve Rust idiomatics across submodule support
- deduplicate code across graph/mcp crates (code review findings)

### <!-- 6 -->Tests

- *(cli)* use platform-appropriate tempdir for XDG test
- *(cli)* make two cli tests Windows-friendly
- pin `git init -b main` in repo-bootstrap helpers
- *(cli)* make tests robust on fresh CI runners
- *(update)* strengthen extract_zip traversal coverage (P7)
- MCP idempotency, non-git→git transition, scan_records_head edges (T17, T18, T19)
- *(decisions)* forget across states + import edges + strict atomicity (T13, T14, T15)
- *(freshness)* never-scanned + backward HEAD + final-emit (T6, T7, T8)
- *(cross-branch)* reverse direction, Partial/Recorded, FK-decoupling (T2, T3, T4)
- *(decisions)* non-strict import conflict resolution end-to-end (T12)
- *(decisions)* forget error-path coverage required by US-014 (T11)
- *(tui)* end-to-end Partial review action integration test (T16)
- *(cross-branch)* non-FF (3-way) merge regression guard (T1)
- *(cli/instructions)* cover claude_home, opencode_config_dir, hook_command_exists edges
- *(cli/scan)* cover extract_body_snippet boundary and clamping cases
- *(cli/db)* cover unix_now, path resolvers, counts, HEAD parsing, find_git_dir
- *(cli/tui/review_wizard)* cover search-mode and filter-locked branches
- *(cli/uninstall)* cover hook-entry removal and skill-dir cleanup
- *(cli/init)* cover merge error messages, is_already_configured, and resolvers
- *(cli/tui/app)* cover App search/filter, navigation, and helpers
- *(cli/update)* cover parse_rate_limit and check_response_status
- *(cli/debug)* add unit tests for previously untested debug-snippets command
- *(cli/tui)* make example_title OOB test robust to release builds
- add fuzzy_match, levenshtein, and precision edge case tests
- add handle_auto_scan_snapshot tests for all 3 branch paths
- add handle_branch_switch unit tests for all 4 paths
- cover detect_cross_file default, detect_cursor_targets, run_claude_mcp_remove
- add coverage for envelope, fts, instructions, and uninstall edge cases
- add serve.rs branch snapshot, fallback_rescan, and print_startup tests
- improve code coverage across multiple modules (Phases 1-3)
- *(init)* integration tests for agent instruction writing (Story 9.2 Task 6)
