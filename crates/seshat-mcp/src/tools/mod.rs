//! MCP tool handlers.
//!
//! Each tool module is a thin layer: parse input → call `seshat-graph` → wrap
//! in envelope. No business logic lives here.

use rmcp::schemars;

pub mod diff_impact;
pub mod project_context;
pub mod query_code_pattern;
pub mod query_convention;
pub mod query_dependencies;
pub mod record_decision;
pub mod remove_decision;
pub mod update_decision;
pub mod validate_approach;

/// Evidence example from the codebase, used in record_decision and update_decision.
#[derive(Debug, serde::Serialize, serde::Deserialize, rmcp::schemars::JsonSchema)]
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

impl From<&ExampleInput> for seshat_graph::decisions::ExampleInput {
    fn from(ex: &ExampleInput) -> Self {
        Self {
            file: ex.file.clone(),
            line: ex.line.unwrap_or(0),
            end_line: ex.end_line.unwrap_or(ex.line.unwrap_or(0)),
            snippet: ex.snippet.clone().unwrap_or_default(),
        }
    }
}
