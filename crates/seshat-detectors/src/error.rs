/// Errors originating from convention detection.
#[derive(Debug, thiserror::Error)]
pub enum DetectorError {
    /// A detector failed to analyze a file.
    #[error("Detector '{detector}' failed on {file}: {reason}")]
    DetectionFailed {
        detector: String,
        file: String,
        reason: String,
    },

    /// Confidence calculation error.
    #[error("Confidence calculation error: {0}")]
    ConfidenceError(String),
}
