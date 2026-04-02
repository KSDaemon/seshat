//! MCP tool handlers.
//!
//! Each tool module is a thin layer: parse input → call `seshat-graph` → wrap
//! in envelope. No business logic lives here.

use rmcp::schemars;

pub mod project_context;
pub mod query_convention;
pub mod record_decision;
pub mod remove_decision;
pub mod update_decision;

/// Evidence example from the codebase, used in record_decision and update_decision.
#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct ExampleInput {
    /// File path.
    pub file: String,
    /// Start line number.
    pub line: Option<u32>,
    /// End line number.
    pub end_line: Option<u32>,
    /// Code snippet.
    pub snippet: Option<String>,
}

impl ExampleInput {
    /// Convert to the graph-layer `ExampleInput` with defaults for optional fields.
    pub fn to_graph_example(&self) -> seshat_graph::decisions::ExampleInput {
        seshat_graph::decisions::ExampleInput {
            file: self.file.clone(),
            line: self.line.unwrap_or(0),
            end_line: self.end_line.unwrap_or(self.line.unwrap_or(0)),
            snippet: self.snippet.clone().unwrap_or_default(),
        }
    }
}
