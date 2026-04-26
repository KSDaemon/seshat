//! SQLite implementation of [`NodeRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use seshat_core::{BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, NodeId};

use super::NodeRepository;
use crate::StorageError;

/// SQLite-backed node repository.
#[derive(Debug, Clone)]
pub struct SqliteNodeRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteNodeRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })
    }
}

/// Serialize `ext_data` to a JSON string for storage.
fn serialize_ext_data(data: &Option<serde_json::Value>) -> Result<Option<String>, StorageError> {
    data.as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| StorageError::SerializationError(e.to_string()))
}

impl NodeRepository for SqliteNodeRepository {
    fn insert(&self, node: &KnowledgeNode) -> Result<KnowledgeNode, StorageError> {
        let conn = self.conn()?;

        let ext_data_str = serialize_ext_data(&node.ext_data)?;

        conn.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                node.branch_id.0,
                node.nature.as_str(),
                node.weight.as_str(),
                node.confidence,
                node.adoption_count,
                node.total_count,
                node.description,
                ext_data_str,
            ],
        )?;

        let id = conn.last_insert_rowid();

        let mut inserted = node.clone();
        inserted.id = NodeId(id);
        Ok(inserted)
    }

    fn get_by_id(&self, id: NodeId) -> Result<KnowledgeNode, StorageError> {
        let conn = self.conn()?;

        conn.query_row(
            "SELECT id, branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data
             FROM nodes WHERE id = ?1",
            params![id.0],
            row_to_node,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => StorageError::NotFound {
                entity: "Node",
                id: id.0.to_string(),
            },
            other => StorageError::from(other),
        })
    }

    fn find_by_nature(&self, nature: KnowledgeNature) -> Result<Vec<KnowledgeNode>, StorageError> {
        self.query_nodes(
            "SELECT id, branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data
             FROM nodes WHERE nature = ?1",
            &nature.as_str(),
        )
    }

    fn find_by_branch(&self, branch_id: &BranchId) -> Result<Vec<KnowledgeNode>, StorageError> {
        self.query_nodes(
            "SELECT id, branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data
             FROM nodes WHERE branch_id = ?1",
            &branch_id.0,
        )
    }

    fn update(&self, node: &KnowledgeNode) -> Result<(), StorageError> {
        let conn = self.conn()?;

        let ext_data_str = serialize_ext_data(&node.ext_data)?;

        let affected = conn.execute(
            "UPDATE nodes SET branch_id = ?1, nature = ?2, weight = ?3, confidence = ?4,
             adoption_count = ?5, total_count = ?6, description = ?7, ext_data = ?8
             WHERE id = ?9",
            params![
                node.branch_id.0,
                node.nature.as_str(),
                node.weight.as_str(),
                node.confidence,
                node.adoption_count,
                node.total_count,
                node.description,
                ext_data_str,
                node.id.0,
            ],
        )?;

        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "Node",
                id: node.id.0.to_string(),
            });
        }

        Ok(())
    }

    fn delete(&self, id: NodeId) -> Result<(), StorageError> {
        let conn = self.conn()?;

        let affected = conn.execute("DELETE FROM nodes WHERE id = ?1", params![id.0])?;

        if affected == 0 {
            return Err(StorageError::NotFound {
                entity: "Node",
                id: id.0.to_string(),
            });
        }

        Ok(())
    }

    fn delete_by_branch(&self, branch_id: &BranchId) -> Result<usize, StorageError> {
        let conn = self.conn()?;

        let affected = conn.execute(
            "DELETE FROM nodes WHERE branch_id = ?1",
            params![branch_id.0],
        )?;

        Ok(affected)
    }

    fn delete_facts_by_branch(&self, branch_id: &BranchId) -> Result<usize, StorageError> {
        let conn = self.conn()?;

        let affected = conn.execute(
            "DELETE FROM nodes WHERE branch_id = ?1 AND nature = 'fact'",
            params![branch_id.0],
        )?;

        Ok(affected)
    }

    fn delete_auto_detected_by_branch(&self, branch_id: &BranchId) -> Result<usize, StorageError> {
        let conn = self.conn()?;

        let affected = conn.execute(
            "DELETE FROM nodes WHERE branch_id = ?1
             AND json_extract(ext_data, '$.source') = 'auto_detected'",
            params![branch_id.0],
        )?;

        Ok(affected)
    }

    fn find_conventions_by_branch(
        &self,
        branch_id: &BranchId,
    ) -> Result<Vec<KnowledgeNode>, StorageError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data
            FROM nodes
            WHERE branch_id = ?1
              AND json_extract(ext_data, '$.source') IN ('auto_detected', 'user')
              AND COALESCE(json_extract(ext_data, '$.removed'), 0) NOT IN (1, 'true')",
        )?;
        let rows = stmt.query_map(params![branch_id.0], row_to_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl SqliteNodeRepository {
    /// Run a parameterised node query and collect the results.
    fn query_nodes(
        &self,
        sql: &str,
        param: &dyn rusqlite::types::ToSql,
    ) -> Result<Vec<KnowledgeNode>, StorageError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([param], row_to_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// Map a rusqlite Row to a `KnowledgeNode`.
fn row_to_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeNode> {
    let id: i64 = row.get(0)?;
    let branch_id: String = row.get(1)?;
    let nature_str: String = row.get(2)?;
    let weight_str: String = row.get(3)?;
    let confidence: f64 = row.get(4)?;
    let adoption_count: u32 = row.get(5)?;
    let total_count: u32 = row.get(6)?;
    let description: String = row.get(7)?;
    let ext_data_str: Option<String> = row.get(8)?;

    let nature: KnowledgeNature = nature_str.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let weight: KnowledgeWeight = weight_str.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let ext_data = ext_data_str
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
        })?;

    Ok(KnowledgeNode {
        id: NodeId(id),
        branch_id: BranchId(branch_id),
        nature,
        weight,
        confidence,
        adoption_count,
        total_count,
        description,
        ext_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use seshat_core::test_helpers::make_knowledge_node;

    /// Helper: create an in-memory DB and return a `SqliteNodeRepository`.
    fn test_repo() -> SqliteNodeRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqliteNodeRepository::new(db.connection().clone())
    }

    #[test]
    fn insert_and_get_by_id() {
        let repo = test_repo();
        let node = make_knowledge_node(KnowledgeNature::Convention, 0.9);

        let inserted = repo.insert(&node).expect("insert should succeed");
        assert_ne!(inserted.id.0, 0, "should get assigned ID");

        let fetched = repo
            .get_by_id(inserted.id)
            .expect("get_by_id should succeed");
        assert_eq!(fetched.id, inserted.id);
        assert_eq!(fetched.nature, KnowledgeNature::Convention);
        assert_eq!(fetched.weight, KnowledgeWeight::Strong);
        assert!((fetched.confidence - 0.9).abs() < f64::EPSILON);
        assert_eq!(fetched.branch_id, BranchId::from("main"));
    }

    #[test]
    fn insert_with_ext_data() {
        let repo = test_repo();
        let mut node = make_knowledge_node(KnowledgeNature::Decision, 1.0);
        node.ext_data = Some(serde_json::json!({"reasoning": "perf requirement"}));
        node.description = "Use SQLite".to_string();

        let inserted = repo.insert(&node).expect("insert");
        let fetched = repo.get_by_id(inserted.id).expect("get");

        assert_eq!(
            fetched.ext_data.as_ref().unwrap()["reasoning"],
            "perf requirement"
        );
        assert_eq!(fetched.description, "Use SQLite");
    }

    #[test]
    fn get_by_id_not_found() {
        let repo = test_repo();
        let result = repo.get_by_id(NodeId(999));

        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::NotFound { entity, id } => {
                assert_eq!(entity, "Node");
                assert_eq!(id, "999");
            }
            other => panic!("expected NotFound, got: {other}"),
        }
    }

    #[test]
    fn find_by_nature() {
        let repo = test_repo();

        let n1 = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        let n2 = make_knowledge_node(KnowledgeNature::Fact, 0.5);
        let n3 = make_knowledge_node(KnowledgeNature::Convention, 0.6);

        repo.insert(&n1).unwrap();
        repo.insert(&n2).unwrap();
        repo.insert(&n3).unwrap();

        let conventions = repo.find_by_nature(KnowledgeNature::Convention).unwrap();
        assert_eq!(conventions.len(), 2);

        let facts = repo.find_by_nature(KnowledgeNature::Fact).unwrap();
        assert_eq!(facts.len(), 1);
    }

    #[test]
    fn find_by_branch() {
        let repo = test_repo();

        let mut n1 = make_knowledge_node(KnowledgeNature::Fact, 0.8);
        n1.branch_id = BranchId::from("feature-a");

        let n2 = make_knowledge_node(KnowledgeNature::Fact, 0.8);
        // n2 defaults to branch "main"

        repo.insert(&n1).unwrap();
        repo.insert(&n2).unwrap();

        let feature_nodes = repo.find_by_branch(&BranchId::from("feature-a")).unwrap();
        assert_eq!(feature_nodes.len(), 1);

        let main_nodes = repo.find_by_branch(&BranchId::from("main")).unwrap();
        assert_eq!(main_nodes.len(), 1);
    }

    #[test]
    fn update_node() {
        let repo = test_repo();
        let node = make_knowledge_node(KnowledgeNature::Convention, 0.9);

        let mut inserted = repo.insert(&node).unwrap();
        inserted.description = "Updated description".to_string();
        inserted.confidence = 0.95;
        inserted.adoption_count = 19;
        inserted.total_count = 20;

        repo.update(&inserted).expect("update should succeed");

        let fetched = repo.get_by_id(inserted.id).unwrap();
        assert_eq!(fetched.description, "Updated description");
        assert!((fetched.confidence - 0.95).abs() < f64::EPSILON);
        assert_eq!(fetched.adoption_count, 19);
        assert_eq!(fetched.total_count, 20);
    }

    #[test]
    fn update_not_found() {
        let repo = test_repo();
        let mut node = make_knowledge_node(KnowledgeNature::Fact, 0.5);
        node.id = NodeId(999);

        let result = repo.update(&node);
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[test]
    fn delete_node() {
        let repo = test_repo();
        let node = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        let inserted = repo.insert(&node).unwrap();

        repo.delete(inserted.id).expect("delete should succeed");

        let result = repo.get_by_id(inserted.id);
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[test]
    fn delete_not_found() {
        let repo = test_repo();
        let result = repo.delete(NodeId(999));
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[test]
    fn all_nature_variants_roundtrip() {
        let repo = test_repo();

        let natures = [
            KnowledgeNature::Fact,
            KnowledgeNature::Convention,
            KnowledgeNature::Observation,
            KnowledgeNature::Decision,
            KnowledgeNature::Preference,
        ];

        for nature in natures {
            let node = make_knowledge_node(nature, 0.5);
            let inserted = repo.insert(&node).unwrap();
            let fetched = repo.get_by_id(inserted.id).unwrap();
            assert_eq!(
                fetched.nature, nature,
                "nature roundtrip failed for {nature}"
            );
        }
    }

    #[test]
    fn all_weight_variants_roundtrip() {
        let repo = test_repo();

        let cases: [(KnowledgeWeight, f64); 5] = [
            (KnowledgeWeight::Info, 0.1),
            (KnowledgeWeight::Weak, 0.3),
            (KnowledgeWeight::Moderate, 0.6),
            (KnowledgeWeight::Strong, 0.9),
            (KnowledgeWeight::Rule, 1.0),
        ];

        for (expected_weight, confidence) in cases {
            let mut node = make_knowledge_node(KnowledgeNature::Fact, confidence);
            // Override the weight to test independently from auto-assignment
            node.weight = expected_weight;
            let inserted = repo.insert(&node).unwrap();
            let fetched = repo.get_by_id(inserted.id).unwrap();
            assert_eq!(
                fetched.weight, expected_weight,
                "weight roundtrip failed for {expected_weight}"
            );
        }
    }

    #[test]
    fn delete_by_branch() {
        let repo = test_repo();
        let branch_a = BranchId::from("branch-a");
        let branch_b = BranchId::from("branch-b");

        let mut n1 = make_knowledge_node(KnowledgeNature::Fact, 0.8);
        n1.branch_id = branch_a.clone();
        let mut n2 = make_knowledge_node(KnowledgeNature::Fact, 0.7);
        n2.branch_id = branch_a.clone();
        let mut n3 = make_knowledge_node(KnowledgeNature::Fact, 0.6);
        n3.branch_id = branch_b.clone();

        repo.insert(&n1).unwrap();
        repo.insert(&n2).unwrap();
        repo.insert(&n3).unwrap();

        let deleted = repo.delete_by_branch(&branch_a).unwrap();
        assert_eq!(deleted, 2, "should delete 2 nodes from branch-a");

        let a_nodes = repo.find_by_branch(&branch_a).unwrap();
        assert!(a_nodes.is_empty(), "branch-a should have no nodes");

        let b_nodes = repo.find_by_branch(&branch_b).unwrap();
        assert_eq!(b_nodes.len(), 1, "branch-b should still have 1 node");
    }

    #[test]
    fn delete_by_branch_empty() {
        let repo = test_repo();
        let branch = BranchId::from("empty-branch");

        let deleted = repo.delete_by_branch(&branch).unwrap();
        assert_eq!(deleted, 0, "should delete 0 nodes from empty branch");
    }

    #[test]
    fn delete_auto_detected_preserves_user_decisions() {
        let repo = test_repo();
        let branch = BranchId::from("main");

        // Auto-detected convention
        let mut auto_node = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        auto_node.branch_id = branch.clone();
        auto_node.description = "Uses thiserror".to_string();
        auto_node.ext_data = Some(serde_json::json!({
            "source": "auto_detected",
            "detector_name": "error_handling"
        }));
        repo.insert(&auto_node).unwrap();

        // User-recorded decision
        let mut user_node = make_knowledge_node(KnowledgeNature::Decision, 1.0);
        user_node.branch_id = branch.clone();
        user_node.description = "Always use Result".to_string();
        user_node.ext_data = Some(serde_json::json!({
            "source": "user",
            "user_confirmed": true
        }));
        repo.insert(&user_node).unwrap();

        // Module fact (no source field in ext_data)
        let mut fact_node = make_knowledge_node(KnowledgeNature::Fact, 0.8);
        fact_node.branch_id = branch.clone();
        fact_node.description = "Module: seshat-core".to_string();
        repo.insert(&fact_node).unwrap();

        let deleted = repo.delete_auto_detected_by_branch(&branch).unwrap();
        assert_eq!(deleted, 1, "should only delete auto_detected node");

        let all_nodes = repo.find_by_branch(&branch).unwrap();
        assert_eq!(all_nodes.len(), 2, "user decision + fact should remain");

        // Verify the user node is still there
        let user = all_nodes
            .iter()
            .find(|n| n.description == "Always use Result");
        assert!(user.is_some(), "user decision should be preserved");

        // Verify the fact node is still there
        let fact = all_nodes
            .iter()
            .find(|n| n.description == "Module: seshat-core");
        assert!(fact.is_some(), "fact node should be preserved");
    }

    #[test]
    fn delete_auto_detected_empty_branch() {
        let repo = test_repo();
        let branch = BranchId::from("empty");

        let deleted = repo.delete_auto_detected_by_branch(&branch).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn find_conventions_by_branch_returns_auto_and_user() {
        let repo = test_repo();
        let branch = BranchId::from("main");

        // Auto-detected convention
        let mut auto_node = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        auto_node.branch_id = branch.clone();
        auto_node.description = "Uses thiserror".to_string();
        auto_node.ext_data = Some(serde_json::json!({
            "source": "auto_detected",
            "detector_name": "error_handling"
        }));
        repo.insert(&auto_node).unwrap();

        // User-recorded decision
        let mut user_node = make_knowledge_node(KnowledgeNature::Decision, 1.0);
        user_node.branch_id = branch.clone();
        user_node.description = "Always use Result".to_string();
        user_node.ext_data = Some(serde_json::json!({
            "source": "user",
            "user_confirmed": true
        }));
        repo.insert(&user_node).unwrap();

        // Module fact (no source field — should NOT appear)
        let mut fact_node = make_knowledge_node(KnowledgeNature::Fact, 0.8);
        fact_node.branch_id = branch.clone();
        fact_node.description = "Module: seshat-core".to_string();
        repo.insert(&fact_node).unwrap();

        let conventions = repo.find_conventions_by_branch(&branch).unwrap();
        assert_eq!(
            conventions.len(),
            2,
            "should return auto_detected + user nodes"
        );

        let descriptions: Vec<&str> = conventions.iter().map(|n| n.description.as_str()).collect();
        assert!(descriptions.contains(&"Uses thiserror"));
        assert!(descriptions.contains(&"Always use Result"));
    }

    #[test]
    fn find_conventions_by_branch_excludes_other_branches() {
        let repo = test_repo();

        let mut n1 = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        n1.branch_id = BranchId::from("main");
        n1.ext_data = Some(serde_json::json!({"source": "auto_detected"}));
        repo.insert(&n1).unwrap();

        let mut n2 = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        n2.branch_id = BranchId::from("feature");
        n2.ext_data = Some(serde_json::json!({"source": "auto_detected"}));
        repo.insert(&n2).unwrap();

        let main_conventions = repo
            .find_conventions_by_branch(&BranchId::from("main"))
            .unwrap();
        assert_eq!(main_conventions.len(), 1);

        let feature_conventions = repo
            .find_conventions_by_branch(&BranchId::from("feature"))
            .unwrap();
        assert_eq!(feature_conventions.len(), 1);
    }
}
