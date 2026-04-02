//! Thin handler for the `query_project_context` MCP tool.
//!
//! Parses MCP input, calls `seshat_graph::query_project_context`, and wraps
//! the result in a `ResponseEnvelope`. No business logic lives here.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use rmcp::schemars;
use rusqlite::Connection;

use crate::envelope::{ErrorCode, ErrorEnvelope, ResponseEnvelope, ResponseMetadata};

/// Request parameters for `query_project_context`.
#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct ProjectContextRequest {
    /// Optional focus area to filter results (e.g., "logging", "testing").
    /// Filters conventions by case-insensitive substring match on description.
    #[schemars(description = "Optional domain to focus on (e.g., 'logging', 'testing')")]
    pub focus_area: Option<String>,
}

/// Execute the `query_project_context` tool.
///
/// Thin layer: parse input → call `seshat_graph::query_project_context` → wrap
/// in envelope. Returns the serialised JSON envelope as a `String`.
pub fn handle(
    conn: &Arc<Mutex<Connection>>,
    repo_name: &str,
    branch: &str,
    req: ProjectContextRequest,
) -> String {
    let start = Instant::now();
    let tool = "query_project_context";

    let result = seshat_graph::query_project_context(conn, branch, req.focus_area.as_deref());

    match result {
        Ok(data) => {
            let mut next_steps =
                vec!["Use query_convention to explore specific conventions".to_owned()];

            // Suggest focus areas based on what we found.
            if !data.dependencies.by_domain.is_empty() {
                if let Some(top) = data.dependencies.by_domain.first() {
                    next_steps.push(format!(
                        "Query conventions for '{}' domain: query_convention(topic: '{}')",
                        top.domain, top.domain
                    ));
                }
            }

            let metadata = ResponseMetadata::new(next_steps).with_extra(
                "focus_area",
                req.focus_area
                    .as_deref()
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null),
            );

            let envelope =
                ResponseEnvelope::success(tool, repo_name, branch, data, metadata, start);

            serde_json::to_string(&envelope).unwrap_or_else(|e| {
                let err = ErrorEnvelope::new(
                    tool,
                    repo_name,
                    ErrorCode::InternalError,
                    format!("Failed to serialize response: {e}"),
                    "Please report this issue".to_owned(),
                );
                serde_json::to_string(&err).unwrap_or_default()
            })
        }
        Err(e) => {
            let err = ErrorEnvelope::new(
                tool,
                repo_name,
                ErrorCode::InternalError,
                format!("{e}"),
                "Check database and retry".to_owned(),
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::test_helpers::make_project_file;
    use seshat_core::{
        BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, Language, NodeId,
    };
    use seshat_storage::{
        Database, FileIRRepository, NodeRepository, SqliteFileIRRepository, SqliteNodeRepository,
    };

    fn test_conn() -> Arc<Mutex<Connection>> {
        let db = Database::open(":memory:").expect("in-memory DB");
        db.connection().clone()
    }

    fn insert_file(conn: &Arc<Mutex<Connection>>, path: &str, lang: Language) {
        let repo = SqliteFileIRRepository::new(conn.clone());
        let branch = BranchId::from("main");
        let mut file = make_project_file(lang);
        file.path = path.into();
        file.content_hash = format!("hash_{path}");
        repo.upsert(&branch, &file, None).unwrap();
    }

    fn insert_convention(conn: &Arc<Mutex<Connection>>, description: &str, confidence: f64) {
        let repo = SqliteNodeRepository::new(conn.clone());
        let mut ext = serde_json::Map::new();
        ext.insert("source".into(), "auto_detected".into());
        ext.insert("detector_name".into(), "dependency_usage".into());
        ext.insert("adoption_rate".into(), serde_json::json!(confidence));

        let node = KnowledgeNode {
            id: NodeId(0),
            branch_id: BranchId::from("main"),
            nature: KnowledgeNature::Convention,
            weight: KnowledgeWeight::Strong,
            confidence,
            adoption_count: (confidence * 10.0) as u32,
            total_count: 10,
            description: description.to_owned(),
            ext_data: Some(serde_json::Value::Object(ext)),
        };
        repo.insert(&node).unwrap();
    }

    #[test]
    fn handle_returns_success_envelope() {
        let conn = test_conn();
        insert_file(&conn, "src/main.rs", Language::Rust);
        insert_convention(&conn, "Uses reqwest for HTTP client (Rust)", 0.9);

        let result = handle(
            &conn,
            "test-project",
            "main",
            ProjectContextRequest { focus_area: None },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["tool"], "query_project_context");
        assert_eq!(parsed["repo"], "test-project");
        assert_eq!(parsed["branch"], "main");
        assert_eq!(parsed["scope"], "root");
        assert!(parsed["duration_ms"].is_number());
        assert!(parsed["data"]["languages"].is_array());
        assert!(parsed["data"]["golden_files"].is_array());
        assert!(parsed["data"]["submodules"].is_array());
        assert_eq!(parsed["data"]["conventions_count"], 1);
    }

    #[test]
    fn handle_with_focus_area() {
        let conn = test_conn();
        insert_convention(&conn, "Uses reqwest for HTTP client (Rust)", 0.9);
        insert_convention(&conn, "snake_case naming (Rust)", 0.95);

        let result = handle(
            &conn,
            "test-project",
            "main",
            ProjectContextRequest {
                focus_area: Some("HTTP".to_owned()),
            },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["conventions_count"], 1);
        assert_eq!(parsed["metadata"]["focus_area"], "HTTP");
    }

    #[test]
    fn handle_empty_db() {
        let conn = test_conn();

        let result = handle(
            &conn,
            "test-project",
            "main",
            ProjectContextRequest { focus_area: None },
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["conventions_count"], 0);
        assert_eq!(parsed["data"]["languages"].as_array().unwrap().len(), 0);
    }
}
