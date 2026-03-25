//! SQLite implementation of [`BranchRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use seshat_core::BranchId;

use super::BranchRepository;
use crate::StorageError;

/// Key used in the `metadata` table to store the current branch.
const CURRENT_BRANCH_KEY: &str = "current_branch";

/// Default branch name when none has been set.
const DEFAULT_BRANCH: &str = "main";

/// SQLite-backed branch repository.
#[derive(Debug, Clone)]
pub struct SqliteBranchRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteBranchRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

impl BranchRepository for SqliteBranchRepository {
    fn create_snapshot(
        &self,
        source_branch: &BranchId,
        new_branch: &BranchId,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        // Copy nodes
        tx.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             SELECT ?1, nature, weight, confidence, adoption_count, total_count, description, ext_data
             FROM nodes WHERE branch_id = ?2",
            params![new_branch.0, source_branch.0],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        // Copy edges — only edges that belong to the source branch
        tx.execute(
            "INSERT INTO edges (source_id, target_id, edge_type, branch_id, weight, metadata)
             SELECT source_id, target_id, edge_type, ?1, weight, metadata
             FROM edges WHERE branch_id = ?2",
            params![new_branch.0, source_branch.0],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        // Copy files_ir
        tx.execute(
            "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data, updated_at)
             SELECT ?1, file_path, language, content_hash, ir_data, updated_at
             FROM files_ir WHERE branch_id = ?2",
            params![new_branch.0, source_branch.0],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        tx.commit()
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        Ok(())
    }

    fn switch_branch(&self, branch_id: &BranchId) -> Result<(), StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        conn.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![CURRENT_BRANCH_KEY, branch_id.0],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        Ok(())
    }

    fn delete_branch(&self, branch_id: &BranchId) -> Result<(), StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        // Delete edges first (they reference nodes via FK)
        tx.execute(
            "DELETE FROM edges WHERE branch_id = ?1",
            params![branch_id.0],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        tx.execute(
            "DELETE FROM nodes WHERE branch_id = ?1",
            params![branch_id.0],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        tx.execute(
            "DELETE FROM files_ir WHERE branch_id = ?1",
            params![branch_id.0],
        )
        .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        tx.commit()
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        Ok(())
    }

    fn list_branches(&self) -> Result<Vec<BranchId>, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        // Collect distinct branch IDs across all three tables
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT branch_id FROM (
                     SELECT branch_id FROM nodes
                     UNION
                     SELECT branch_id FROM edges
                     UNION
                     SELECT branch_id FROM files_ir
                 ) ORDER BY branch_id",
            )
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                Ok(BranchId(id))
            })
            .map_err(|e| StorageError::Sqlite(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::Sqlite(e.to_string()))
    }

    fn get_current_branch(&self) -> Result<BranchId, StorageError> {
        let conn = self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })?;

        let result: Result<String, _> = conn.query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            params![CURRENT_BRANCH_KEY],
            |row| row.get(0),
        );

        match result {
            Ok(branch) => Ok(BranchId(branch)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(BranchId(DEFAULT_BRANCH.to_string())),
            Err(e) => Err(StorageError::Sqlite(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::file_ir_repository::SqliteFileIRRepository;
    use crate::repository::node_repository::SqliteNodeRepository;
    use crate::repository::{FileIRRepository, NodeRepository};
    use crate::Database;
    use seshat_core::test_helpers::{make_knowledge_node, make_project_file};
    use seshat_core::{KnowledgeNature, Language};

    /// Helper: create an in-memory DB and return repos for testing.
    fn test_repos() -> (
        SqliteBranchRepository,
        SqliteNodeRepository,
        SqliteFileIRRepository,
    ) {
        let db = Database::open(":memory:").expect("in-memory DB");
        let conn = db.connection().clone();
        (
            SqliteBranchRepository::new(conn.clone()),
            SqliteNodeRepository::new(conn.clone()),
            SqliteFileIRRepository::new(conn),
        )
    }

    #[test]
    fn get_current_branch_default() {
        let (branch_repo, _, _) = test_repos();
        let current = branch_repo.get_current_branch().unwrap();
        assert_eq!(current, BranchId::from("main"));
    }

    #[test]
    fn switch_and_get_current_branch() {
        let (branch_repo, _, _) = test_repos();
        let feature = BranchId::from("feature-x");

        branch_repo.switch_branch(&feature).unwrap();
        let current = branch_repo.get_current_branch().unwrap();
        assert_eq!(current, feature);
    }

    #[test]
    fn switch_branch_overwrites() {
        let (branch_repo, _, _) = test_repos();

        branch_repo
            .switch_branch(&BranchId::from("branch-a"))
            .unwrap();
        branch_repo
            .switch_branch(&BranchId::from("branch-b"))
            .unwrap();

        let current = branch_repo.get_current_branch().unwrap();
        assert_eq!(current, BranchId::from("branch-b"));
    }

    #[test]
    fn create_snapshot_copies_nodes_and_files() {
        let (branch_repo, node_repo, file_repo) = test_repos();
        let main_branch = BranchId::from("main");

        // Insert nodes on main
        let mut n1 = make_knowledge_node(KnowledgeNature::Convention, 0.9);
        n1.branch_id = main_branch.clone();
        node_repo.insert(&n1).unwrap();

        let mut n2 = make_knowledge_node(KnowledgeNature::Fact, 0.7);
        n2.branch_id = main_branch.clone();
        node_repo.insert(&n2).unwrap();

        // Insert a file IR on main
        let mut file = make_project_file(Language::Rust);
        file.path = "src/lib.rs".into();
        file.content_hash = "snap_hash".to_string();
        file_repo.upsert(&main_branch, &file).unwrap();

        // Create snapshot
        let feature = BranchId::from("feature-snap");
        branch_repo.create_snapshot(&main_branch, &feature).unwrap();

        // Verify nodes were copied
        let main_nodes = node_repo.find_by_branch(&main_branch).unwrap();
        let feature_nodes = node_repo.find_by_branch(&feature).unwrap();
        assert_eq!(main_nodes.len(), 2);
        assert_eq!(feature_nodes.len(), 2);

        // Verify file IR was copied
        let feature_files = file_repo.get_by_branch(&feature).unwrap();
        assert_eq!(feature_files.len(), 1);
        assert_eq!(feature_files[0].content_hash, "snap_hash");
    }

    #[test]
    fn create_snapshot_empty_branch() {
        let (branch_repo, node_repo, _) = test_repos();
        let empty = BranchId::from("empty");
        let target = BranchId::from("copy-of-empty");

        // Snapshot of a branch with no data should succeed
        branch_repo.create_snapshot(&empty, &target).unwrap();

        let nodes = node_repo.find_by_branch(&target).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn list_branches_empty() {
        let (branch_repo, _, _) = test_repos();
        let branches = branch_repo.list_branches().unwrap();
        assert!(branches.is_empty());
    }

    #[test]
    fn list_branches_with_data() {
        let (branch_repo, node_repo, file_repo) = test_repos();
        let main_branch = BranchId::from("main");
        let feature = BranchId::from("feature");

        // Add a node on main
        let mut n = make_knowledge_node(KnowledgeNature::Fact, 0.5);
        n.branch_id = main_branch.clone();
        node_repo.insert(&n).unwrap();

        // Add a file on feature
        let mut f = make_project_file(Language::Python);
        f.path = "app.py".into();
        f.content_hash = "h".to_string();
        file_repo.upsert(&feature, &f).unwrap();

        let branches = branch_repo.list_branches().unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches.contains(&main_branch));
        assert!(branches.contains(&feature));
    }

    #[test]
    fn delete_branch() {
        let (branch_repo, node_repo, file_repo) = test_repos();
        let branch = BranchId::from("to-delete");

        // Insert node and file
        let mut n = make_knowledge_node(KnowledgeNature::Observation, 0.6);
        n.branch_id = branch.clone();
        node_repo.insert(&n).unwrap();

        let mut f = make_project_file(Language::TypeScript);
        f.path = "index.ts".into();
        f.content_hash = "del_hash".to_string();
        file_repo.upsert(&branch, &f).unwrap();

        // Verify data exists
        assert_eq!(node_repo.find_by_branch(&branch).unwrap().len(), 1);
        assert_eq!(file_repo.get_by_branch(&branch).unwrap().len(), 1);

        // Delete branch
        branch_repo.delete_branch(&branch).unwrap();

        // Verify data was removed
        assert!(node_repo.find_by_branch(&branch).unwrap().is_empty());
        assert!(file_repo.get_by_branch(&branch).unwrap().is_empty());
    }

    #[test]
    fn delete_branch_no_data_succeeds() {
        let (branch_repo, _, _) = test_repos();
        // Deleting a branch with no data should not error
        branch_repo.delete_branch(&BranchId::from("ghost")).unwrap();
    }

    #[test]
    fn snapshot_and_delete_isolation() {
        let (branch_repo, node_repo, file_repo) = test_repos();
        let main_branch = BranchId::from("main");

        // Set up main data
        let mut n = make_knowledge_node(KnowledgeNature::Decision, 0.95);
        n.branch_id = main_branch.clone();
        node_repo.insert(&n).unwrap();

        let mut f = make_project_file(Language::Rust);
        f.path = "src/main.rs".into();
        f.content_hash = "iso_hash".to_string();
        file_repo.upsert(&main_branch, &f).unwrap();

        // Create snapshot
        let snapshot = BranchId::from("snapshot");
        branch_repo
            .create_snapshot(&main_branch, &snapshot)
            .unwrap();

        // Delete snapshot — main should be unaffected
        branch_repo.delete_branch(&snapshot).unwrap();

        assert_eq!(node_repo.find_by_branch(&main_branch).unwrap().len(), 1);
        assert_eq!(file_repo.get_by_branch(&main_branch).unwrap().len(), 1);
        assert!(node_repo.find_by_branch(&snapshot).unwrap().is_empty());
    }
}
