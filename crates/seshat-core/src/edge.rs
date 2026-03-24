use serde::{Deserialize, Serialize};

use crate::ids::{BranchId, EdgeId, NodeId};

/// A typed edge between two knowledge nodes in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Edge {
    pub id: EdgeId,
    pub source_id: NodeId,
    pub target_id: NodeId,
    pub edge_type: EdgeType,
    pub branch_id: BranchId,
    pub weight: f64,
    /// JSON-encoded edge-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// The type of relationship an edge represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    /// General topical relationship.
    RelatedTo,
    /// Source updates/supersedes target.
    Updates,
    /// Source contradicts target (code vs documentation).
    Contradicts,
    /// Source is a component of target.
    PartOf,
    /// Source depends on target.
    DependsOn,
    /// Source implements target (e.g., function implements interface).
    Implements,
}
