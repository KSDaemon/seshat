//! # Seshat MCP
//!
//! MCP (Model Context Protocol) server with thin tool handlers. This crate
//! is intentionally minimal — it parses input, validates parameters, calls
//! into `seshat-graph` for intelligence, and formats the JSON response
//! envelope.
//!
//! Tools exposed:
//! - `query_project_context` — project overview
//! - `query_convention` — convention lookup
//! - `query_code_pattern` — code pattern search
//! - `validate_approach` — approach validation with graduated response
//! - `query_dependencies` — dependency and blast radius analysis
//!
//! Supports stdio, SSE, and HTTP transports via `rmcp`.

pub mod error;

pub use error::McpError;
