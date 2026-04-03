//! # Seshat MCP
//!
//! MCP (Model Context Protocol) server with thin tool handlers. This crate
//! is intentionally minimal — it parses input, validates parameters, calls
//! into `seshat-graph` for intelligence, and formats the JSON response
//! envelope.
//!
//! Tools exposed:
//! - `query_project_context` — project overview
//! - `query_convention` — convention lookup by topic via FTS5
//! - `record_decision` — record team conventions / decisions (planned)
//! - `update_decision` — modify recorded decisions (planned)
//! - `remove_decision` — soft-delete recorded decisions (planned)
//!
//! Supports stdio transport via `rmcp`. SSE and HTTP transports
//! will be enabled in future stories.

pub mod envelope;
pub mod error;
pub mod scope;
pub mod server;
pub mod tools;

pub use envelope::{ErrorCode, ErrorDetail, ErrorEnvelope, ResponseEnvelope, ResponseMetadata};
pub use error::McpError;
pub use scope::ProjectConnection;
pub use server::{McpServer, start_stdio, start_stdio_with_shutdown};
