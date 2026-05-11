# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-watcher-v0.1.1...seshat-watcher-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- US-009 - Wire last_scanned_commit updates in scan paths
- US-004 - Watcher does not start when auto-scan failed
- US-004 - Replace watcher bulk-rescan with ADR-14 snapshot switch
- US-001 - Unify detect_branch into single implementation
- US-002 US-003 - wire detected branch into serve flow and add branch snapshot on switch
- *(watcher)* implement WatcherHandle, start_watcher() and integration tests
- *(watcher)* implement warm tier convention recalculation (Story 10.2)
- *(watcher)* implement bulk change detector and hot tier (Story 10.1, 10.3)
- *(watcher)* add notify-debouncer-full dep and extend WatcherConfig
- scaffold Rust workspace with 9 crates

### <!-- 1 -->Bug Fixes

- *(docs)* resolve broken and private intra-doc links
- *(scanner)* store files_ir paths relative to project_root (Bug #3)
- *(sync)* propagate source_map through incremental detection (Bug #2)
- worktree detached HEAD accepts abbreviated hashes, add iteration limit to find_git_dir
- fix lint warnings and additional edge case fixes
- *(review)* address BMAD code review findings (P-1 through P-6)
- *(serve,watcher)* eliminate startup latency by offloading watcher init to background
- *(hot_tier)* apply ScanConfig to per-file processing
- *(watcher)* address code review findings (P1–P9, P11, P12)

### <!-- 4 -->Refactor

- *(scanner)* move sentinel write into orchestrator (P19, P21)
- *(watcher)* correct sentinel update on bulk rescan (P17, P18)
- extract execute_bulk_rescan from on_bulk_rescan closure for testability
- *(watcher,scan)* eliminate detection pipeline duplication

### <!-- 6 -->Tests

- *(watcher)* ignore flaky hot_tier_detects_file_creation on CI
- improve code coverage across multiple modules (Phases 1-3)
