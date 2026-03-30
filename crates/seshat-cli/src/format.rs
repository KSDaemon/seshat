//! Shared output formatting utilities for CLI reports.
//!
//! All colored output flows through this module so that `NO_COLOR` support
//! and verbosity filtering are centralised in one place.
//!
//! ## Color Policy
//!
//! The [`NO_COLOR`](https://no-color.org/) environment variable disables all
//! color output when set (to any value). Check once at startup via
//! [`color_enabled()`] and pass the result through to the formatting helpers.
//!
//! ## Verbosity Levels
//!
//! Three levels control how much output the user sees:
//!
//! | Level     | Errors | Warnings | Summary | Findings | Verbose details |
//! |-----------|--------|----------|---------|----------|-----------------|
//! | `Quiet`   | yes    | no       | final   | no       | no              |
//! | `Default` | yes    | yes      | yes     | key      | no              |
//! | `Verbose` | yes    | yes      | yes     | all      | yes             |

use std::fmt::Write;

use owo_colors::OwoColorize;

// ── Color support ────────────────────────────────────────────────────

/// Returns `true` if colored output is enabled.
///
/// Color is disabled when the `NO_COLOR` environment variable is set
/// (to any value, including empty string).
pub fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

// ── Verbosity ────────────────────────────────────────────────────────

/// CLI output verbosity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// Errors + final summary line only.
    Quiet,
    /// Errors + warnings + summary + key findings (default).
    Default,
    /// Everything: skipped files, detector details, timing breakdown.
    Verbose,
}

impl Verbosity {
    /// Create from the `--verbose` / `--quiet` flags.
    ///
    /// If both are set, `--quiet` wins (principle of least output).
    pub fn from_flags(verbose: bool, quiet: bool) -> Self {
        if quiet {
            Self::Quiet
        } else if verbose {
            Self::Verbose
        } else {
            Self::Default
        }
    }

    /// Whether to show warnings (not shown in quiet mode).
    pub fn show_warnings(self) -> bool {
        self != Self::Quiet
    }

    /// Whether to show the main findings list (not shown in quiet mode).
    pub fn show_findings(self) -> bool {
        self != Self::Quiet
    }

    /// Whether to show verbose details (skipped files, timing, detector table).
    pub fn show_verbose(self) -> bool {
        self == Self::Verbose
    }
}

// ── Section header ───────────────────────────────────────────────────

/// Total width of the header line (including the title text).
const HEADER_WIDTH: usize = 60;

/// Format a section header using box-drawing characters.
///
/// Produces: `── Title ──────────────────────────────────────────`
/// padded to ~60 characters.
///
/// When `color` is `true`, the dashes are dimmed.
pub fn format_section_header(title: &str, color: bool) -> String {
    let prefix = "── ";
    let separator = " ";
    // Count display characters, not bytes (─ is 3 bytes in UTF-8).
    let used = prefix.chars().count() + title.chars().count() + separator.chars().count();
    let remaining = HEADER_WIDTH.saturating_sub(used);
    let dashes: String = "─".repeat(remaining);

    if color {
        format!(
            "{}{}{}{}",
            "── ".dimmed(),
            title.bold(),
            " ".dimmed(),
            dashes.dimmed()
        )
    } else {
        format!("{prefix}{title}{separator}{dashes}")
    }
}

// ── Bar chart ────────────────────────────────────────────────────────

/// Maximum width of the bar (in characters).
const BAR_WIDTH: usize = 20;

/// Format a horizontal bar chart entry.
///
/// Produces: `  ▓▓▓▓▓▓▓░░░░░░░░░░░░░  34.5%  Rust (42 files)`
///
/// `fraction` should be in `0.0..=1.0`.
pub fn format_bar_chart(
    label: &str,
    count: usize,
    fraction: f64,
    unit: &str,
    color: bool,
) -> String {
    let filled = (fraction * BAR_WIDTH as f64).round() as usize;
    let empty = BAR_WIDTH.saturating_sub(filled);

    let bar_filled: String = "\u{2593}".repeat(filled); // ▓
    let bar_empty: String = "\u{2591}".repeat(empty); // ░
    let pct = fraction * 100.0;

    if color {
        format!(
            "  {}{} {:>5.1}%  {} ({} {})",
            bar_filled.cyan(),
            bar_empty.dimmed(),
            pct,
            label.bold(),
            count,
            unit,
        )
    } else {
        format!("  {bar_filled}{bar_empty} {pct:>5.1}%  {label} ({count} {unit})",)
    }
}

// ── Tier bullets ─────────────────────────────────────────────────────

/// Format a confidence tier bullet.
///
/// - `●` (filled circle) for high confidence (> 85%)
/// - `◐` (half circle) for medium confidence (50–85%)
/// - `○` (empty circle) for low confidence (< 50%)
///
/// Returns: `● High (12)` or `◐ Medium (5)` etc.
pub fn format_tier_bullet(label: &str, count: usize, tier: ConfidenceTier, color: bool) -> String {
    let (bullet, tier_color) = match tier {
        ConfidenceTier::High => ("\u{25CF}", TierColor::Green), // ●
        ConfidenceTier::Medium => ("\u{25D0}", TierColor::Yellow), // ◐
        ConfidenceTier::Low => ("\u{25CB}", TierColor::Red),    // ○
    };

    if color {
        let styled_bullet = match tier_color {
            TierColor::Green => bullet.green().to_string(),
            TierColor::Yellow => bullet.yellow().to_string(),
            TierColor::Red => bullet.red().to_string(),
        };
        format!("{styled_bullet} {label} ({count})")
    } else {
        format!("{bullet} {label} ({count})")
    }
}

/// Confidence tier for display purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfidenceTier {
    /// Confidence > 85%.
    High,
    /// Confidence 50–85%.
    Medium,
    /// Confidence < 50%.
    Low,
}

impl ConfidenceTier {
    /// Classify a confidence percentage into a tier.
    pub fn from_confidence(confidence: f64) -> Self {
        if confidence > 85.0 {
            Self::High
        } else if confidence >= 50.0 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

/// Internal color helper — avoids exposing owo-colors types in the public API.
enum TierColor {
    Green,
    Yellow,
    Red,
}

// ── Human-readable sizes ─────────────────────────────────────────────

/// Format a byte count as a human-readable size.
///
/// Uses base-10 units: KB (10^3), MB (10^6), GB (10^9).
///
/// Examples:
/// - `0` → `"0 B"`
/// - `1023` → `"1023 B"`
/// - `1024` → `"1.0 KB"`
/// - `1_500_000` → `"1.5 MB"`
/// - `2_500_000_000` → `"2.5 GB"`
pub fn format_human_size(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;

    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} GB", b / GB)
    }
}

// ── Number formatting ────────────────────────────────────────────────

/// Format a number with thousands separators.
///
/// Examples: `1234` → `"1,234"`, `1234567` → `"1,234,567"`.
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len <= 3 {
        return s;
    }

    let mut result = String::with_capacity(len + (len - 1) / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(b as char);
    }
    result
}

// ── Error / hint formatting ──────────────────────────────────────────

/// Format an error message with optional hint lines.
///
/// Produces:
/// ```text
/// error: something went wrong
///
/// hint: try doing X instead
/// hint: see https://example.com for details
/// ```
pub fn format_error_hint(message: &str, hints: &[&str], color: bool) -> String {
    let mut buf = String::new();

    if color {
        let _ = write!(buf, "{} {message}", "error:".red().bold());
    } else {
        let _ = write!(buf, "error: {message}");
    }

    if !hints.is_empty() {
        buf.push('\n');
        for hint in hints {
            buf.push('\n');
            if color {
                let _ = write!(buf, "{} {hint}", "hint:".cyan());
            } else {
                let _ = write!(buf, "hint: {hint}");
            }
        }
    }

    buf
}

// ── Bordered box ─────────────────────────────────────────────────────

/// Format text inside a bordered box using box-drawing characters.
///
/// ```text
/// ┌────────────────────────────────────────┐
/// │ your content here                      │
/// │ second line                            │
/// └────────────────────────────────────────┘
/// ```
///
/// Used for code/config snippet display (e.g., future `seshat init` output).
pub fn format_bordered_box(lines: &[&str], color: bool) -> String {
    let max_width = lines.iter().map(|l| l.len()).max().unwrap_or(0);
    // Minimum inner width of 20, padded by 1 space on each side.
    let inner = max_width.max(20);

    let mut buf = String::new();

    // Top border.
    let top = format!("\u{250C}{}\u{2510}", "\u{2500}".repeat(inner + 2)); // ┌─┐
    if color {
        let _ = writeln!(buf, "{}", top.dimmed());
    } else {
        let _ = writeln!(buf, "{top}");
    }

    // Content lines.
    for line in lines {
        let padded = format!("{line:<width$}", width = inner);
        if color {
            let _ = writeln!(
                buf,
                "{} {padded} {}",
                "\u{2502}".dimmed(), // │
                "\u{2502}".dimmed(),
            );
        } else {
            let _ = writeln!(buf, "\u{2502} {padded} \u{2502}");
        }
    }

    // Bottom border.
    let bottom = format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(inner + 2)); // └─┘
    if color {
        let _ = write!(buf, "{}", bottom.dimmed());
    } else {
        let _ = write!(buf, "{bottom}");
    }

    buf
}

// ── Level-prefixed messages ──────────────────────────────────────────

/// Format a warning message: `warn: {message}`.
pub fn format_warn(message: &str, color: bool) -> String {
    if color {
        format!("{} {message}", "warn:".yellow().bold())
    } else {
        format!("warn: {message}")
    }
}

/// Format an info message: `info: {message}`.
pub fn format_info(message: &str, color: bool) -> String {
    if color {
        format!("{} {message}", "info:".blue())
    } else {
        format!("info: {message}")
    }
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── color_enabled ────────────────────────────────────────────────

    #[test]
    fn test_no_color_respected() {
        // We can't safely set env vars in parallel tests, so just verify
        // the function returns a bool based on current env. The real test
        // is that format functions accept a `color: bool` parameter and
        // produce different output for `true` vs `false`.
        let _ = color_enabled(); // should not panic
    }

    // ── format_section_header ────────────────────────────────────────

    #[test]
    fn test_section_header_no_color() {
        let h = format_section_header("Project Overview", false);
        assert!(h.starts_with("── Project Overview "));
        assert!(h.contains("─────"));
        // Should be exactly 60 display characters (not bytes — ─ is 3 bytes in UTF-8).
        assert_eq!(h.chars().count(), HEADER_WIDTH);
    }

    #[test]
    fn test_section_header_with_color_contains_title() {
        let h = format_section_header("Project Overview", true);
        // Must still contain the title text even with ANSI codes.
        assert!(h.contains("Project Overview"));
    }

    #[test]
    fn test_section_header_long_title() {
        let title = "A".repeat(70);
        let h = format_section_header(&title, false);
        // Title longer than HEADER_WIDTH — remaining dashes is 0.
        assert!(h.contains(&title));
        assert!(!h.ends_with("──")); // no trailing dashes when title is too long
    }

    // ── format_bar_chart ─────────────────────────────────────────────

    #[test]
    fn test_bar_chart_full() {
        let b = format_bar_chart("Rust", 42, 1.0, "files", false);
        assert!(b.contains("▓".repeat(BAR_WIDTH).as_str()));
        assert!(!b.contains('░'));
        assert!(b.contains("100.0%"));
        assert!(b.contains("Rust"));
        assert!(b.contains("42 files"));
    }

    #[test]
    fn test_bar_chart_empty() {
        let b = format_bar_chart("Python", 0, 0.0, "files", false);
        assert!(b.contains("░".repeat(BAR_WIDTH).as_str()));
        assert!(b.contains("0.0%"));
    }

    #[test]
    fn test_bar_chart_half() {
        let b = format_bar_chart("TypeScript", 10, 0.5, "files", false);
        let filled = "▓".repeat(BAR_WIDTH / 2);
        let empty = "░".repeat(BAR_WIDTH / 2);
        assert!(b.contains(&filled));
        assert!(b.contains(&empty));
        assert!(b.contains("50.0%"));
    }

    #[test]
    fn test_bar_chart_with_color() {
        let b = format_bar_chart("Rust", 42, 0.75, "files", true);
        // Should contain the label and count even with ANSI codes.
        assert!(b.contains("Rust"));
        assert!(b.contains("42 files"));
        assert!(b.contains("75.0%"));
    }

    // ── format_tier_bullet ───────────────────────────────────────────

    #[test]
    fn test_tier_bullet_high() {
        let b = format_tier_bullet("High", 12, ConfidenceTier::High, false);
        assert!(b.contains('●'));
        assert!(b.contains("High (12)"));
    }

    #[test]
    fn test_tier_bullet_medium() {
        let b = format_tier_bullet("Medium", 5, ConfidenceTier::Medium, false);
        assert!(b.contains('◐'));
        assert!(b.contains("Medium (5)"));
    }

    #[test]
    fn test_tier_bullet_low() {
        let b = format_tier_bullet("Low", 3, ConfidenceTier::Low, false);
        assert!(b.contains('○'));
        assert!(b.contains("Low (3)"));
    }

    #[test]
    fn test_tier_bullet_with_color() {
        let b = format_tier_bullet("High", 12, ConfidenceTier::High, true);
        // Should contain the text even with ANSI codes.
        assert!(b.contains("High (12)"));
    }

    // ── ConfidenceTier::from_confidence ──────────────────────────────

    #[test]
    fn test_confidence_tier_boundaries() {
        assert_eq!(ConfidenceTier::from_confidence(100.0), ConfidenceTier::High);
        assert_eq!(ConfidenceTier::from_confidence(86.0), ConfidenceTier::High);
        assert_eq!(
            ConfidenceTier::from_confidence(85.0),
            ConfidenceTier::Medium,
        );
        assert_eq!(
            ConfidenceTier::from_confidence(50.0),
            ConfidenceTier::Medium,
        );
        assert_eq!(ConfidenceTier::from_confidence(49.9), ConfidenceTier::Low);
        assert_eq!(ConfidenceTier::from_confidence(0.0), ConfidenceTier::Low);
    }

    // ── format_human_size ────────────────────────────────────────────

    #[test]
    fn test_human_size_bytes() {
        assert_eq!(format_human_size(0), "0 B");
        assert_eq!(format_human_size(999), "999 B");
    }

    #[test]
    fn test_human_size_kilobytes() {
        assert_eq!(format_human_size(1_000), "1.0 KB");
        assert_eq!(format_human_size(1_500), "1.5 KB");
        assert_eq!(format_human_size(999_999), "1000.0 KB");
    }

    #[test]
    fn test_human_size_megabytes() {
        assert_eq!(format_human_size(1_000_000), "1.0 MB");
        assert_eq!(format_human_size(12_400_000), "12.4 MB");
    }

    #[test]
    fn test_human_size_gigabytes() {
        assert_eq!(format_human_size(2_500_000_000), "2.5 GB");
    }

    // ── format_number ────────────────────────────────────────────────

    #[test]
    fn test_format_number_no_separator() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn test_format_number_with_separators() {
        assert_eq!(format_number(1_234), "1,234");
        assert_eq!(format_number(1_234_567), "1,234,567");
        assert_eq!(format_number(1_000_000_000), "1,000,000,000");
    }

    // ── format_error_hint ────────────────────────────────────────────

    #[test]
    fn test_error_hint_no_hints() {
        let e = format_error_hint("something broke", &[], false);
        assert_eq!(e, "error: something broke");
    }

    #[test]
    fn test_error_hint_with_hints() {
        let e = format_error_hint("bad path", &["check the path", "try again"], false);
        assert!(e.contains("error: bad path"));
        assert!(e.contains("hint: check the path"));
        assert!(e.contains("hint: try again"));
    }

    #[test]
    fn test_error_hint_with_color() {
        let e = format_error_hint("something broke", &["try X"], true);
        assert!(e.contains("something broke"));
        assert!(e.contains("try X"));
    }

    // ── format_bordered_box ──────────────────────────────────────────

    #[test]
    fn test_bordered_box_basic() {
        let b = format_bordered_box(&["hello", "world"], false);
        assert!(b.contains('┌'));
        assert!(b.contains('┐'));
        assert!(b.contains('│'));
        assert!(b.contains('└'));
        assert!(b.contains('┘'));
        assert!(b.contains("hello"));
        assert!(b.contains("world"));
    }

    #[test]
    fn test_bordered_box_empty() {
        let b = format_bordered_box(&[], false);
        // Should still have top and bottom border.
        assert!(b.contains('┌'));
        assert!(b.contains('└'));
    }

    #[test]
    fn test_bordered_box_with_color() {
        let b = format_bordered_box(&["test"], true);
        assert!(b.contains("test"));
    }

    // ── format_warn / format_info ────────────────────────────────────

    #[test]
    fn test_warn_no_color() {
        assert_eq!(format_warn("oops", false), "warn: oops");
    }

    #[test]
    fn test_info_no_color() {
        assert_eq!(format_info("hello", false), "info: hello");
    }

    #[test]
    fn test_warn_with_color() {
        let w = format_warn("oops", true);
        assert!(w.contains("oops"));
    }

    // ── Verbosity ────────────────────────────────────────────────────

    #[test]
    fn test_verbosity_from_flags_default() {
        assert_eq!(Verbosity::from_flags(false, false), Verbosity::Default);
    }

    #[test]
    fn test_verbosity_from_flags_verbose() {
        assert_eq!(Verbosity::from_flags(true, false), Verbosity::Verbose);
    }

    #[test]
    fn test_verbosity_from_flags_quiet() {
        assert_eq!(Verbosity::from_flags(false, true), Verbosity::Quiet);
    }

    #[test]
    fn test_verbosity_from_flags_both_quiet_wins() {
        // When both are set, quiet takes precedence.
        assert_eq!(Verbosity::from_flags(true, true), Verbosity::Quiet);
    }

    #[test]
    fn test_verbosity_show_warnings() {
        assert!(!Verbosity::Quiet.show_warnings());
        assert!(Verbosity::Default.show_warnings());
        assert!(Verbosity::Verbose.show_warnings());
    }

    #[test]
    fn test_verbosity_show_findings() {
        assert!(!Verbosity::Quiet.show_findings());
        assert!(Verbosity::Default.show_findings());
        assert!(Verbosity::Verbose.show_findings());
    }

    #[test]
    fn test_verbosity_show_verbose() {
        assert!(!Verbosity::Quiet.show_verbose());
        assert!(!Verbosity::Default.show_verbose());
        assert!(Verbosity::Verbose.show_verbose());
    }

    // ── NO_COLOR integration ─────────────────────────────────────────
    //
    // We test that format functions produce different output with
    // color=true vs color=false, which proves the NO_COLOR path works
    // (since the caller passes `color_enabled()` → `false` when NO_COLOR
    // is set).

    #[test]
    fn test_no_color_produces_different_output() {
        let with_color = format_section_header("Test", true);
        let without_color = format_section_header("Test", false);
        // ANSI codes make the colored version longer.
        assert_ne!(with_color.len(), without_color.len());
    }

    #[test]
    fn test_no_color_bar_chart_different() {
        let with_color = format_bar_chart("Rust", 10, 0.5, "files", true);
        let without_color = format_bar_chart("Rust", 10, 0.5, "files", false);
        assert_ne!(with_color.len(), without_color.len());
    }

    #[test]
    fn test_no_color_error_hint_different() {
        let with_color = format_error_hint("fail", &["hint1"], true);
        let without_color = format_error_hint("fail", &["hint1"], false);
        assert_ne!(with_color.len(), without_color.len());
    }
}
