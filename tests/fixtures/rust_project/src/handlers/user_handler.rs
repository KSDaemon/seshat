// Demonstrates:
// - Grouped imports
// - Async handler pattern
// - Error propagation with ?
// - tracing instrumentation

use std::sync::Arc;

use serde::Serialize;
use tracing::instrument;

use crate::models::{AppState, UserId};
use crate::services::user_service::{self, UserServiceError};

/// Response envelope for user endpoints.
#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: i64,
    pub username: String,
    pub email: String,
}

/// Error response body.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: u16,
}

/// Handles GET /users/:id requests.
#[instrument(skip(state))]
pub async fn get_user_handler(
    state: Arc<AppState>,
    user_id: i64,
) -> Result<UserResponse, ErrorResponse> {
    let user = user_service::get_user(state, UserId(user_id))
        .await
        .map_err(|e| to_error_response(&e))?;

    Ok(UserResponse {
        id: user.id.0,
        username: user.username,
        email: user.email,
    })
}

fn to_error_response(err: &UserServiceError) -> ErrorResponse {
    match err {
        UserServiceError::NotFound(_) => ErrorResponse {
            error: err.to_string(),
            code: 404,
        },
        UserServiceError::AlreadyExists(_) => ErrorResponse {
            error: err.to_string(),
            code: 409,
        },
        UserServiceError::ValidationFailed { .. } => ErrorResponse {
            error: err.to_string(),
            code: 400,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::User;

    #[tokio::test]
    async fn test_get_user_not_found() {
        let state = Arc::new(AppState::default());
        let result = get_user_handler(state, 999).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, 404);
    }

    #[tokio::test]
    async fn test_get_user_success() {
        let state = Arc::new(AppState::default());
        let user = User::new(UserId(1), "alice".into(), "alice@example.com".into());
        state.users.lock().unwrap().insert(UserId(1), user);

        let result = get_user_handler(state, 1).await;
        assert!(result.is_ok());
        let resp = result.unwrap();
        assert_eq!(resp.username, "alice");
    }
}
