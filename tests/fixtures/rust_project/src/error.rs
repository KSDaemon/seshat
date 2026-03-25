// Demonstrates:
// - Centralized thiserror error type
// - #[from] for error conversion
// - Nested error wrapping
// - Display implementations

use thiserror::Error;

use crate::services::notification_service::NotificationError;
use crate::services::user_service::UserServiceError;

/// Top-level application error type.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("User error: {0}")]
    User(#[from] UserServiceError),

    #[error("Notification error: {0}")]
    Notification(#[from] NotificationError),

    #[error("Configuration error: {details}")]
    Config { details: String },

    #[error("Internal error: {0}")]
    Internal(String),
}

impl AppError {
    /// Creates a configuration error.
    pub fn config(details: impl Into<String>) -> Self {
        Self::Config {
            details: details.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UserId;

    #[test]
    fn test_from_user_error() {
        let user_err = UserServiceError::NotFound(UserId(1));
        let app_err: AppError = user_err.into();
        assert!(matches!(app_err, AppError::User(_)));
    }

    #[test]
    fn test_config_error() {
        let err = AppError::config("Missing field");
        assert!(err.to_string().contains("Configuration error"));
    }
}
