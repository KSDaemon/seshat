//! SQLite implementation of [`BranchRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use seshat_core::BranchId;

use super::{BranchRepository, lock_conn};
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
        let conn = lock_conn(&self.conn)?;

        let tx = conn.unchecked_transaction()?;

        // Ensure the source branch is registered in the `branches` table so
        // it shows up in `list_branches` even if no scan has happened yet.
        tx.execute(
            "INSERT OR IGNORE INTO branches (branch_id) VALUES (?1)",
            params![source_branch.0],
        )?;

        // Register the new (target) branch with `snapshot_source` set so we
        // can later trace where the snapshot came from.
        tx.execute(
            "INSERT INTO branches (branch_id, snapshot_source) VALUES (?1, ?2)
             ON CONFLICT(branch_id) DO UPDATE SET snapshot_source = excluded.snapshot_source",
            params![new_branch.0, source_branch.0],
        )?;

        // Copy nodes
        tx.execute(
            "INSERT INTO nodes (branch_id, nature, weight, confidence, adoption_count, total_count, description, ext_data)
             SELECT ?1, nature, weight, confidence, adoption_count, total_count, description, ext_data
             FROM nodes WHERE branch_id = ?2",
            params![new_branch.0, source_branch.0],
        )?;

        // Copy edges — only edges that belong to the source branch
        tx.execute(
            "INSERT INTO edges (source_id, target_id, edge_type, branch_id, weight, metadata)
             SELECT source_id, target_id, edge_type, ?1, weight, metadata
             FROM edges WHERE branch_id = ?2",
            params![new_branch.0, source_branch.0],
        )?;

        // Copy files_ir
        tx.execute(
            "INSERT INTO files_ir (branch_id, file_path, language, content_hash, ir_data, updated_at)
             SELECT ?1, file_path, language, content_hash, ir_data, updated_at
             FROM files_ir WHERE branch_id = ?2",
            params![new_branch.0, source_branch.0],
        )?;

        tx.commit()?;

        Ok(())
    }

    fn switch_branch(&self, branch_id: &BranchId) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;

        let tx = conn.unchecked_transaction()?;

        // Make the branch known to the `branches` table so subsequent
        // `list_branches` / freshness queries can find it.
        tx.execute(
            "INSERT OR IGNORE INTO branches (branch_id) VALUES (?1)",
            params![branch_id.0],
        )?;

        tx.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![CURRENT_BRANCH_KEY, branch_id.0],
        )?;

        tx.commit()?;

        Ok(())
    }

    fn delete_branch(&self, branch_id: &BranchId) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;

        let tx = conn.unchecked_transaction()?;

        // Delete edges first (they reference nodes via FK)
        tx.execute(
            "DELETE FROM edges WHERE branch_id = ?1",
            params![branch_id.0],
        )?;

        tx.execute(
            "DELETE FROM nodes WHERE branch_id = ?1",
            params![branch_id.0],
        )?;

        tx.execute(
            "DELETE FROM files_ir WHERE branch_id = ?1",
            params![branch_id.0],
        )?;

        // Drop the registry row last so failures above don't orphan the
        // branch metadata.
        tx.execute(
            "DELETE FROM branches WHERE branch_id = ?1",
            params![branch_id.0],
        )?;

        tx.commit()?;

        Ok(())
    }

    fn list_branches(&self) -> Result<Vec<BranchId>, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let mut stmt = conn.prepare("SELECT branch_id FROM branches ORDER BY branch_id")?;

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            Ok(BranchId(id))
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn get_current_branch(&self) -> Result<BranchId, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let result: Result<String, _> = conn.query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            params![CURRENT_BRANCH_KEY],
            |row| row.get(0),
        );

        match result {
            Ok(branch) => Ok(BranchId(branch)),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                tracing::debug!("No current_branch in metadata, defaulting to 'main'");
                Ok(BranchId(DEFAULT_BRANCH.to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn get_last_scanned_commit(
        &self,
        branch_id: &BranchId,
    ) -> Result<Option<String>, StorageError> {
        let conn = lock_conn(&self.conn)?;

        let result: Result<Option<String>, _> = conn.query_row(
            "SELECT last_scanned_commit FROM branches WHERE branch_id = ?1",
            params![branch_id.0],
            |row| row.get(0),
        );

        match result {
            Ok(commit) => Ok(commit),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set_last_scanned_commit(
        &self,
        branch_id: &BranchId,
        commit: &str,
    ) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;

        conn.execute(
            "INSERT INTO branches (branch_id, last_scanned_commit, last_scanned_at)
             VALUES (?1, ?2, unixepoch())
             ON CONFLICT(branch_id) DO UPDATE SET
                 last_scanned_commit = excluded.last_scanned_commit,
                 last_scanned_at     = excluded.last_scanned_at",
            params![branch_id.0, commit],
        )?;

        Ok(())
    }

    fn ensure_branch_exists(&self, branch_id: &BranchId) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;

        conn.execute(
            "INSERT OR IGNORE INTO branches (branch_id) VALUES (?1)",
            params![branch_id.0],
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use crate::repository::file_ir_repository::SqliteFileIRRepository;
    use crate::repository::node_repository::SqliteNodeRepository;
    use crate::repository::{FileIRRepository, NodeRepository};
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
        file_repo.upsert(&main_branch, &file, None).unwrap();

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

        // Branches must be explicitly registered now — `list_branches`
        // reads from the `branches` table, not from `nodes` / `files_ir`.
        branch_repo.ensure_branch_exists(&main_branch).unwrap();
        branch_repo.ensure_branch_exists(&feature).unwrap();

        // Insert data afterwards so the rest of the assertions on
        // node/file presence still exercise the snapshot/list interplay.
        let mut n = make_knowledge_node(KnowledgeNature::Fact, 0.5);
        n.branch_id = main_branch.clone();
        node_repo.insert(&n).unwrap();

        let mut f = make_project_file(Language::Python);
        f.path = "app.py".into();
        f.content_hash = "h".to_string();
        file_repo.upsert(&feature, &f, None).unwrap();

        let branches = branch_repo.list_branches().unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches.contains(&main_branch));
        assert!(branches.contains(&feature));
    }

    /// Regression guard for US-003: `list_branches` must not fall back to
    /// `SELECT DISTINCT branch_id FROM nodes` (the old behaviour). A branch
    /// with raw `nodes`/`edges`/`files_ir` rows but no entry in `branches`
    /// must NOT appear in the listing — we want explicit registration.
    #[test]
    fn list_branches_reads_from_branches_table_not_nodes() {
        let (branch_repo, node_repo, file_repo) = test_repos();
        let ghost = BranchId::from("ghost-branch");

        // Insert raw rows for a branch that was never registered. With the
        // old `UNION` query this branch would be returned by `list_branches`.
        let mut n = make_knowledge_node(KnowledgeNature::Fact, 0.4);
        n.branch_id = ghost.clone();
        node_repo.insert(&n).unwrap();

        let mut f = make_project_file(Language::Rust);
        f.path = "ghost.rs".into();
        f.content_hash = "ghost_hash".to_string();
        file_repo.upsert(&ghost, &f, None).unwrap();

        let branches = branch_repo.list_branches().unwrap();
        assert!(
            branches.is_empty(),
            "list_branches should ignore raw rows in nodes/files_ir, got {branches:?}"
        );
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
        file_repo.upsert(&branch, &f, None).unwrap();

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
        file_repo.upsert(&main_branch, &f, None).unwrap();

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

    // ── US-003: BranchRepository extensions ────────────────────────────

    #[test]
    fn ensure_branch_exists_is_idempotent() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("idem");

        branch_repo.ensure_branch_exists(&b).unwrap();
        branch_repo.ensure_branch_exists(&b).unwrap();
        branch_repo.ensure_branch_exists(&b).unwrap();

        let branches = branch_repo.list_branches().unwrap();
        assert_eq!(branches, vec![b]);
    }

    #[test]
    fn ensure_branch_exists_does_not_overwrite_existing_metadata() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("preserve-me");

        branch_repo.set_last_scanned_commit(&b, "abc1234").unwrap();
        // Calling ensure_branch_exists must not clobber `last_scanned_commit`.
        branch_repo.ensure_branch_exists(&b).unwrap();

        let commit = branch_repo.get_last_scanned_commit(&b).unwrap();
        assert_eq!(commit.as_deref(), Some("abc1234"));
    }

    #[test]
    fn get_last_scanned_commit_returns_none_for_unknown_branch() {
        let (branch_repo, _, _) = test_repos();
        let result = branch_repo
            .get_last_scanned_commit(&BranchId::from("never-scanned"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_last_scanned_commit_returns_none_when_branch_exists_but_not_scanned() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("registered-only");

        // Branch exists in the registry but never scanned — column is NULL.
        branch_repo.ensure_branch_exists(&b).unwrap();

        let result = branch_repo.get_last_scanned_commit(&b).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_last_scanned_commit_round_trip() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("round-trip");

        branch_repo.set_last_scanned_commit(&b, "deadbeef").unwrap();
        let read = branch_repo.get_last_scanned_commit(&b).unwrap();
        assert_eq!(read.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn set_last_scanned_commit_upsert_overwrites_previous_value() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("overwrite-me");

        branch_repo.set_last_scanned_commit(&b, "first00").unwrap();
        branch_repo.set_last_scanned_commit(&b, "secondf0").unwrap();

        let read = branch_repo.get_last_scanned_commit(&b).unwrap();
        assert_eq!(read.as_deref(), Some("secondf0"));

        // Still exactly one row in `branches` for this id.
        let branches = branch_repo.list_branches().unwrap();
        assert_eq!(
            branches.iter().filter(|x| **x == b).count(),
            1,
            "UPSERT must not duplicate rows"
        );
    }

    #[test]
    fn set_last_scanned_commit_bumps_last_scanned_at() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("bump");

        branch_repo.set_last_scanned_commit(&b, "h1").unwrap();
        // Read the timestamp directly.
        let conn = branch_repo.conn.lock().unwrap();
        let ts1: i64 = conn
            .query_row(
                "SELECT last_scanned_at FROM branches WHERE branch_id = ?1",
                params![b.0],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);

        // Sleep at least one whole second so unixepoch() ticks forward
        // (resolution is per-second, not per-microsecond).
        std::thread::sleep(std::time::Duration::from_millis(1100));

        branch_repo.set_last_scanned_commit(&b, "h2").unwrap();
        let conn = branch_repo.conn.lock().unwrap();
        let ts2: i64 = conn
            .query_row(
                "SELECT last_scanned_at FROM branches WHERE branch_id = ?1",
                params![b.0],
                |row| row.get(0),
            )
            .unwrap();
        assert!(ts2 >= ts1, "last_scanned_at must monotonically advance");
    }

    #[test]
    fn create_snapshot_registers_target_branch_with_snapshot_source() {
        let (branch_repo, _, _) = test_repos();
        let main_branch = BranchId::from("main");
        let snap = BranchId::from("snap-1");

        // Source isn't pre-registered — create_snapshot must register both.
        branch_repo.create_snapshot(&main_branch, &snap).unwrap();

        let listed = branch_repo.list_branches().unwrap();
        assert!(listed.contains(&main_branch), "source must be registered");
        assert!(listed.contains(&snap), "target must be registered");

        // `snapshot_source` must be set on the target row.
        let conn = branch_repo.conn.lock().unwrap();
        let source: Option<String> = conn
            .query_row(
                "SELECT snapshot_source FROM branches WHERE branch_id = ?1",
                params![snap.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source.as_deref(), Some("main"));
    }

    #[test]
    fn delete_branch_removes_branches_row() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("doomed");

        branch_repo.set_last_scanned_commit(&b, "abc").unwrap();
        assert!(branch_repo.list_branches().unwrap().contains(&b));

        branch_repo.delete_branch(&b).unwrap();
        assert!(
            !branch_repo.list_branches().unwrap().contains(&b),
            "delete_branch must drop the registry row"
        );
    }

    #[test]
    fn switch_branch_registers_branch_implicitly() {
        let (branch_repo, _, _) = test_repos();
        let b = BranchId::from("switched-only");

        // No prior ensure / set / snapshot — the act of switching must
        // be enough to surface the branch in `list_branches`.
        branch_repo.switch_branch(&b).unwrap();

        assert!(branch_repo.list_branches().unwrap().contains(&b));
    }
}
