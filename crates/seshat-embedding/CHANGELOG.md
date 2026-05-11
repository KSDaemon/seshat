# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/KSDaemon/seshat/compare/seshat-embedding-v0.1.1...seshat-embedding-v0.2.0) - 2026-05-11

### <!-- 0 -->Features

- *(epic8)* replace HTTP embedding providers with built-in fastembed-rs
- US-007 - seshat-embedding crate with Ollama and OpenAI providers

### <!-- 1 -->Bug Fixes

- *(epic8)* code review findings — body snippet, type labels, imports, dimension
- *(embedding)* harden validation, security and error handling
- add api_key field to EmbeddingConfig, eliminate unsafe env var manipulation in tests

### <!-- 3 -->Dependencies

- *(deps)* bump fastembed 4 → 5

### <!-- 6 -->Tests

- improve code coverage across multiple modules (Phases 1-3)
