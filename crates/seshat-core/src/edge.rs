use serde::{Deserialize, Serialize};

use crate::error::ParseEnumError;
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

impl EdgeType {
    /// Return the canonical snake_case representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RelatedTo => "related_to",
            Self::Updates => "updates",
            Self::Contradicts => "contradicts",
            Self::PartOf => "part_of",
            Self::DependsOn => "depends_on",
            Self::Implements => "implements",
        }
    }
}

impl std::fmt::Display for EdgeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RelatedTo => write!(f, "RelatedTo"),
            Self::Updates => write!(f, "Updates"),
            Self::Contradicts => write!(f, "Contradicts"),
            Self::PartOf => write!(f, "PartOf"),
            Self::DependsOn => write!(f, "DependsOn"),
            Self::Implements => write!(f, "Implements"),
        }
    }
}

impl std::str::FromStr for EdgeType {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "related_to" => Ok(Self::RelatedTo),
            "updates" => Ok(Self::Updates),
            "contradicts" => Ok(Self::Contradicts),
            "part_of" => Ok(Self::PartOf),
            "depends_on" => Ok(Self::DependsOn),
            "implements" => Ok(Self::Implements),
            _ => Err(ParseEnumError {
                type_name: "EdgeType",
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{BranchId, EdgeId, NodeId};

    #[test]
    fn edge_serialization_roundtrip() {
        let edge = Edge {
            id: EdgeId(1),
            source_id: NodeId(10),
            target_id: NodeId(20),
            edge_type: EdgeType::DependsOn,
            branch_id: BranchId::from("main"),
            weight: 1.0,
            metadata: None,
        };

        let json = serde_json::to_string(&edge).expect("serialize");
        assert!(
            !json.contains("metadata"),
            "None metadata should be skipped"
        );

        let deserialized: Edge = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.edge_type, EdgeType::DependsOn);
        assert_eq!(deserialized.source_id, NodeId(10));
        assert_eq!(deserialized.target_id, NodeId(20));
    }

    #[test]
    fn all_edge_type_variants() {
        let types = [
            EdgeType::RelatedTo,
            EdgeType::Updates,
            EdgeType::Contradicts,
            EdgeType::PartOf,
            EdgeType::DependsOn,
            EdgeType::Implements,
        ];
        assert_eq!(types.len(), 6);
    }

    #[test]
    fn edge_type_display() {
        assert_eq!(EdgeType::DependsOn.to_string(), "DependsOn");
        assert_eq!(EdgeType::Contradicts.to_string(), "Contradicts");
    }

    #[test]
    fn edge_type_roundtrip_str() {
        let types = [
            EdgeType::RelatedTo,
            EdgeType::Updates,
            EdgeType::Contradicts,
            EdgeType::PartOf,
            EdgeType::DependsOn,
            EdgeType::Implements,
        ];
        for et in types {
            let s = et.as_str();
            let parsed: EdgeType = s.parse().unwrap();
            assert_eq!(parsed, et);
        }
    }

    #[test]
    fn edge_type_parse_unknown() {
        assert!("bogus".parse::<EdgeType>().is_err());
    }
}
