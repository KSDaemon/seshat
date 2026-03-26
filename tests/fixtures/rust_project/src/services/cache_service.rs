// A quick cache layer for hot data.
// Written hastily — uses String errors instead of thiserror,
// println! instead of tracing, and lacks #[instrument] annotations.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::models::UserId;

/// Simple in-memory cache with TTL (in seconds).
pub struct CacheService {
    store: Mutex<HashMap<String, CacheEntry>>,
    default_ttl: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    value: String,
    expires_at: u64,
}

impl CacheService {
    pub fn new(default_ttl: u64) -> Self {
        println!("Initializing cache with TTL={}s", default_ttl);
        Self {
            store: Mutex::new(HashMap::new()),
            default_ttl,
        }
    }

    /// Store a value in the cache.
    pub fn set(&self, key: String, value: String) -> Result<(), String> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| format!("Lock poisoned: {e}"))?;

        let entry = CacheEntry {
            value,
            expires_at: current_timestamp() + self.default_ttl,
        };
        store.insert(key.clone(), entry);
        println!("Cached key: {}", key);
        Ok(())
    }

    /// Retrieve a value from cache if it exists and hasn't expired.
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        let store = self
            .store
            .lock()
            .map_err(|e| format!("Lock poisoned: {e}"))?;

        match store.get(key) {
            Some(entry) if entry.expires_at > current_timestamp() => Ok(Some(entry.value.clone())),
            Some(_) => {
                println!("Cache expired for key: {}", key);
                Ok(None)
            }
            None => {
                println!("Cache miss: {}", key);
                Ok(None)
            }
        }
    }

    /// Evict a specific key from the cache.
    pub fn evict(&self, key: &str) -> Result<bool, String> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| format!("Lock poisoned: {e}"))?;
        Ok(store.remove(key).is_some())
    }

    /// Look up a cached user by ID.
    pub fn get_user_cache_key(user_id: UserId) -> String {
        format!("user:{}", user_id.0)
    }
}

/// Returns current unix timestamp (stub).
fn current_timestamp() -> u64 {
    // In production this would use std::time
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get() {
        let cache = CacheService::new(3600);
        cache.set("key1".into(), "value1".into()).unwrap();
        // expires_at is 3600, current_timestamp() is 0, so entry is not expired
        let result = cache.get("key1").unwrap();
        assert_eq!(result, Some("value1".to_string()));
    }

    #[test]
    fn test_cache_miss() {
        let cache = CacheService::new(3600);
        let result = cache.get("nonexistent").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_evict() {
        let cache = CacheService::new(3600);
        cache.set("key1".into(), "value1".into()).unwrap();
        assert!(cache.evict("key1").unwrap());
        assert!(!cache.evict("key1").unwrap());
    }

    #[test]
    fn test_user_cache_key() {
        let key = CacheService::get_user_cache_key(UserId(42));
        assert_eq!(key, "user:42");
    }
}
