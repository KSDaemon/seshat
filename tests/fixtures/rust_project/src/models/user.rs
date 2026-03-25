// Demonstrates:
// - Derive macros (Debug, Clone, Serialize, Deserialize)
// - Newtype pattern for type-safe IDs
// - serde attributes
// - pub/private visibility
// - Type definitions (struct)

use serde::{Deserialize, Serialize};

/// A type-safe wrapper for user identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub i64);

/// Represents a user in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub email: String,
    display_name: Option<String>,
    pub is_active: bool,
}

impl User {
    /// Creates a new active user.
    pub fn new(id: UserId, username: String, email: String) -> Self {
        Self {
            id,
            username,
            email,
            display_name: None,
            is_active: true,
        }
    }

    /// Returns the display name, falling back to the username.
    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }

    /// Deactivates the user.
    fn deactivate(&mut self) {
        self.is_active = false;
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "user-{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_creation() {
        let user = User::new(UserId(1), "alice".into(), "alice@example.com".into());
        assert_eq!(user.id, UserId(1));
        assert!(user.is_active);
    }

    #[test]
    fn test_display_name_fallback() {
        let user = User::new(UserId(2), "bob".into(), "bob@example.com".into());
        assert_eq!(user.display_name(), "bob");
    }

    #[test]
    fn test_deactivate() {
        let mut user = User::new(UserId(3), "charlie".into(), "charlie@example.com".into());
        user.deactivate();
        assert!(!user.is_active);
    }

    #[test]
    fn test_user_id_display() {
        let id = UserId(42);
        assert_eq!(id.to_string(), "user-42");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let user = User::new(UserId(1), "alice".into(), "alice@example.com".into());
        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("camelCase").not() || json.contains("isActive"));
        let deserialized: User = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, user.id);
    }
}
