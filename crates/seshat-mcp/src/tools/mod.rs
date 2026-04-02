//! MCP tool handlers.
//!
//! Each tool module is a thin layer: parse input → call `seshat-graph` → wrap
//! in envelope. No business logic lives here.

pub mod project_context;
