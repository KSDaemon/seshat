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

impl std::fmt::Display for KnowledgeNature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fact => write!(f, "Fact"),
            Self::Convention => write!(f, "Convention"),
            Self::Observation => write!(f, "Observation"),
            Self::Decision => write!(f, "Decision"),
            Self::Preference => write!(f, "Preference"),
        }
    }
}

impl std::fmt::Display for KnowledgeWeight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rule => write!(f, "Rule"),
            Self::Strong => write!(f, "Strong"),
            Self::Moderate => write!(f, "Moderate"),
            Self::Weak => write!(f, "Weak"),
            Self::Info => write!(f, "Info"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{BranchId, NodeId};

    #[test]
    fn knowledge_node_serialization_roundtrip() {
        let node = KnowledgeNode {
            id: NodeId(42),
            branch_id: BranchId::from("main"),
            nature: KnowledgeNature::Convention,
            weight: KnowledgeWeight::Strong,
            confidence: 0.92,
            adoption_count: 23,
            total_count: 25,
            description: "Use thiserror for error types".to_owned(),
            ext_data: None,
        };

        let json = serde_json::to_string(&node).expect("serialize");
        assert!(!json.contains("ext_data"), "None fields should be skipped");

        let deserialized: KnowledgeNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.id, node.id);
        assert_eq!(deserialized.nature, KnowledgeNature::Convention);
        assert_eq!(deserialized.weight, KnowledgeWeight::Strong);
        assert!((deserialized.confidence - 0.92).abs() < f64::EPSILON);
    }

    #[test]
    fn knowledge_node_with_ext_data() {
        let node = KnowledgeNode {
            id: NodeId(1),
            branch_id: BranchId::from("feature"),
            nature: KnowledgeNature::Decision,
            weight: KnowledgeWeight::Rule,
            confidence: 1.0,
            adoption_count: 1,
            total_count: 1,
            description: "Use SQLite for storage".to_owned(),
            ext_data: Some(serde_json::json!({"reasoning": "Embedded, no runtime deps"})),
        };

        let json = serde_json::to_string(&node).expect("serialize");
        assert!(json.contains("ext_data"));
        assert!(json.contains("reasoning"));
    }

    #[test]
    fn nature_and_weight_display() {
        assert_eq!(KnowledgeNature::Convention.to_string(), "Convention");
        assert_eq!(KnowledgeWeight::Strong.to_string(), "Strong");
    }

    #[test]
    fn all_nature_variants() {
        let natures = [
            KnowledgeNature::Fact,
            KnowledgeNature::Convention,
            KnowledgeNature::Observation,
            KnowledgeNature::Decision,
            KnowledgeNature::Preference,
        ];
        assert_eq!(natures.len(), 5);
    }

    #[test]
    fn all_weight_variants() {
        let weights = [
            KnowledgeWeight::Rule,
            KnowledgeWeight::Strong,
            KnowledgeWeight::Moderate,
            KnowledgeWeight::Weak,
            KnowledgeWeight::Info,
        ];
        assert_eq!(weights.len(), 5);
    }
}
