// Sample: import grouping and ordering conventions
// Expected detections: grouped imports (std, external, local), use statements

// Group 1: Standard library imports
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

// Group 2: External crate imports
use serde::{Deserialize, Serialize};
use tracing::info;

/// A simple registry demonstrating grouped imports.
#[derive(Debug, Default)]
pub struct Registry {
    entries: HashMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    pub value: String,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: String, value: String) {
        info!(key = %key, "Inserting registry entry");
        self.entries.insert(key.clone(), Entry { key, value });
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Display for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Registry({} entries)", self.entries.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_insert_and_get() {
        let mut reg = Registry::new();
        reg.insert("key1".into(), "value1".into());
        assert_eq!(reg.get("key1").unwrap().value, "value1");
    }

    #[test]
    fn test_registry_len() {
        let mut reg = Registry::new();
        assert!(reg.is_empty());
        reg.insert("k".into(), "v".into());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn test_registry_display() {
        let reg = Registry::new();
        assert_eq!(reg.to_string(), "Registry(0 entries)");
    }
}
