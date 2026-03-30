//! Conventions Detected and Next Steps sections of the scan report.
//!
//! Displays aggregated convention findings with confidence tiers, trend
//! indicators, and actionable next steps for the user.

use owo_colors::OwoColorize;
use seshat_core::Trend;
use seshat_detectors::AggregatedConvention;

use crate::format::{self, ConfidenceTier, Verbosity};
use crate::report::ReportData;

/// Maximum number of conventions shown in default (non-verbose) mode.
const DEFAULT_TOP_N: usize = 10;

/// Print the Conventions Detected section.
///
/// ```text
/// ── Conventions Detected (42) ────────────────────────────────
///   ● High (12)  ◐ Medium (18)  ○ Low (12)
///
///   ● snake_case function naming        ↑  98%  (naming)
///   ● thiserror for error types         ─  92%  (error_handling)
///   ◐ ESM module system                 ↑  75%  (export_patterns)
///   ...
/// ```
pub fn print_conventions(data: &ReportData, verbosity: Verbosity, color: bool) {
    let conventions = &data.conventions;
    if conventions.is_empty() {
        return;
    }

    // Sort by confidence descending for display.
    let mut sorted: Vec<&AggregatedConvention> = conventions.iter().collect();
    sorted.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Section header with count.
    let header_title = format!("Conventions Detected ({})", conventions.len());
    eprintln!("{}", format::format_section_header(&header_title, color),);

    // Tier summary line.
    let (high, medium, low) = count_tiers(conventions);
    print_tier_summary(high, medium, low, color);

    eprintln!();

    // Top findings list.
    let limit = if verbosity.show_verbose() {
        sorted.len()
    } else {
        sorted.len().min(DEFAULT_TOP_N)
    };

    for conv in sorted.iter().take(limit) {
        print_convention_line(conv, color);
    }

    // Show "... and N more" hint in default mode when truncated.
    let remaining = sorted.len().saturating_sub(limit);
    if remaining > 0 {
        if color {
            eprintln!(
                "  {} and {} more — use --verbose to see all",
                "...".dimmed(),
                remaining,
            );
        } else {
            eprintln!("  ... and {remaining} more — use --verbose to see all");
        }
    }

    eprintln!();
}

/// Print the Next Steps section.
///
/// ```text
/// ── Next Steps ───────────────────────────────────────────────
///   Run `seshat review` to validate detected conventions
///   Run `seshat serve` to start MCP server
///   Run `seshat init` to generate MCP config
/// ```
pub fn print_next_steps(color: bool) {
    eprintln!("{}", format::format_section_header("Next Steps", color));

    let steps = [
        "Run `seshat review` to validate detected conventions",
        "Run `seshat serve` to start MCP server",
        "Run `seshat init` to generate MCP config",
    ];

    for step in &steps {
        if color {
            eprintln!("  {}", step.dimmed());
        } else {
            eprintln!("  {step}");
        }
    }

    eprintln!();
}

/// Count conventions in each confidence tier.
fn count_tiers(conventions: &[AggregatedConvention]) -> (usize, usize, usize) {
    let mut high = 0;
    let mut medium = 0;
    let mut low = 0;

    for conv in conventions {
        match ConfidenceTier::from_confidence(conv.confidence * 100.0) {
            ConfidenceTier::High => high += 1,
            ConfidenceTier::Medium => medium += 1,
            ConfidenceTier::Low => low += 1,
        }
    }

    (high, medium, low)
}

/// Print the tier summary line with Unicode bullets.
fn print_tier_summary(high: usize, medium: usize, low: usize, color: bool) {
    let parts: Vec<String> = [
        (high, "High", ConfidenceTier::High),
        (medium, "Medium", ConfidenceTier::Medium),
        (low, "Low", ConfidenceTier::Low),
    ]
    .iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, label, tier)| format::format_tier_bullet(label, *count, *tier, color))
    .collect();

    if !parts.is_empty() {
        eprintln!("  {}", parts.join("  "));
    }
}

/// Format a trend indicator as a single character.
///
/// - `↑` Rising
/// - `─` Stable
/// - `↓` Declining
/// - ` ` Unknown (space)
fn trend_indicator(trend: Trend) -> &'static str {
    match trend {
        Trend::Rising => "\u{2191}",    // ↑
        Trend::Stable => "\u{2500}",    // ─
        Trend::Declining => "\u{2193}", // ↓
        Trend::Unknown => " ",
    }
}

/// Print a single convention finding line.
///
/// Format: `  ● description                ↑  98%  (detector_name)`
fn print_convention_line(conv: &AggregatedConvention, color: bool) {
    let tier = ConfidenceTier::from_confidence(conv.confidence * 100.0);
    let bullet = match tier {
        ConfidenceTier::High => "\u{25CF}",   // ●
        ConfidenceTier::Medium => "\u{25D0}", // ◐
        ConfidenceTier::Low => "\u{25CB}",    // ○
    };

    let pct = (conv.confidence * 100.0).round() as u32;
    let trend = trend_indicator(conv.trend);
    let detector = &conv.detector_name;
    let desc = &conv.description;

    if color {
        let styled_bullet = match tier {
            ConfidenceTier::High => bullet.green().to_string(),
            ConfidenceTier::Medium => bullet.yellow().to_string(),
            ConfidenceTier::Low => bullet.red().to_string(),
        };
        eprintln!(
            "  {styled_bullet} {desc:<40} {trend} {pct:>3}%  ({})",
            detector.dimmed(),
        );
    } else {
        eprintln!("  {bullet} {desc:<40} {trend} {pct:>3}%  ({detector})");
    }
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::{KnowledgeNature, KnowledgeWeight};

    fn make_convention(
        description: &str,
        detector_name: &str,
        confidence: f64,
        trend: Trend,
    ) -> AggregatedConvention {
        AggregatedConvention {
            detector_name: detector_name.to_owned(),
            description: description.to_owned(),
            nature: KnowledgeNature::Convention,
            adoption_count: (confidence * 10.0) as u32,
            total_count: 10,
            confidence,
            weight: KnowledgeWeight::Strong,
            evidence: vec![],
            trend,
        }
    }

    fn make_report_data_with_conventions(conventions: Vec<AggregatedConvention>) -> ReportData {
        ReportData {
            language_breakdown: vec![],
            total_files: 10,
            total_dependencies: 0,
            dependency_breakdown: vec![],
            conventions,
            files_discovered: 10,
            files_parsed: 10,
            nodes_persisted: 0,
            edges_persisted: 0,
            manifests_analyzed: 0,
            docs_ingested: 0,
            db_path: std::path::PathBuf::from("/tmp/test.db"),
            db_size: 12_400_000,
            elapsed: std::time::Duration::from_secs(2),
        }
    }

    // ── count_tiers ──────────────────────────────────────────────────

    #[test]
    fn test_count_tiers_empty() {
        let (high, medium, low) = count_tiers(&[]);
        assert_eq!((high, medium, low), (0, 0, 0));
    }

    #[test]
    fn test_count_tiers_mixed() {
        let conventions = vec![
            make_convention("a", "d1", 0.95, Trend::Rising), // high (95%)
            make_convention("b", "d2", 0.70, Trend::Stable), // medium (70%)
            make_convention("c", "d3", 0.30, Trend::Unknown), // low (30%)
            make_convention("d", "d4", 0.90, Trend::Declining), // high (90%)
        ];
        let (high, medium, low) = count_tiers(&conventions);
        assert_eq!(high, 2);
        assert_eq!(medium, 1);
        assert_eq!(low, 1);
    }

    #[test]
    fn test_count_tiers_boundary_85_percent() {
        // 85% is medium, not high (ConfidenceTier::from_confidence uses > 85)
        let conventions = vec![make_convention("a", "d1", 0.85, Trend::Stable)];
        let (high, medium, _low) = count_tiers(&conventions);
        assert_eq!(high, 0);
        assert_eq!(medium, 1);
    }

    #[test]
    fn test_count_tiers_boundary_50_percent() {
        // 50% is medium (ConfidenceTier::from_confidence uses >= 50)
        let conventions = vec![make_convention("a", "d1", 0.50, Trend::Stable)];
        let (_high, medium, low) = count_tiers(&conventions);
        assert_eq!(medium, 1);
        assert_eq!(low, 0);
    }

    // ── trend_indicator ──────────────────────────────────────────────

    #[test]
    fn test_trend_indicator_rising() {
        assert_eq!(trend_indicator(Trend::Rising), "\u{2191}"); // ↑
    }

    #[test]
    fn test_trend_indicator_stable() {
        assert_eq!(trend_indicator(Trend::Stable), "\u{2500}"); // ─
    }

    #[test]
    fn test_trend_indicator_declining() {
        assert_eq!(trend_indicator(Trend::Declining), "\u{2193}"); // ↓
    }

    #[test]
    fn test_trend_indicator_unknown() {
        assert_eq!(trend_indicator(Trend::Unknown), " ");
    }

    // ── print_conventions ────────────────────────────────────────────

    #[test]
    fn test_print_conventions_empty_does_not_panic() {
        let data = make_report_data_with_conventions(vec![]);
        // Should early-return without printing anything.
        print_conventions(&data, Verbosity::Default, false);
    }

    #[test]
    fn test_print_conventions_default_mode_does_not_panic() {
        let conventions = (0..15)
            .map(|i| {
                make_convention(
                    &format!("convention_{i}"),
                    "detector",
                    0.95 - (i as f64 * 0.05),
                    Trend::Stable,
                )
            })
            .collect();
        let data = make_report_data_with_conventions(conventions);
        // Default mode: should show at most DEFAULT_TOP_N (10) and "... and N more".
        print_conventions(&data, Verbosity::Default, false);
    }

    #[test]
    fn test_print_conventions_verbose_shows_all() {
        let conventions = (0..15)
            .map(|i| {
                make_convention(
                    &format!("convention_{i}"),
                    "detector",
                    0.95 - (i as f64 * 0.05),
                    Trend::Rising,
                )
            })
            .collect();
        let data = make_report_data_with_conventions(conventions);
        // Verbose mode should show all 15 without truncation.
        print_conventions(&data, Verbosity::Verbose, false);
    }

    #[test]
    fn test_print_conventions_with_color_does_not_panic() {
        let conventions = vec![
            make_convention("snake_case naming", "naming", 0.98, Trend::Rising),
            make_convention("thiserror usage", "error_handling", 0.72, Trend::Stable),
            make_convention(
                "test file placement",
                "test_patterns",
                0.30,
                Trend::Declining,
            ),
        ];
        let data = make_report_data_with_conventions(conventions);
        print_conventions(&data, Verbosity::Default, true);
    }

    #[test]
    fn test_print_conventions_quiet_mode() {
        let conventions = vec![make_convention("a", "d", 0.90, Trend::Stable)];
        let data = make_report_data_with_conventions(conventions);
        // Quiet mode — findings hidden, but print_conventions is only called
        // when show_findings() is true, so this just verifies no panic.
        print_conventions(&data, Verbosity::Quiet, false);
    }

    // ── print_next_steps ─────────────────────────────────────────────

    #[test]
    fn test_print_next_steps_no_color() {
        print_next_steps(false);
    }

    #[test]
    fn test_print_next_steps_with_color() {
        print_next_steps(true);
    }
}
