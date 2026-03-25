// Demonstrates:
// - thiserror error types
// - tracing logging (info, warn, error, instrument)
// - Grouped imports (std, external, local)
// - Result type alias
// - Async functions
// - pub/private visibility

use std::sync::Arc;

use thiserror::Error;
use tracing::{info, instrument, warn};

use crate::models::{AppState, User, UserId};

/// Errors that can occur in the user service.
#[derive(Debug, Error)]
pub enum UserServiceError {
    #[error("User not found: {0}")]
    NotFound(UserId),

    #[error("User already exists: {0}")]
    AlreadyExists(UserId),

    #[error("Validation failed: {reason}")]
    ValidationFailed { reason: String },
}

/// Type alias for user service results.
pub type UserResult<T> = Result<T, UserServiceError>;

/// Creates a new user in the store.
#[instrument(skip(state), fields(user_id = %id))]
pub async fn create_user(
    state: Arc<AppState>,
    id: UserId,
    username: String,
    email: String,
) -> UserResult<User> {
    validate_email(&email)?;

    let user = User::new(id, username, email);
    let mut users = state.users.lock().unwrap();

    if users.contains_key(&id) {
        warn!("Attempted to create duplicate user");
        return Err(UserServiceError::AlreadyExists(id));
    }

    info!("User created successfully");
    users.insert(id, user.clone());
    Ok(user)
}

/// Retrieves a user by ID.
#[instrument(skip(state))]
pub async fn get_user(state: Arc<AppState>, id: UserId) -> UserResult<User> {
    let users = state.users.lock().unwrap();
    users
        .get(&id)
        .cloned()
        .ok_or(UserServiceError::NotFound(id))
}

/// Lists all active users.
pub fn list_active_users(state: &AppState) -> Vec<User> {
    let users = state.users.lock().unwrap();
    users.values().filter(|u| u.is_active).cloned().collect()
}

/// Validates an email address (simple check).
fn validate_email(email: &str) -> UserResult<()> {
    if email.contains('@') && email.contains('.') {
        Ok(())
    } else {
        Err(UserServiceError::ValidationFailed {
            reason: format!("Invalid email format: {email}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::default())
    }

    #[tokio::test]
    async fn test_create_user() {
        let state = test_state();
        let user = create_user(state, UserId(1), "alice".into(), "alice@example.com".into())
            .await
            .unwrap();
        assert_eq!(user.id, UserId(1));
    }

    #[tokio::test]
    async fn test_create_duplicate_user() {
        let state = test_state();
        create_user(
            state.clone(),
            UserId(1),
            "alice".into(),
            "alice@example.com".into(),
        )
        .await
        .unwrap();

        let result = create_user(state, UserId(1), "bob".into(), "bob@example.com".into()).await;
        assert!(matches!(result, Err(UserServiceError::AlreadyExists(_))));
    }

    #[tokio::test]
    async fn test_get_user_not_found() {
        let state = test_state();
        let result = get_user(state, UserId(999)).await;
        assert!(matches!(result, Err(UserServiceError::NotFound(_))));
    }

    #[test]
    fn test_validate_email() {
        assert!(validate_email("good@example.com").is_ok());
        assert!(validate_email("bad-email").is_err());
    }

    #[test]
    fn test_list_active_users() {
        let state = AppState::default();
        let user = User::new(UserId(1), "alice".into(), "alice@example.com".into());
        state.users.lock().unwrap().insert(UserId(1), user);

        let active = list_active_users(&state);
        assert_eq!(active.len(), 1);
    }
}
