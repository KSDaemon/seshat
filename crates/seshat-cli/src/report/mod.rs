//! Scan report rendering.
//!
//! This module formats and prints the scan report to stderr. Report code
//! is separated from scan logic so that `scan.rs` handles orchestration
//! while `report/` handles presentation.
//!
//! ## Architecture
//!
//! The report module receives pre-computed `ReportData` — it never queries
//! the database directly. Data collection happens in `scan.rs` which passes
//! a fully populated `ReportData` struct.

/// Conventions Detected and Next Steps sections.
pub mod conventions;
/// Project Overview section (language breakdown, dependency counts).
pub mod overview;

use std::path::Path;

use seshat_core::Language;
use seshat_detectors::AggregatedConvention;

use crate::format::Verbosity;

/// Pre-computed data for the scan report.
///
/// Assembled in `scan.rs` from the scan result, parsed files, and aggregated
/// conventions. The report module only reads this — no database access.
#[derive(Debug)]
pub struct ReportData {
    /// Per-language file counts, sorted by count descending.
    pub language_breakdown: Vec<LanguageCount>,
    /// Total number of parsed files across all languages.
    pub total_files: usize,
    /// Total number of dependencies (unique packages across all files).
    pub total_dependencies: usize,
    /// Dependency counts per ecosystem (language), sorted by count descending.
    pub dependency_breakdown: Vec<EcosystemCount>,
    /// Aggregated convention findings from detectors.
    pub conventions: Vec<AggregatedConvention>,
    /// Number of files discovered (including unparseable).
    pub files_discovered: usize,
    /// Number of files successfully parsed.
    pub files_parsed: usize,
    /// Number of knowledge graph nodes persisted.
    pub nodes_persisted: usize,
    /// Number of knowledge graph edges persisted.
    pub edges_persisted: usize,
    /// Number of manifests analyzed.
    pub manifests_analyzed: usize,
    /// Number of docs ingested.
    pub docs_ingested: usize,
    /// Path to the database file.
    pub db_path: std::path::PathBuf,
    /// Database file size in bytes.
    pub db_size: u64,
    /// Total scan duration.
    pub elapsed: std::time::Duration,
    /// Submodule paths excluded from root file walk.
    ///
    /// These are always populated when `.gitmodules` declares submodules (their
    /// files are excluded from the root walk regardless of whether they are
    /// scanned separately).
    pub excluded_submodules: Vec<String>,
    /// Whether the user explicitly passed `--exclude-submodules`.
    ///
    /// When `true`, submodules were **not** scanned at all and the report
    /// should tell the user how to include them. When `false`, submodules
    /// were scanned into separate DBs and no warning is needed.
    pub submodules_excluded_by_flag: bool,
}

/// File count for a single language.
#[derive(Debug, Clone)]
pub struct LanguageCount {
    /// The language.
    pub language: Language,
    /// Number of files in this language.
    pub count: usize,
}

/// Dependency count for a single ecosystem (language).
#[derive(Debug, Clone)]
pub struct EcosystemCount {
    /// The ecosystem label (e.g., "npm", "pip", "cargo").
    pub label: String,
    /// Number of unique packages in this ecosystem.
    pub count: usize,
}

/// Print the full scan report, respecting verbosity and color settings.
///
/// Report structure:
/// 1. Scan stats (always)
/// 2. Project Overview (default + verbose)
/// 3. Conventions Detected (default + verbose)
/// 4. Next Steps (default + verbose)
/// 5. Summary line with convention count (always)
/// 6. Database path (default + verbose)
/// 7. Timing breakdown (verbose only)
/// 8. Warnings (default + verbose)
pub fn print_report(data: &ReportData, verbosity: Verbosity, color: bool) {
    use crate::format;

    eprintln!();

    // Scan stats — always shown (even in quiet mode).
    eprintln!(
        "  Scanned {} files, parsed {}, {} nodes, {} edges",
        format::format_number(data.files_discovered as u64),
        format::format_number(data.files_parsed as u64),
        format::format_number(data.nodes_persisted as u64),
        format::format_number(data.edges_persisted as u64),
    );

    if data.manifests_analyzed > 0 && verbosity.show_warnings() {
        eprintln!(
            "  Analyzed {} manifest(s), ingested {} doc(s)",
            data.manifests_analyzed, data.docs_ingested,
        );
    }

    if data.submodules_excluded_by_flag
        && !data.excluded_submodules.is_empty()
        && verbosity.show_warnings()
    {
        let paths_joined = data.excluded_submodules.join(", ");
        eprintln!(
            "  Skipped {} submodule(s): {} (remove --exclude-submodules to include)",
            data.excluded_submodules.len(),
            paths_joined,
        );
    }

    // Project Overview — shown in default and verbose.
    if verbosity.show_findings() {
        eprintln!();
        overview::print_overview(data, color);
    }

    // Conventions Detected — shown in default and verbose.
    if verbosity.show_findings() {
        conventions::print_conventions(data, verbosity, color);
    }

    // Next Steps — shown in default and verbose.
    if verbosity.show_findings() {
        conventions::print_next_steps(color);
    }

    // Summary line — always shown.
    eprintln!(
        "  {} conventions detected. Run `seshat review` to validate.",
        data.conventions.len(),
    );

    // Database path with human-readable size — shown in default and verbose.
    if verbosity.show_warnings() {
        eprintln!(
            "  Database: {} ({})",
            data.db_path.display(),
            format::format_human_size(data.db_size),
        );
    }

    // Scan timing — always shown.
    eprintln!("  Completed in {:.1}s", data.elapsed.as_secs_f64());

    // Verbose: detailed timing breakdown.
    if verbosity.show_verbose() {
        eprintln!();
        eprintln!("{}", format::format_section_header("Timing", color));
        eprintln!("  Total: {:.3}s", data.elapsed.as_secs_f64());
    }

    // Warnings — shown in default and verbose.
    if verbosity.show_warnings() && data.files_discovered == 0 {
        eprintln!();
        eprintln!(
            "  {}",
            format::format_warn(
                "no files discovered — check that the path contains source code",
                color,
            ),
        );
    }
}

/// Build [`ReportData`] from scan results and parsed files.
///
/// This is the single point of data collection for the report. It computes
/// language breakdown from the in-memory file list, and dependency counts
/// from manifest analysis results — no database queries needed.
pub fn build_report_data(
    scan_result: &seshat_scanner::ScanResult,
    files: &[seshat_core::ProjectFile],
    conventions: Vec<AggregatedConvention>,
    db_path: &Path,
    elapsed: std::time::Duration,
    submodules_excluded_by_flag: bool,
) -> ReportData {
    use std::collections::HashMap;

    // -- Language breakdown ------------------------------------------------
    let mut lang_counts: HashMap<Language, usize> = HashMap::new();
    for file in files {
        *lang_counts.entry(file.language).or_default() += 1;
    }
    let mut language_breakdown: Vec<LanguageCount> = lang_counts
        .into_iter()
        .map(|(language, count)| LanguageCount { language, count })
        .collect();
    language_breakdown.sort_by_key(|b| std::cmp::Reverse(b.count));

    // -- Dependency counts from manifest analysis -------------------------
    // Count declared dependencies per ecosystem (manifest type).
    let mut ecosystem_counts: HashMap<&str, usize> = HashMap::new();
    for analysis in &scan_result.manifest_analyses {
        let label = manifest_ecosystem_label(analysis.manifest_type);
        let count = analysis.dependencies.len();
        *ecosystem_counts.entry(label).or_default() += count;
    }

    let total_dependencies: usize = ecosystem_counts.values().sum();

    let mut dependency_breakdown: Vec<EcosystemCount> = ecosystem_counts
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| EcosystemCount {
            label: label.to_owned(),
            count,
        })
        .collect();
    dependency_breakdown.sort_by_key(|b| std::cmp::Reverse(b.count));

    // -- Database size ----------------------------------------------------
    let db_size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);

    ReportData {
        language_breakdown,
        total_files: files.len(),
        total_dependencies,
        dependency_breakdown,
        conventions,
        files_discovered: scan_result.files_discovered,
        files_parsed: scan_result.files_parsed,
        nodes_persisted: scan_result.nodes_persisted,
        edges_persisted: scan_result.edges_persisted,
        manifests_analyzed: scan_result.manifests_analyzed,
        docs_ingested: scan_result.docs_ingested,
        db_path: db_path.to_path_buf(),
        db_size,
        elapsed,
        excluded_submodules: scan_result.excluded_submodules.clone(),
        submodules_excluded_by_flag,
    }
}

/// Map a manifest type to its ecosystem label (used in dependency breakdown).
fn manifest_ecosystem_label(manifest_type: seshat_scanner::ManifestType) -> &'static str {
    match manifest_type {
        seshat_scanner::ManifestType::CargoToml => "cargo",
        seshat_scanner::ManifestType::PackageJson => "npm",
        seshat_scanner::ManifestType::PyprojectToml => "pip",
    }
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_ecosystem_label_cargo() {
        assert_eq!(
            manifest_ecosystem_label(seshat_scanner::ManifestType::CargoToml),
            "cargo",
        );
    }

    #[test]
    fn test_manifest_ecosystem_label_npm() {
        assert_eq!(
            manifest_ecosystem_label(seshat_scanner::ManifestType::PackageJson),
            "npm",
        );
    }

    #[test]
    fn test_manifest_ecosystem_label_pip() {
        assert_eq!(
            manifest_ecosystem_label(seshat_scanner::ManifestType::PyprojectToml),
            "pip",
        );
    }

    #[test]
    fn test_build_report_data_empty() {
        use std::path::PathBuf;
        use std::time::Duration;

        let scan_result = seshat_scanner::ScanResult {
            files_discovered: 0,
            files_parsed: 0,
            nodes_persisted: 0,
            edges_persisted: 0,
            manifests_analyzed: 0,
            docs_ingested: 0,
            manifest_analyses: vec![],
            incremental: None,
            file_dates: std::collections::HashMap::new(),
            excluded_submodules: vec![],
            source_map: std::collections::HashMap::new(),
            changed_paths: std::collections::HashSet::new(),
        };

        let data = build_report_data(
            &scan_result,
            &[],
            vec![],
            &PathBuf::from("/tmp/test.db"),
            Duration::from_secs(1),
            false,
        );

        assert_eq!(data.total_files, 0);
        assert_eq!(data.total_dependencies, 0);
        assert!(data.language_breakdown.is_empty());
        assert!(data.dependency_breakdown.is_empty());
    }

    #[test]
    fn test_build_report_data_language_breakdown() {
        use seshat_core::{LanguageIR, ProjectFile, RustIR};
        use std::path::PathBuf;
        use std::time::Duration;

        let files = vec![
            ProjectFile {
                path: PathBuf::from("src/main.rs"),
                language: Language::Rust,
                content_hash: "a".to_owned(),
                imports: vec![],
                exports: vec![],
                functions: vec![],
                types: vec![],
                dependencies_used: vec![],
                language_ir: LanguageIR::Rust(RustIR {
                    mod_declarations: vec![],
                    derive_macros: vec![],
                    trait_implementations: vec![],
                    error_types: vec![],
                    macro_calls: vec![],
                    function_calls: vec![],
                }),
                file_doc: None,
            },
            ProjectFile {
                path: PathBuf::from("src/lib.rs"),
                language: Language::Rust,
                content_hash: "b".to_owned(),
                imports: vec![],
                exports: vec![],
                functions: vec![],
                types: vec![],
                dependencies_used: vec![],
                language_ir: LanguageIR::Rust(RustIR {
                    mod_declarations: vec![],
                    derive_macros: vec![],
                    trait_implementations: vec![],
                    error_types: vec![],
                    macro_calls: vec![],
                    function_calls: vec![],
                }),
                file_doc: None,
            },
            ProjectFile {
                path: PathBuf::from("app.py"),
                language: Language::Python,
                content_hash: "c".to_owned(),
                imports: vec![],
                exports: vec![],
                functions: vec![],
                types: vec![],
                dependencies_used: vec![],
                language_ir: LanguageIR::Python(seshat_core::PythonIR {
                    has_all_export: false,
                    is_init_file: false,
                    type_hints_used: false,
                    decorators: vec![],
                    function_calls: vec![],
                }),
                file_doc: None,
            },
        ];

        let scan_result = seshat_scanner::ScanResult {
            files_discovered: 3,
            files_parsed: 3,
            nodes_persisted: 10,
            edges_persisted: 5,
            manifests_analyzed: 0,
            docs_ingested: 0,
            manifest_analyses: vec![],
            incremental: None,
            file_dates: std::collections::HashMap::new(),
            excluded_submodules: vec![],
            source_map: std::collections::HashMap::new(),
            changed_paths: std::collections::HashSet::new(),
        };

        let data = build_report_data(
            &scan_result,
            &files,
            vec![],
            &PathBuf::from("/tmp/test.db"),
            Duration::from_secs(2),
            false,
        );

        assert_eq!(data.total_files, 3);
        assert_eq!(data.language_breakdown.len(), 2);
        // Sorted by count descending — Rust (2) before Python (1).
        assert_eq!(data.language_breakdown[0].language, Language::Rust);
        assert_eq!(data.language_breakdown[0].count, 2);
        assert_eq!(data.language_breakdown[1].language, Language::Python);
        assert_eq!(data.language_breakdown[1].count, 1);
    }

    #[test]
    fn test_build_report_data_dependency_breakdown() {
        use seshat_core::DependencyDomain;
        use seshat_scanner::{DeclaredDependency, ManifestAnalysis};
        use std::path::PathBuf;
        use std::time::Duration;

        let manifest_analyses = vec![ManifestAnalysis {
            manifest_path: PathBuf::from("Cargo.toml"),
            manifest_type: seshat_scanner::ManifestType::CargoToml,
            internal_names: vec!["seshat_scanner".to_owned()],
            dependencies: vec![
                seshat_scanner::manifest::DependencyUsageStats {
                    dependency: DeclaredDependency {
                        name: "serde".to_owned(),
                        version: "1.0".to_owned(),
                        is_dev: false,
                        category: DependencyDomain::Serialization,
                    },
                    files_using: 2,
                    is_dead: false,
                },
                seshat_scanner::manifest::DependencyUsageStats {
                    dependency: DeclaredDependency {
                        name: "tokio".to_owned(),
                        version: "1.0".to_owned(),
                        is_dev: false,
                        category: DependencyDomain::AsyncRuntime,
                    },
                    files_using: 1,
                    is_dead: false,
                },
            ],
        }];

        let scan_result = seshat_scanner::ScanResult {
            files_discovered: 2,
            files_parsed: 2,
            nodes_persisted: 0,
            edges_persisted: 0,
            manifests_analyzed: 1,
            docs_ingested: 0,
            manifest_analyses,
            incremental: None,
            file_dates: std::collections::HashMap::new(),
            excluded_submodules: vec![],
            source_map: std::collections::HashMap::new(),
            changed_paths: std::collections::HashSet::new(),
        };

        let data = build_report_data(
            &scan_result,
            &[],
            vec![],
            &PathBuf::from("/tmp/test.db"),
            Duration::from_secs(1),
            false,
        );

        // serde + tokio = 2 declared dependencies from Cargo.toml.
        assert_eq!(data.total_dependencies, 2);
        assert_eq!(data.dependency_breakdown.len(), 1);
        assert_eq!(data.dependency_breakdown[0].label, "cargo");
        assert_eq!(data.dependency_breakdown[0].count, 2);
    }
}
