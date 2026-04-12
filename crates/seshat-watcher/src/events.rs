//! Bulk change detection with a sliding time-window counter.
//!
//! Tracks the number of distinct file-change events observed within a 2-second
//! window.  When the count exceeds `threshold`, the caller should abandon
//! per-file hot-tier processing and trigger a full incremental rescan instead.
//!
//! Also detects `.git/HEAD` changes, which indicate a branch switch.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

/// Sliding-window bulk-change detector.
///
/// Call [`observe`] for each file-change event. Call [`should_bulk_rescan`]
/// to learn whether the threshold has been exceeded.
pub struct BulkChangeDetector {
    /// Maximum number of events in the window before bulk mode is triggered.
    threshold: usize,
    /// Width of the sliding window.
    window: Duration,
    /// Timestamps of events currently inside the window.
    events: VecDeque<Instant>,
}

impl BulkChangeDetector {
    /// Create a new detector with the given threshold and a 2-second window.
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold,
            window: Duration::from_secs(2),
            events: VecDeque::new(),
        }
    }

    /// Record a new file-change event at the current time.
    pub fn observe(&mut self) {
        let now = Instant::now();
        self.evict_old(now);
        self.events.push_back(now);
    }

    /// Returns `true` when the number of recent events exceeds the threshold.
    pub fn should_bulk_rescan(&mut self) -> bool {
        self.evict_old(Instant::now());
        self.events.len() > self.threshold
    }

    /// Reset the window (call after a bulk rescan completes).
    pub fn reset(&mut self) {
        self.events.clear();
    }

    /// Drop events that have fallen outside the sliding window.
    fn evict_old(&mut self, now: Instant) {
        while let Some(&front) = self.events.front() {
            if now.duration_since(front) > self.window {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }
}

/// Returns `true` when the given path is `.git/HEAD`, indicating a branch
/// switch (or other ref-change).
pub fn is_git_head_change(path: &Path) -> bool {
    path.components().collect::<Vec<_>>().windows(2).any(|w| {
        let parent = w[0].as_os_str().to_string_lossy();
        let name = w[1].as_os_str().to_string_lossy();
        parent == ".git" && name == "HEAD"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn bulk_detector_below_threshold_is_fine() {
        let mut d = BulkChangeDetector::new(5);
        for _ in 0..5 {
            d.observe();
        }
        assert!(!d.should_bulk_rescan());
    }

    #[test]
    fn bulk_detector_above_threshold_triggers() {
        let mut d = BulkChangeDetector::new(5);
        for _ in 0..6 {
            d.observe();
        }
        assert!(d.should_bulk_rescan());
    }

    #[test]
    fn bulk_detector_reset_clears_window() {
        let mut d = BulkChangeDetector::new(2);
        for _ in 0..10 {
            d.observe();
        }
        assert!(d.should_bulk_rescan());
        d.reset();
        assert!(!d.should_bulk_rescan());
    }

    #[test]
    fn git_head_detected_in_nested_path() {
        let p = PathBuf::from("/home/user/project/.git/HEAD");
        assert!(is_git_head_change(&p));
    }

    #[test]
    fn git_head_not_triggered_for_regular_file() {
        let p = PathBuf::from("/home/user/project/src/main.rs");
        assert!(!is_git_head_change(&p));
    }

    #[test]
    fn git_head_not_triggered_for_git_other_file() {
        let p = PathBuf::from("/home/user/project/.git/COMMIT_EDITMSG");
        assert!(!is_git_head_change(&p));
    }
}
