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
/// Call [`BulkChangeDetector::observe`] for each file-change event. Call
/// [`BulkChangeDetector::should_bulk_rescan`] to learn whether the threshold
/// has been exceeded.
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
    ///
    /// `threshold` is clamped to a minimum of 1 — a value of 0 would trigger
    /// bulk-rescan on every single event, causing an infinite rescan storm.
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold: threshold.max(1),
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

/// Returns `true` when the given path is the `.git/HEAD` file, indicating a
/// branch switch (or other ref-change).
///
/// Requires both:
/// - The parent component is literally `.git` and the filename is `HEAD`.
/// - The path refers to a regular file (not a directory named `HEAD`).
pub fn is_git_head_change(path: &Path) -> bool {
    let component_match = path.components().collect::<Vec<_>>().windows(2).any(|w| {
        let parent = w[0].as_os_str().to_string_lossy();
        let name = w[1].as_os_str().to_string_lossy();
        parent == ".git" && name == "HEAD"
    });
    // Guard against a directory named HEAD inside .git (non-standard but possible).
    component_match && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn bulk_detector_threshold_zero_clamped_to_one() {
        let mut d = BulkChangeDetector::new(0);
        // With threshold clamped to 1, one event is NOT enough (len > threshold = 1 > 1 is false).
        d.observe();
        assert!(
            !d.should_bulk_rescan(),
            "one event should not trigger with threshold clamped to 1"
        );
        // Two events should trigger.
        d.observe();
        assert!(d.should_bulk_rescan());
    }

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
    fn git_head_detected_for_real_file() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        let head = git_dir.join("HEAD");
        fs::write(&head, "ref: refs/heads/main\n").unwrap();
        assert!(is_git_head_change(&head));
    }

    #[test]
    fn git_head_not_triggered_for_regular_file() {
        // Non-existent path — component check fails before is_file().
        let p = PathBuf::from("/home/user/project/src/main.rs");
        assert!(!is_git_head_change(&p));
    }

    #[test]
    fn git_head_not_triggered_for_git_other_file() {
        // Wrong filename — component check fails.
        let p = PathBuf::from("/home/user/project/.git/COMMIT_EDITMSG");
        assert!(!is_git_head_change(&p));
    }

    #[test]
    fn git_head_not_triggered_for_directory_named_head() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        let head_dir = git_dir.join("HEAD");
        fs::create_dir_all(&head_dir).unwrap();
        // Path has .git/HEAD components but HEAD is a directory — should return false.
        assert!(!is_git_head_change(&head_dir));
    }
}
