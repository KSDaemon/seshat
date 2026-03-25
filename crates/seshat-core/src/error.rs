/// Errors originating from core type operations.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// An invalid confidence value was provided.
    #[error("Invalid confidence value: {value} (must be between 0.0 and 1.0)")]
    InvalidConfidence { value: f64 },

    /// Serialization or deserialization failed.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// A required field was missing.
    #[error("Missing required field: {field}")]
    MissingField { field: String },
}

/// Error returned when parsing a string into an enum variant fails.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown {type_name}: {value:?}")]
pub struct ParseEnumError {
    pub type_name: &'static str,
    pub value: String,
}
