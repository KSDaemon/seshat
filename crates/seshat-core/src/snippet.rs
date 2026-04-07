//! Code snippet truncation utilities.
//!
//! Shared by `seshat-graph` (convention evidence) and `seshat-mcp` (response
//! envelopes) to avoid duplicating the struct and truncation logic.

use serde::{Deserialize, Serialize};

/// Maximum number of lines in a code snippet before truncation.
pub const MAX_SNIPPET_LINES: usize = 20;

/// A code snippet that may be truncated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeSnippet {
    /// The (possibly truncated) snippet content.
    pub content: String,
    /// `true` when the original snippet exceeded [`MAX_SNIPPET_LINES`].
    pub truncated: bool,
}

/// Truncate a code snippet to at most [`MAX_SNIPPET_LINES`] lines.
///
/// Returns a [`CodeSnippet`] with `truncated: true` when lines were removed.
pub fn truncate_snippet(raw: &str) -> CodeSnippet {
    truncate_snippet_to(raw, MAX_SNIPPET_LINES)
}

/// Truncate a code snippet to at most `max_lines` lines.
///
/// Use this when a context-specific limit differs from [`MAX_SNIPPET_LINES`].
pub fn truncate_snippet_to(raw: &str, max_lines: usize) -> CodeSnippet {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() > max_lines {
        CodeSnippet {
            content: lines[..max_lines].join("\n"),
            truncated: true,
        }
    } else {
        CodeSnippet {
            content: raw.to_owned(),
            truncated: false,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_content_is_not_truncated() {
        let snippet = truncate_snippet("line 1\nline 2\nline 3");
        assert!(!snippet.truncated);
        assert_eq!(snippet.content, "line 1\nline 2\nline 3");
    }

    #[test]
    fn exact_limit_is_not_truncated() {
        let lines: Vec<String> = (1..=MAX_SNIPPET_LINES)
            .map(|i| format!("line {i}"))
            .collect();
        let raw = lines.join("\n");
        let result = truncate_snippet(&raw);
        assert!(!result.truncated);
        assert_eq!(result.content, raw);
    }

    #[test]
    fn over_limit_is_truncated() {
        let lines: Vec<String> = (1..=25).map(|i| format!("line {i}")).collect();
        let raw = lines.join("\n");
        let result = truncate_snippet(&raw);
        assert!(result.truncated);
        let result_lines: Vec<&str> = result.content.lines().collect();
        assert_eq!(result_lines.len(), MAX_SNIPPET_LINES);
        assert_eq!(result_lines[0], "line 1");
        assert_eq!(
            result_lines[MAX_SNIPPET_LINES - 1],
            format!("line {MAX_SNIPPET_LINES}")
        );
    }
}
