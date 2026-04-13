//! Source snippet extraction utilities.
//!
//! Provides [`extract_snippet`] for lifting real source code lines from a
//! file's content using 1-indexed line coordinates from the IR.

/// Extract lines [`line`..=`end_line`] from `source` (1-indexed, inclusive).
///
/// Returns up to `max_lines` lines joined by `"\n"`.
/// Gracefully handles all edge cases — never panics.
///
/// # Edge cases
///
/// - `line == 0` or `line > end_line` → returns `""`
/// - `max_lines == 0` → returns `""`
/// - `end_line` beyond EOF → clamped to last line
/// - `max_lines` shorter than the range → first `max_lines` lines returned
/// - `source` empty → returns `""`
pub fn extract_snippet(source: &str, line: usize, end_line: usize, max_lines: usize) -> String {
    if source.is_empty() || line == 0 || line > end_line || max_lines == 0 {
        return String::new();
    }
    let start = line - 1; // convert to 0-indexed
    let lines: Vec<&str> = source.lines().collect();
    let end_clamped = end_line.min(lines.len()); // clamp to actual file length
    if start >= end_clamped {
        return String::new();
    }
    let take = (end_clamped - start).min(max_lines);
    lines[start..start + take].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn normal_range() {
        let src = make_source(10);
        let result = extract_snippet(&src, 3, 5, 10);
        assert_eq!(result, "line 3\nline 4\nline 5");
    }

    #[test]
    fn line_zero_returns_empty() {
        let src = make_source(10);
        assert_eq!(extract_snippet(&src, 0, 0, 10), "");
    }

    #[test]
    fn end_beyond_eof_clamped() {
        let src = make_source(5);
        let result = extract_snippet(&src, 3, 999, 10);
        assert_eq!(result, "line 3\nline 4\nline 5");
    }

    #[test]
    fn single_line() {
        let src = make_source(5);
        let result = extract_snippet(&src, 2, 2, 10);
        assert_eq!(result, "line 2");
    }

    #[test]
    fn empty_source_returns_empty() {
        assert_eq!(extract_snippet("", 1, 1, 10), "");
    }

    #[test]
    fn line_greater_than_end_line_returns_empty() {
        let src = make_source(10);
        assert_eq!(extract_snippet(&src, 5, 3, 10), "");
    }

    #[test]
    fn max_lines_truncation() {
        let src = make_source(20);
        let result = extract_snippet(&src, 1, 20, 5);
        let expected = "line 1\nline 2\nline 3\nline 4\nline 5";
        assert_eq!(result, expected);
    }

    #[test]
    fn utf8_multibyte_no_panic() {
        let src = "héllo wörld\nвторой ряд\n";
        let result = extract_snippet(src, 1, 2, 10);
        assert_eq!(result, "héllo wörld\nвторой ряд");
    }

    #[test]
    fn line_start_equals_clamped_end_returns_empty() {
        // 5-line source, requesting line 6..8 — start >= end_clamped
        let src = make_source(5);
        assert_eq!(extract_snippet(&src, 6, 8, 10), "");
    }

    #[test]
    fn first_line() {
        let src = make_source(5);
        assert_eq!(extract_snippet(&src, 1, 1, 10), "line 1");
    }

    #[test]
    fn last_line() {
        let src = make_source(5);
        assert_eq!(extract_snippet(&src, 5, 5, 10), "line 5");
    }

    #[test]
    fn max_lines_zero_returns_empty() {
        let src = make_source(5);
        assert_eq!(extract_snippet(&src, 1, 5, 0), "");
    }
}
