# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-bin-v0.1.1...seshat-bin-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- *(cli)* add `seshat completions` subcommand with shell auto-detect
- Add integration tests for end-to-end serve guardrails
- Background update notice on CLI commands
- Comprehensive fix for the seshat review TUI review wizard: UI layout matching design spec, left-right example navigation, convention dedup via description hash, rich summary with total/pending/precision/coverage, non-blocking event loop to prevent hang on exit, consistent branch ID, and snapshot hash for reject concurrency.
- tui review wizard
- *(cli)* implement seshat init command
- seshat status command
- Implement seshat serve CLI command with DB discovery and startup/shutdown UX
- Branch code review and quality gate
- Basic seshat scan command with clap and two-phase progress
- Release pipeline and version string with git hash
- Configuration system with seshat.toml and zero-config defaults
- scaffold Rust workspace with 9 crates

### <!-- 1 -->Bug Fixes

- fix failing tests
- harden autoscan/watcher guardrails per code review
- use RAII Drop guard for scan integration test DB cleanup
- clean up temp project DBs in scan integration tests
- wrap env var mutations in unsafe blocks for Rust 1.83+ compatibility

### <!-- 6 -->Tests

- pin `git init -b main` in repo-bootstrap helpers
- *(cli)* tighten completions tests — no leaks, stderr-clean, cfg-guarded
- *(bin/scan_integration)* isolate HOME for every test by default
