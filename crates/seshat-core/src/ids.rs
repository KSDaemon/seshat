use serde::{Deserialize, Serialize};
use std::fmt;

/// Type-safe wrapper for knowledge node identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub i64);

/// Type-safe wrapper for edge identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeId(pub i64);

/// Type-safe wrapper for branch identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BranchId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EdgeId({})", self.0)
    }
}

impl fmt::Display for BranchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<i64> for NodeId {
    fn from(id: i64) -> Self {
        Self(id)
    }
}

impl From<i64> for EdgeId {
    fn from(id: i64) -> Self {
        Self(id)
    }
}

impl From<String> for BranchId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for BranchId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_equality() {
        assert_eq!(NodeId(1), NodeId(1));
        assert_ne!(NodeId(1), NodeId(2));
    }

    #[test]
    fn edge_id_equality() {
        assert_eq!(EdgeId(1), EdgeId(1));
        assert_ne!(EdgeId(1), EdgeId(2));
    }

    #[test]
    fn branch_id_from_str() {
        let b = BranchId::from("main");
        assert_eq!(b.0, "main");
    }

    #[test]
    fn branch_id_from_string() {
        let b = BranchId::from("feature/foo".to_owned());
        assert_eq!(b.0, "feature/foo");
    }

    #[test]
    fn id_display() {
        assert_eq!(NodeId(42).to_string(), "NodeId(42)");
        assert_eq!(EdgeId(7).to_string(), "EdgeId(7)");
        assert_eq!(BranchId::from("main").to_string(), "main");
    }

    #[test]
    fn id_from_i64() {
        let n: NodeId = 5i64.into();
        assert_eq!(n, NodeId(5));
        let e: EdgeId = 10i64.into();
        assert_eq!(e, EdgeId(10));
    }
}
