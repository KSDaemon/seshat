//! Repository traits and SQLite implementations for Seshat's knowledge graph.
//!
//! Each trait defines the persistence API for a single entity type. The SQLite
//! implementations operate on the shared `Database` handle.

mod branch_repository;
mod edge_repository;
pub mod embedding_repository;
mod file_ir_repository;
mod node_repository;
mod package_metadata_repository;
mod repo_metadata_repository;
mod submodule_repository;

pub use branch_repository::SqliteBranchRepository;
pub use edge_repository::SqliteEdgeRepository;
pub use embedding_repository::{
    EmbeddingInput, EmbeddingRow, SqliteEmbeddingRepository, bytes_to_f32s, f32s_to_bytes,
};
pub use file_ir_repository::SqliteFileIRRepository;
pub use node_repository::SqliteNodeRepository;
pub use package_metadata_repository::{PackageMetadataRow, SqlitePackageMetadataRepository};
pub use repo_metadata_repository::SqliteRepoMetadataRepository;
pub use submodule_repository::{SqliteSubmoduleRepository, SubmoduleInput, SubmoduleRow};

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

use crate::StorageError;
use seshat_core::{
    BranchId, Edge, EdgeId, EdgeType, KnowledgeNature, KnowledgeNode, NodeId, ProjectFile,
};

/// Acquire a lock on a shared `Connection`, mapping poisoned-mutex errors
/// to [`StorageError`].
///
/// All SQLite repository implementations use `Arc<Mutex<Connection>>`.
/// This helper eliminates the identical `conn()` method from each one.
pub(crate) fn lock_conn(
    conn: &Arc<Mutex<Connection>>,
) -> Result<MutexGuard<'_, Connection>, StorageError> {
    conn.lock()
        .map_err(|e| StorageError::QueryError(format!("Failed to acquire connection lock: {e}")))
}

/// Persistence operations for [`KnowledgeNode`]s.
pub trait NodeRepository {
    /// Insert a new node. Returns the node with its assigned ID.
    fn insert(&self, node: &KnowledgeNode) -> Result<KnowledgeNode, StorageError>;

    /// Get a node by its ID.
    fn get_by_id(&self, id: NodeId) -> Result<KnowledgeNode, StorageError>;

    /// Find all nodes with the given nature.
    fn find_by_nature(&self, nature: KnowledgeNature) -> Result<Vec<KnowledgeNode>, StorageError>;

    /// Find all nodes belonging to the given branch.
    fn find_by_branch(&self, branch_id: &BranchId) -> Result<Vec<KnowledgeNode>, StorageError>;

    /// Update an existing node. The node's `id` field identifies which row to update.
    fn update(&self, node: &KnowledgeNode) -> Result<(), StorageError>;

    /// Delete a node by its ID.
    fn delete(&self, id: NodeId) -> Result<(), StorageError>;

    /// Delete all nodes for the given branch. Returns the number of rows deleted.
    fn delete_by_branch(&self, branch_id: &BranchId) -> Result<usize, StorageError>;

    /// Delete only `fact` nodes for a branch (module structure, documentation).
    ///
    /// Preserves `convention`, `observation`, and user-recorded decision nodes.
    /// Use this instead of `delete_by_branch` when rebuilding module graphs
    /// to avoid wiping user-confirmed conventions.
    fn delete_facts_by_branch(&self, branch_id: &BranchId) -> Result<usize, StorageError>;

    /// Delete auto-detected convention nodes for a branch.
    ///
    /// Only removes nodes where `ext_data` contains `"source": "auto_detected"`.
    /// User-recorded decisions (`"source": "user"`) are preserved.
    /// Returns the number of rows deleted.
    fn delete_auto_detected_by_branch(&self, branch_id: &BranchId) -> Result<usize, StorageError>;

    /// Find all convention nodes for the given branch.
    ///
    /// Returns nodes where `ext_data` contains `"source": "auto_detected"` or
    /// `"source": "user"` (i.e., convention-related nodes, not module/doc facts).
    fn find_conventions_by_branch(
        &self,
        branch_id: &BranchId,
    ) -> Result<Vec<KnowledgeNode>, StorageError>;
}

/// Persistence operations for [`Edge`]s.
pub trait EdgeRepository {
    /// Insert a new edge. Returns the edge with its assigned ID.
    fn insert(&self, edge: &Edge) -> Result<Edge, StorageError>;

    /// Find all edges originating from the given source node.
    fn find_by_source(&self, source_id: NodeId) -> Result<Vec<Edge>, StorageError>;

    /// Find all edges targeting the given node.
    fn find_by_target(&self, target_id: NodeId) -> Result<Vec<Edge>, StorageError>;

    /// Find all edges of the given type.
    fn find_by_type(&self, edge_type: EdgeType) -> Result<Vec<Edge>, StorageError>;

    /// Delete an edge by its ID.
    fn delete(&self, id: EdgeId) -> Result<(), StorageError>;

    /// Delete all edges for the given branch. Returns the number of rows deleted.
    fn delete_by_branch(&self, branch_id: &BranchId) -> Result<usize, StorageError>;
}

/// Persistence operations for file IR records (parsed source file cache).
pub trait FileIRRepository {
    /// Insert or update a file IR record. Uses `(branch_id, file_path)` as the
    /// natural key — if a row already exists, it is replaced.
    ///
    /// `last_commit_date` is the Unix timestamp of the most recent git commit
    /// that touched this file (from `collect_git_file_dates`). `None` means
    /// the project is not a git repo or the file has no commit history.
    fn upsert(
        &self,
        branch_id: &BranchId,
        file: &ProjectFile,
        last_commit_date: Option<i64>,
    ) -> Result<(), StorageError>;

    /// Get the IR for a file by its path within a branch.
    fn get_by_path(
        &self,
        branch_id: &BranchId,
        file_path: &str,
    ) -> Result<ProjectFile, StorageError>;

    /// Get all file IR records for the given branch.
    fn get_by_branch(&self, branch_id: &BranchId) -> Result<Vec<ProjectFile>, StorageError>;

    /// Get all `(file_path, content_hash)` pairs for a branch.
    ///
    /// This is more efficient than [`get_by_branch`](Self::get_by_branch) when you only need
    /// path + hash for incremental comparison (avoids deserializing the full IR).
    fn get_file_hashes_by_branch(
        &self,
        branch_id: &BranchId,
    ) -> Result<HashMap<String, String>, StorageError>;

    /// Delete the IR record for a file path within a branch.
    fn delete_by_path(&self, branch_id: &BranchId, file_path: &str) -> Result<(), StorageError>;

    /// Check whether the stored content hash matches the given hash.
    /// Returns `true` if a record exists and the hash matches, `false` otherwise.
    fn check_content_hash(
        &self,
        branch_id: &BranchId,
        file_path: &str,
        content_hash: &str,
    ) -> Result<bool, StorageError>;

    /// Get all `(file_path, last_commit_date)` pairs for a branch.
    ///
    /// Returns a map of file paths to their most recent git commit timestamps.
    /// Files without a recorded date are included with `None`.
    fn get_file_dates_by_branch(
        &self,
        branch_id: &BranchId,
    ) -> Result<HashMap<String, Option<i64>>, StorageError>;

    /// Update `convention_compliance_count` for multiple files in a single
    /// transaction.
    ///
    /// `counts` maps `file_path` → compliance count (number of
    /// `follows_convention == true` findings for that file).
    fn update_convention_compliance_counts(
        &self,
        branch_id: &BranchId,
        counts: &HashMap<String, u32>,
    ) -> Result<(), StorageError>;
}

/// Persistence operations for branch management.
///
/// Branch snapshots work by copying all nodes, edges, and files_ir rows with a
/// new `branch_id`. The current branch is tracked in the `metadata` table.
pub trait BranchRepository {
    /// Create a snapshot of the source branch under a new branch name.
    /// Copies all nodes, edges, and files_ir rows in a single transaction.
    fn create_snapshot(
        &self,
        source_branch: &BranchId,
        new_branch: &BranchId,
    ) -> Result<(), StorageError>;

    /// Switch the current branch to the given branch.
    fn switch_branch(&self, branch_id: &BranchId) -> Result<(), StorageError>;

    /// Delete all data associated with the given branch.
    fn delete_branch(&self, branch_id: &BranchId) -> Result<(), StorageError>;

    /// List all distinct branch IDs present in the database.
    fn list_branches(&self) -> Result<Vec<BranchId>, StorageError>;

    /// Get the current branch. Returns the branch stored in the metadata table,
    /// or a default of `"main"` if no current branch has been set.
    fn get_current_branch(&self) -> Result<BranchId, StorageError>;
}

/// Persistence operations for package registry metadata cache.
///
/// Stores categories, keywords, and descriptions fetched from package registries
/// (crates.io, npm, PyPI) keyed by `(name, registry)`.
pub trait PackageMetadataRepository {
    /// Insert or update a package metadata row. Uses `(name, registry)` as the
    /// natural key — if a row already exists, it is replaced.
    fn upsert(&self, row: &PackageMetadataRow) -> Result<(), StorageError>;

    /// Get metadata for a package from a specific registry.
    /// Returns `None` if no cached entry exists.
    fn get(&self, name: &str, registry: &str) -> Result<Option<PackageMetadataRow>, StorageError>;

    /// Get all cached metadata entries for a specific registry.
    fn get_by_registry(&self, registry: &str) -> Result<Vec<PackageMetadataRow>, StorageError>;

    /// Delete entries with `fetched_at` older than the given Unix timestamp.
    /// Returns the number of rows deleted.
    fn delete_stale(&self, before_timestamp: i64) -> Result<usize, StorageError>;
}

/// Persistence operations for submodule records.
///
/// Tracks git submodules linked to a parent project, each with a dedicated DB.
pub trait SubmoduleRepository {
    /// Insert a new submodule record. Returns the full row (with generated `id`
    /// and timestamps).
    fn insert(&self, input: &SubmoduleInput) -> Result<SubmoduleRow, StorageError>;

    /// Update an existing submodule by its `relative_path`.
    fn update(&self, input: &SubmoduleInput) -> Result<(), StorageError>;

    /// Insert or update a submodule record atomically.
    ///
    /// Uses `INSERT ... ON CONFLICT(relative_path) DO UPDATE` so the caller
    /// doesn't need a separate try-update-then-insert pattern.
    fn upsert(&self, input: &SubmoduleInput) -> Result<(), StorageError>;

    /// Delete a submodule record by its `relative_path`.
    fn delete(&self, relative_path: &str) -> Result<(), StorageError>;

    /// List all submodules, sorted by `relative_path`.
    fn list(&self) -> Result<Vec<SubmoduleRow>, StorageError>;

    /// Find a submodule by its mount path relative to the repo root.
    /// Returns `None` if no record exists for this path.
    fn find_by_path(&self, relative_path: &str) -> Result<Option<SubmoduleRow>, StorageError>;
}

/// Persistence operations for code embedding vectors.
///
/// Stores per-item (function, type, export) embeddings generated during
/// `seshat scan` when an embedding provider is configured. When the
/// `[embedding]` config section is absent, this table remains empty.
pub trait EmbeddingRepository {
    /// Insert or update a single embedding.
    fn upsert(&self, branch_id: &str, input: &EmbeddingInput) -> Result<(), StorageError>;

    /// Insert or update a batch of embeddings in a single transaction.
    fn upsert_batch(&self, branch_id: &str, inputs: &[EmbeddingInput]) -> Result<(), StorageError>;

    /// Get all embeddings for a branch.
    fn get_by_branch(&self, branch_id: &str) -> Result<Vec<EmbeddingRow>, StorageError>;

    /// Get embeddings for a specific file within a branch.
    fn get_by_file(
        &self,
        branch_id: &str,
        file_path: &str,
    ) -> Result<Vec<EmbeddingRow>, StorageError>;

    /// Delete all embeddings for a specific file within a branch.
    /// Returns the number of rows deleted.
    fn delete_by_file(&self, branch_id: &str, file_path: &str) -> Result<usize, StorageError>;

    /// Delete all embeddings for a branch. Returns the number of rows deleted.
    fn delete_by_branch(&self, branch_id: &str) -> Result<usize, StorageError>;

    /// Count embeddings for a branch.
    fn count_by_branch(&self, branch_id: &str) -> Result<usize, StorageError>;

    /// Get all (file_path, item_name, item_kind) keys stored for a branch.
    fn get_stored_keys(
        &self,
        branch_id: &str,
    ) -> Result<Vec<(String, String, String)>, StorageError>;

    /// Delete embedding rows identified by the given composite keys.
    ///
    /// Deletes in batches of 100 per transaction. Returns total rows deleted.
    fn delete_stale(
        &self,
        branch_id: &str,
        stale_keys: &[(String, String, String)],
    ) -> Result<usize, StorageError>;
}

/// Persistence operations for repo-level key-value metadata.
///
/// Stores lightweight metadata like `project_name`, `last_scan_time`,
/// `file_count`, `convention_count`, etc.
pub trait RepoMetadataRepository {
    /// Get the value for a key. Returns `None` if the key does not exist.
    fn get(&self, key: &str) -> Result<Option<String>, StorageError>;

    /// Set a key-value pair. Overwrites if the key already exists.
    fn set(&self, key: &str, value: &str) -> Result<(), StorageError>;

    /// Get all key-value pairs, sorted by key.
    fn get_all(&self) -> Result<Vec<(String, String)>, StorageError>;
}
