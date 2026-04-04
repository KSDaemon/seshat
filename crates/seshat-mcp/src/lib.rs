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

pub mod call_logger;
pub mod envelope;
pub mod error;
pub mod scope;
pub mod server;
pub mod tools;

pub use envelope::{ErrorCode, ErrorDetail, ErrorEnvelope, ResponseEnvelope, ResponseMetadata};
pub use error::McpError;
pub use scope::ProjectConnection;
pub use server::{McpServer, start_stdio, start_stdio_with_shutdown};

/// Shared test helpers for MCP tool tests.
///
/// Provides reusable fixtures like in-memory connections, convention insertion,
/// and decision recording so individual tool test modules stay DRY.
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::{Arc, Mutex};

    use rusqlite::Connection;
    use seshat_core::{BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, NodeId};
    use seshat_storage::{Database, NodeRepository, SqliteNodeRepository};

    use crate::scope::ProjectConnection;

    /// Open an in-memory database and return the shared connection.
    pub fn test_conn() -> Arc<Mutex<Connection>> {
        let db = Database::open(":memory:").expect("in-memory DB");
        db.connection().clone()
    }

    /// Create a `ProjectConnection` backed by an in-memory DB.
    pub fn make_conn(name: &str, branch: &str) -> ProjectConnection {
        let conn = test_conn();
        ProjectConnection::new(conn, name, branch)
    }

    /// Insert an auto-detected convention node for testing.
    pub fn insert_convention(
        conn: &Arc<Mutex<Connection>>,
        description: &str,
        detector_name: &str,
        confidence: f64,
    ) {
        let repo = SqliteNodeRepository::new(conn.clone());
        let mut ext = serde_json::Map::new();
        ext.insert("source".into(), "auto_detected".into());
        ext.insert("detector_name".into(), detector_name.into());
        ext.insert("trend".into(), "stable".into());
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

    /// Record a user decision and return its node ID.
    pub fn record_test_decision(conn: &Arc<Mutex<Connection>>) -> i64 {
        let result = seshat_graph::record_decision(
            conn,
            "main",
            seshat_graph::RecordDecisionParams {
                description: "Test decision for removal/update".to_owned(),
                nature: "decision".to_owned(),
                weight: "strong".to_owned(),
                category: Some("testing".to_owned()),
                examples: vec![],
                reason: None,
            },
        )
        .unwrap();
        result.id
    }
}
