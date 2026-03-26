//! SQLite implementation of [`EdgeRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use seshat_core::{BranchId, Edge, EdgeId, EdgeType, NodeId};

use super::EdgeRepository;
use crate::StorageError;

/// SQLite-backed edge repository.
#[derive(Debug, Clone)]
pub struct SqliteEdgeRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteEdgeRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

impl EdgeRepository for SqliteEdgeRepository {
    fn insert(&self, edge: &Edge) -> Result<Edge, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let edge_type_str = edge.edge_type.as_str();
        let metadata_str = edge
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        conn.execute(
            "INSERT INTO edges (source_id, target_id, edge_type, branch_id, weight, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                edge.source_id.0,
                edge.target_id.0,
                edge_type_str,
                edge.branch_id.0,
                edge.weight,
                metadata_str,
            ],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let id = conn.last_insert_rowid();

        let mut inserted = edge.clone();
        inserted.id = EdgeId(id);
        Ok(inserted)
    }

    fn find_by_source(&self, source_id: NodeId) -> Result<Vec<Edge>, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, source_id, target_id, edge_type, branch_id, weight, metadata
                 FROM edges WHERE source_id = ?1",
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let rows = stmt
            .query_map(params![source_id.0], row_to_edge)
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Sqlite(e.to_string()))
    }

    fn find_by_target(&self, target_id: NodeId) -> Result<Vec<Edge>, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, source_id, target_id, edge_type, branch_id, weight, metadata
                 FROM edges WHERE target_id = ?1",
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let rows = stmt
            .query_map(params![target_id.0], row_to_edge)
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Sqlite(e.to_string()))
    }

    fn find_by_type(&self, edge_type: EdgeType) -> Result<Vec<Edge>, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let edge_type_str = edge_type.as_str();
        let mut stmt = conn
            .prepare(
                "SELECT id, source_id, target_id, edge_type, branch_id, weight, metadata
                 FROM edges WHERE edge_type = ?1",
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let rows = stmt
            .query_map(params![edge_type_str], row_to_edge)
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Sqlite(e.to_string()))
    }

    fn delete(&self, id: EdgeId) -> Result<(), StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let affected = conn
            .execute("DELETE FROM edges WHERE id = ?1", params![id.0])
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "Edge".to_string(),
                id: id.0.to_string(),
            });
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a rusqlite Row to an `Edge`.
fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    let id: i64 = row.get(0)?;
    let source_id: i64 = row.get(1)?;
    let target_id: i64 = row.get(2)?;
    let edge_type_str: String = row.get(3)?;
    let branch_id: String = row.get(4)?;
    let weight: f64 = row.get(5)?;
    let metadata_str: Option<String> = row.get(6)?;

    let edge_type: EdgeType = edge_type_str.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let metadata = metadata_str
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?;

    Ok(Edge {
        id: EdgeId(id),
        source_id: NodeId(source_id),
        target_id: NodeId(target_id),
        edge_type,
        branch_id: BranchId(branch_id),
        weight,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use crate::repository::NodeRepository;
    use crate::repository::node_repository::SqliteNodeRepository;
    use seshat_core::KnowledgeNature;
    use seshat_core::test_helpers::make_knowledge_node;

    /// Helper: create an in-memory DB and return both repos (edges need nodes for FK).
    fn test_repos() -> (SqliteNodeRepository, SqliteEdgeRepository) {
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        (
            SqliteNodeRepository::new(conn.clone()),
            SqliteEdgeRepository::new(conn),
        )
    }

    /// Helper: insert two nodes and return their IDs.
    fn insert_two_nodes(node_repo: &SqliteNodeRepository) -> (NodeId, NodeId) {
        let n1 = make_knowledge_node(KnowledgeNature::Fact, 0.8);
        let n2 = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        let id1 = node_repo.insert(&n1).unwrap().id;
        let id2 = node_repo.insert(&n2).unwrap().id;
        (id1, id2)
    }

    fn make_edge(source_id: NodeId, target_id: NodeId, edge_type: EdgeType) -> Edge {
        Edge {
            id: EdgeId(0),
            source_id,
            target_id,
            edge_type,
            branch_id: BranchId::from("main"),
            weight: 1.0,
            metadata: None,
        }
    }

    #[test]
    fn insert_and_find_by_source() {
        let (node_repo, edge_repo) = test_repos();
        let (n1, n2) = insert_two_nodes(&node_repo);

        let edge = make_edge(n1, n2, EdgeType::DependsOn);
        let inserted = edge_repo.insert(&edge).expect("insert should succeed");
        assert_ne!(inserted.id.0, 0, "should get assigned ID");

        let edges = edge_repo.find_by_source(n1).expect("find_by_source");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_type, EdgeType::DependsOn);
        assert_eq!(edges[0].source_id, n1);
        assert_eq!(edges[0].target_id, n2);
    }

    #[test]
    fn find_by_target() {
        let (node_repo, edge_repo) = test_repos();
        let (n1, n2) = insert_two_nodes(&node_repo);

        let edge = make_edge(n1, n2, EdgeType::RelatedTo);
        edge_repo.insert(&edge).unwrap();

        let edges = edge_repo.find_by_target(n2).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_id, n2);
    }

    #[test]
    fn find_by_type() {
        let (node_repo, edge_repo) = test_repos();
        let (n1, n2) = insert_two_nodes(&node_repo);

        let e1 = make_edge(n1, n2, EdgeType::DependsOn);
        let e2 = make_edge(n2, n1, EdgeType::RelatedTo);
        let e3 = make_edge(n1, n2, EdgeType::DependsOn);
        edge_repo.insert(&e1).unwrap();
        edge_repo.insert(&e2).unwrap();
        edge_repo.insert(&e3).unwrap();

        let depends = edge_repo.find_by_type(EdgeType::DependsOn).unwrap();
        assert_eq!(depends.len(), 2);

        let related = edge_repo.find_by_type(EdgeType::RelatedTo).unwrap();
        assert_eq!(related.len(), 1);
    }

    #[test]
    fn delete_edge() {
        let (node_repo, edge_repo) = test_repos();
        let (n1, n2) = insert_two_nodes(&node_repo);

        let edge = make_edge(n1, n2, EdgeType::PartOf);
        let inserted = edge_repo.insert(&edge).unwrap();

        edge_repo
            .delete(inserted.id)
            .expect("delete should succeed");

        let edges = edge_repo.find_by_source(n1).unwrap();
        assert!(edges.is_empty(), "edge should be deleted");
    }

    #[test]
    fn delete_not_found() {
        let (_node_repo, edge_repo) = test_repos();
        let result = edge_repo.delete(EdgeId(999));
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[test]
    fn insert_with_metadata() {
        let (node_repo, edge_repo) = test_repos();
        let (n1, n2) = insert_two_nodes(&node_repo);

        let mut edge = make_edge(n1, n2, EdgeType::Implements);
        edge.metadata = Some(serde_json::json!({"via": "trait impl"}));

        let inserted = edge_repo.insert(&edge).unwrap();
        let edges = edge_repo.find_by_source(n1).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id, inserted.id);
        assert_eq!(edges[0].metadata.as_ref().unwrap()["via"], "trait impl");
    }

    #[test]
    fn all_edge_type_variants_roundtrip() {
        let (node_repo, edge_repo) = test_repos();
        let (n1, n2) = insert_two_nodes(&node_repo);

        let types = [
            EdgeType::RelatedTo,
            EdgeType::Updates,
            EdgeType::Contradicts,
            EdgeType::PartOf,
            EdgeType::DependsOn,
            EdgeType::Implements,
        ];

        for et in types {
            let edge = make_edge(n1, n2, et);
            edge_repo.insert(&edge).unwrap();
        }

        // All 6 should be retrievable via find_by_source
        let all_edges = edge_repo.find_by_source(n1).unwrap();
        assert_eq!(all_edges.len(), 6);

        // Each type should match when queried individually
        for et in types {
            let found = edge_repo.find_by_type(et).unwrap();
            assert!(!found.is_empty(), "should find edges of type {et}");
        }
    }
}
