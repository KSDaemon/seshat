//! Project Overview section of the scan report.
//!
//! Displays a language breakdown with bar charts, total file count,
//! and dependency count with per-ecosystem breakdown.

use crate::format;
use crate::report::ReportData;

/// Print the Project Overview section.
///
/// ```text
/// ── Project Overview ─────────────────────────────────────────
///   ▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░  66.7%  Rust (40 files)
///   ▓▓▓▓▓▓░░░░░░░░░░░░░░  33.3%  Python (20 files)
///
///   60 files, 127 packages (98 cargo, 29 pip)
/// ```
pub fn print_overview(data: &ReportData, color: bool) {
    eprintln!(
        "{}",
        format::format_section_header("Project Overview", color)
    );

    // Language breakdown bar charts.
    if !data.language_breakdown.is_empty() {
        let total = data.total_files.max(1) as f64;
        for lc in &data.language_breakdown {
            let fraction = lc.count as f64 / total;
            eprintln!(
                "{}",
                format::format_bar_chart(
                    &lc.language.to_string(),
                    lc.count,
                    fraction,
                    "files",
                    color,
                ),
            );
        }
        eprintln!();
    }

    // Summary: file count + dependency count with ecosystem breakdown.
    let dep_detail = format_dependency_detail(data);
    eprintln!(
        "  {} files, {}",
        format::format_number(data.total_files as u64),
        dep_detail,
    );
    eprintln!();
}

/// Format the dependency count with per-ecosystem breakdown.
///
/// Returns e.g. `"127 packages (98 cargo, 29 pip)"` or `"0 packages"`.
fn format_dependency_detail(data: &ReportData) -> String {
    let total = data.total_dependencies;
    if data.dependency_breakdown.is_empty() {
        return format!("{} packages", format::format_number(total as u64));
    }

    let parts: Vec<String> = data
        .dependency_breakdown
        .iter()
        .map(|ec| format!("{} {}", ec.count, ec.label))
        .collect();

    format!(
        "{} packages ({})",
        format::format_number(total as u64),
        parts.join(", "),
    )
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{EcosystemCount, LanguageCount};
    use seshat_core::Language;

    fn make_report_data(
        language_breakdown: Vec<LanguageCount>,
        total_files: usize,
        total_dependencies: usize,
        dependency_breakdown: Vec<EcosystemCount>,
    ) -> ReportData {
        ReportData {
            language_breakdown,
            total_files,
            total_dependencies,
            dependency_breakdown,
            conventions: vec![],
            files_discovered: total_files,
            files_parsed: total_files,
            nodes_persisted: 0,
            edges_persisted: 0,
            manifests_analyzed: 0,
            docs_ingested: 0,
            db_path: std::path::PathBuf::from("/tmp/test.db"),
            db_size: 0,
            elapsed: std::time::Duration::from_secs(1),
            excluded_submodules: vec![],
            submodules_excluded_by_flag: false,
        }
    }

    #[test]
    fn test_format_dependency_detail_empty() {
        let data = make_report_data(vec![], 0, 0, vec![]);
        assert_eq!(format_dependency_detail(&data), "0 packages");
    }

    #[test]
    fn test_format_dependency_detail_single_ecosystem() {
        let data = make_report_data(
            vec![],
            10,
            42,
            vec![EcosystemCount {
                label: "cargo".to_owned(),
                count: 42,
            }],
        );
        assert_eq!(format_dependency_detail(&data), "42 packages (42 cargo)");
    }

    #[test]
    fn test_format_dependency_detail_multiple_ecosystems() {
        let data = make_report_data(
            vec![],
            10,
            127,
            vec![
                EcosystemCount {
                    label: "npm".to_owned(),
                    count: 98,
                },
                EcosystemCount {
                    label: "pip".to_owned(),
                    count: 29,
                },
            ],
        );
        assert_eq!(
            format_dependency_detail(&data),
            "127 packages (98 npm, 29 pip)",
        );
    }

    #[test]
    fn test_format_dependency_detail_large_number() {
        let data = make_report_data(
            vec![],
            10,
            1500,
            vec![EcosystemCount {
                label: "npm".to_owned(),
                count: 1500,
            }],
        );
        assert_eq!(format_dependency_detail(&data), "1,500 packages (1500 npm)",);
    }

    #[test]
    fn test_print_overview_does_not_panic() {
        let data = make_report_data(
            vec![
                LanguageCount {
                    language: Language::Rust,
                    count: 40,
                },
                LanguageCount {
                    language: Language::Python,
                    count: 20,
                },
            ],
            60,
            127,
            vec![
                EcosystemCount {
                    label: "cargo".to_owned(),
                    count: 98,
                },
                EcosystemCount {
                    label: "pip".to_owned(),
                    count: 29,
                },
            ],
        );
        // Just ensure it doesn't panic — output goes to stderr.
        print_overview(&data, false);
        print_overview(&data, true);
    }

    #[test]
    fn test_print_overview_empty_data() {
        let data = make_report_data(vec![], 0, 0, vec![]);
        // Should not panic even with empty data.
        print_overview(&data, false);
    }
}
