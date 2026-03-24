use serde::{Deserialize, Serialize};

use crate::ids::{BranchId, NodeId};

/// A node in the knowledge graph.
///
/// Each node has a two-dimensional type: `nature` (what kind of knowledge)
/// crossed with `weight` (how authoritative). Confidence is computed from
/// adoption metrics: `adoption_count / total_count`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KnowledgeNode {
    pub id: NodeId,
    pub branch_id: BranchId,
    pub nature: KnowledgeNature,
    pub weight: KnowledgeWeight,
    pub confidence: f64,
    pub adoption_count: u32,
    pub total_count: u32,
    pub description: String,
    /// JSON-encoded type-specific data (e.g., `reasoning` for Decision,
    /// `adoption_rate` for Convention).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext_data: Option<serde_json::Value>,
}

/// The nature of a knowledge node — what kind of knowledge it represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeNature {
    /// A verifiable fact about the codebase.
    Fact,
    /// A detected coding convention.
    Convention,
    /// A pattern observed in code without enough adoption to be a convention.
    Observation,
    /// An explicit architectural or design decision.
    Decision,
    /// A user-confirmed preference.
    Preference,
}

/// The weight (authoritativeness) of a knowledge node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeWeight {
    /// Must follow. Violation in `validate_approach` → `rules_violated`.
    Rule,
    /// Strongly recommended (confidence > 0.85).
    Strong,
    /// Moderately recommended (confidence 0.50–0.85).
    Moderate,
    /// Weakly recommended (confidence 0.20–0.50).
    Weak,
    /// Informational only (confidence < 0.20).
    Info,
}
