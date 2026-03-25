//! Repository traits and SQLite implementations for Seshat's knowledge graph.
//!
//! Each trait defines the persistence API for a single entity type. The SQLite
//! implementations operate on the shared `Database` handle.

mod edge_repository;
mod node_repository;

pub use edge_repository::SqliteEdgeRepository;
pub use node_repository::SqliteNodeRepository;

use crate::StorageError;
use seshat_core::{BranchId, Edge, EdgeId, EdgeType, KnowledgeNature, KnowledgeNode, NodeId};

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
}
