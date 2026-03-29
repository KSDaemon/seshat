//! Repository traits and SQLite implementations for Seshat's knowledge graph.
//!
//! Each trait defines the persistence API for a single entity type. The SQLite
//! implementations operate on the shared `Database` handle.

mod branch_repository;
mod edge_repository;
mod file_ir_repository;
mod node_repository;

pub use branch_repository::SqliteBranchRepository;
pub use edge_repository::SqliteEdgeRepository;
pub use file_ir_repository::SqliteFileIRRepository;
pub use node_repository::SqliteNodeRepository;

use std::collections::HashMap;

use crate::StorageError;
use seshat_core::{
    BranchId, Edge, EdgeId, EdgeType, KnowledgeNature, KnowledgeNode, NodeId, ProjectFile,
};

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
    fn upsert(&self, branch_id: &BranchId, file: &ProjectFile) -> Result<(), StorageError>;

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
