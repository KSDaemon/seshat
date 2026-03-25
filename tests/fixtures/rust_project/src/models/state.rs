// Demonstrates:
// - Derive macros (Debug, Default)
// - Grouped imports
// - Configuration struct with defaults

use std::collections::HashMap;
use std::sync::Mutex;

use super::user::{User, UserId};

/// Shared application state.
#[derive(Debug, Default)]
pub struct AppState {
    pub users: Mutex<HashMap<UserId, User>>,
    pub request_count: Mutex<u64>,
}

impl AppState {
    /// Increments the request counter and returns the new value.
    pub fn increment_requests(&self) -> u64 {
        let mut count = self.request_count.lock().unwrap();
        *count += 1;
        *count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_state() {
        let state = AppState::default();
        assert_eq!(*state.request_count.lock().unwrap(), 0);
    }

    #[test]
    fn test_increment_requests() {
        let state = AppState::default();
        assert_eq!(state.increment_requests(), 1);
        assert_eq!(state.increment_requests(), 2);
    }
}
