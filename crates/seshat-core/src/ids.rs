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
