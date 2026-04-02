//! Thin handler for the `record_decision` MCP tool.
//!
//! Parses MCP input, validates parameters, calls
//! `seshat_graph::record_decision`, and wraps the result in a
//! `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata};

/// Request parameters for `record_decision`.
#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct RecordDecisionRequest {
    /// Description of the convention, decision, or rule to record (required).
    #[schemars(description = "Description of the convention or decision to record")]
    pub description: String,

    /// Nature of the knowledge: 'decision', 'convention', or 'preference'.
    /// Defaults to 'decision'.
    #[schemars(description = "Nature: 'decision' (default), 'convention', or 'preference'")]
    pub nature: Option<String>,

    /// Weight/authoritativeness: 'rule' or 'strong'. Defaults to 'strong'.
    #[schemars(description = "Weight: 'rule' or 'strong' (default)")]
    pub weight: Option<String>,

    /// Optional category for grouping (e.g., "error-handling", "naming").
    #[schemars(description = "Category for grouping (e.g., 'error-handling', 'naming')")]
    pub category: Option<String>,

    /// Optional evidence examples from the codebase.
    #[schemars(description = "Evidence examples: [{file, line, end_line, snippet}]")]
    pub examples: Option<Vec<ExampleInput>>,

    /// Optional reasoning/rationale for the decision.
    #[schemars(description = "Reasoning or rationale for this decision")]
    pub reason: Option<String>,
}

/// An evidence example from the codebase.
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

/// Execute the `record_decision` tool.
///
/// Thin layer: validate input -> call `seshat_graph::record_decision` -> wrap
/// in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    req: RecordDecisionRequest,
) -> String {
    let start = Instant::now();
    let tool = "record_decision";

    // Validate: description must not be empty.
    let description = req.description.trim();
    if description.is_empty() {
        let err = ErrorEnvelope::new(
            tool,
            repo_name,
            ErrorCode::InvalidInput,
            "The description parameter must not be empty",
            "Provide a non-empty description for the decision or convention",
        );
        return serde_json::to_string(&err).unwrap_or_default();
    }

    // Map MCP request to graph params with defaults.
    let examples = req
        .examples
        .unwrap_or_default()
        .into_iter()
        .map(|ex| seshat_graph::decisions::ExampleInput {
            file: ex.file,
            line: ex.line.unwrap_or(0),
            end_line: ex.end_line.unwrap_or(ex.line.unwrap_or(0)),
            snippet: ex.snippet.unwrap_or_default(),
        })
        .collect();

    let params = seshat_graph::RecordDecisionParams {
        description: description.to_owned(),
        nature: req.nature.unwrap_or_else(|| "decision".to_owned()),
        weight: req.weight.unwrap_or_else(|| "strong".to_owned()),
        category: req.category,
        examples,
        reason: req.reason,
    };

    match seshat_graph::record_decision(conn, branch, params) {
        Ok(data) => {
            let metadata = ResponseMetadata::new(vec![
                "Use query_convention to verify this decision appears in results".to_owned(),
                "Use update_decision to modify or remove_decision to retract".to_owned(),
            ])
            .with_extra("node_id", serde_json::Value::from(data.id));

            let envelope =
                ResponseEnvelope::success(tool, repo_name, branch, data, metadata, start);

            serde_json::to_string(&envelope).unwrap_or_else(|e| {
                let err = ErrorEnvelope::new(
                    tool,
                    repo_name,
                    ErrorCode::InternalError,
                    format!("Failed to serialize response: {e}"),
                    "Please report this issue",
                );
                serde_json::to_string(&err).unwrap_or_default()
            })
        }
        Err(seshat_graph::GraphError::InvalidInput(msg)) => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::InvalidInput,
                msg,
                "Check the nature and weight parameter values",
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
        Err(e) => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::InternalError,
                format!("{e}"),
                "Check database and retry",
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_storage::Database;

    fn test_conn() -> Arc<Mutex<Connection>> {
        let db = Database::open(":memory:").expect("in-memory DB");
        db.connection().clone()
    }

    #[test]
    fn handle_records_decision_successfully() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            RecordDecisionRequest {
                description: "Always use Result for fallible operations".to_owned(),
                nature: None,
                weight: None,
                category: Some("error-handling".to_owned()),
                examples: None,
                reason: Some("Explicit error handling preferred".to_owned()),
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "record_decision");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert!(parsed["data"]["id"].as_i64().unwrap() > 0);
        assert_eq!(
            parsed["data"]["description"],
            "Always use Result for fallible operations"
        );
        assert_eq!(parsed["data"]["nature"], "decision");
        assert_eq!(parsed["data"]["weight"], "strong");
        assert!(parsed["metadata"]["node_id"].as_i64().unwrap() > 0);
    }

    #[test]
    fn handle_empty_description_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            RecordDecisionRequest {
                description: "".to_owned(),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn handle_whitespace_description_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            RecordDecisionRequest {
                description: "   ".to_owned(),
                nature: None,
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn handle_invalid_nature_returns_error() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            RecordDecisionRequest {
                description: "Test decision".to_owned(),
                nature: Some("invalid_nature".to_owned()),
                weight: None,
                category: None,
                examples: None,
                reason: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn handle_with_examples() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            RecordDecisionRequest {
                description: "Use snake_case for variables".to_owned(),
                nature: Some("convention".to_owned()),
                weight: Some("rule".to_owned()),
                category: Some("naming".to_owned()),
                examples: Some(vec![ExampleInput {
                    file: "src/lib.rs".to_owned(),
                    line: Some(5),
                    end_line: Some(5),
                    snippet: Some("let my_var = 42;".to_owned()),
                }]),
                reason: None,
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["nature"], "convention");
        assert_eq!(parsed["data"]["weight"], "rule");
    }
}
