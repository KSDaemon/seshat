/// Errors originating from file watching and incremental updates.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// Watcher is disabled via configuration (`[watcher] enabled = false`).
    #[error("File watcher is disabled in configuration")]
    Disabled,

    /// Failed to initialize the file watcher.
    #[error("Watcher initialization failed: {0}")]
    InitError(String),

    /// A file event could not be processed.
    #[error("Failed to process file event for {path}: {reason}")]
    EventProcessingError { path: String, reason: String },

    /// Branch detection failed.
    #[error("Branch detection failed: {0}")]
    BranchDetectionError(String),

    /// Auto-scan failed; watcher refuses to start because there is no
    /// usable indexed state to incrementally update.
    ///
    /// Returned through `start_watcher`'s oneshot channel when the spawned
    /// watcher task observes a failed scan_state after waiting for the
    /// auto-scan to complete. Prevents `notify-debouncer-full` from walking
    /// a project we already decided we can't index.
    #[error("Auto-scan failed; watcher not started: {0}")]
    ScanFailed(String),

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_failed_display_includes_reason() {
        let e = WatcherError::ScanFailed("project too large: 1234567 files".to_owned());
        let rendered = e.to_string();
        assert!(
            rendered.contains("project too large: 1234567 files"),
            "Display should propagate the reason: {rendered}",
        );
        assert!(
            rendered.contains("watcher not started"),
            "Display should explain the consequence: {rendered}",
        );
    }

    #[test]
    fn scan_failed_debug_format_does_not_panic() {
        let e = WatcherError::ScanFailed("scan timeout".to_owned());
        // Debug derives via thiserror; just confirm it formats without panicking
        // and includes the variant name so log lines remain greppable.
        let dbg = format!("{e:?}");
        assert!(dbg.contains("ScanFailed"), "Debug missing variant: {dbg}");
    }
}
