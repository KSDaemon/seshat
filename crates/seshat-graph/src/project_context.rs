//! Project context aggregation for the `query_project_context` MCP tool.
//!
//! Aggregates project-level data from the database: language breakdown,
//! dependency breakdown (extracted from convention nodes), conventions count,
//! confidence summary, and golden files.
//!
//! All queries run against the SQLite database — no filesystem access needed.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::error::GraphError;
use crate::golden_files::{self, GoldenFile};
use crate::{SOURCE_AUTO_DETECTED, SOURCE_USER, SQL_NOT_REMOVED};

// ── Response data types ──────────────────────────────────────

/// Full project context data returned by the MCP tool.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectContextData {
    /// Language breakdown sorted by file count (descending).
    pub languages: Vec<LanguageInfo>,
    /// Module-type nodes discovered in the project.
    pub modules: Vec<ModuleInfo>,
    /// Dependency information grouped by domain.
    pub dependencies: DependencyInfo,
    /// Total number of convention nodes.
    pub conventions_count: usize,
    /// Confidence distribution across convention nodes.
    pub confidence_summary: ConfidenceSummary,
    /// Top convention-compliant files.
    pub golden_files: Vec<GoldenFile>,
    /// Git submodules (always empty — multi-repo scoping deferred).
    pub submodules: Vec<String>,
}

/// Language info with file count.
#[derive(Debug, Clone, Serialize)]
pub struct LanguageInfo {
    pub language: String,
    pub file_count: usize,
}

/// Module-type node info.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleInfo {
    /// Relative path of the module directory, e.g. `crates/seshat-graph/src`.
    pub name: String,
    /// Human-readable purpose of the module.
    ///
    /// Derived from file-level doc comments (PR D). `null` until doc-comment
    /// extraction is implemented.
    pub purpose: Option<String>,
    /// Source files contained in this module (relative paths).
    pub files: Vec<String>,
}

/// Dependency information grouped by functional domain.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyInfo {
    /// Total number of unique packages detected across all domains.
    pub total: usize,
    /// Dependencies grouped by domain, with most-used package highlighted.
    pub by_domain: Vec<DomainDependency>,
}

/// A dependency domain with its most-used package and all packages found.
#[derive(Debug, Clone, Serialize)]
pub struct DomainDependency {
    /// Domain name (e.g., "HTTP", "logging", "testing").
    pub domain: String,
    /// The package used in the most files across the project for this domain.
    pub most_used: String,
    /// All unique packages found in this domain, sorted alphabetically.
    pub packages: Vec<String>,
}

/// Confidence distribution across convention nodes.
#[derive(Debug, Clone, Serialize)]
pub struct ConfidenceSummary {
    /// Number of conventions with confidence > 85%.
    pub high_count: usize,
    /// Number of conventions with confidence 50%–85%.
    pub medium_count: usize,
    /// Number of conventions with confidence < 50%.
    pub low_count: usize,
    /// Ratio of high-confidence conventions to total.
    pub high_ratio: f64,
}

// ── Query function ───────────────────────────────────────────

/// Build full project context data from the database.
///
/// Queries `files_ir` for language breakdown, `nodes` for conventions/modules,
/// and `golden_files` for top compliant files.
///
/// `focus_area` optionally filters results to a specific domain (case-insensitive
/// substring match on convention descriptions).
pub fn query_project_context(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
    focus_area: Option<&str>,
) -> Result<ProjectContextData, GraphError> {
    let languages = query_language_breakdown(conn, branch_id)?;
    let modules = query_modules(conn, branch_id)?;
    let conventions = query_conventions(conn, branch_id)?;

    // Filter conventions by focus_area if provided.
    let filtered_conventions: Cow<'_, [ConventionRow]> = if let Some(focus) = focus_area {
        let focus_lower = focus.to_lowercase();
        Cow::Owned(
            conventions
                .iter()
                .filter(|c| c.description.to_lowercase().contains(&focus_lower))
                .cloned()
                .collect::<Vec<_>>(),
        )
    } else {
        Cow::Borrowed(&conventions)
    };

    let dependencies = build_dependency_info(&filtered_conventions);
    let confidence_summary = build_confidence_summary(&filtered_conventions);
    let golden = golden_files::get_golden_files(
        conn,
        &seshat_core::BranchId::from(branch_id),
        golden_files::DEFAULT_GOLDEN_FILES_LIMIT,
    )?;

    let submodules = query_submodule_paths(conn);

    Ok(ProjectContextData {
        languages,
        modules,
        dependencies,
        conventions_count: filtered_conventions.len(),
        confidence_summary,
        golden_files: golden,
        submodules,
    })
}

// ── Internal helpers ─────────────────────────────────────────

/// Query the `submodules` table and return all registered mount paths
/// (e.g. `["external/walt-portal"]`).
///
/// Returns an empty `Vec` if the table does not exist (pre-submodule DBs)
/// or if no submodules have been registered. This function never errors —
/// submodule data is informational and its absence must not break the query.
fn query_submodule_paths(conn: &Arc<Mutex<Connection>>) -> Vec<String> {
    let conn = match conn.lock() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut stmt = match conn.prepare("SELECT relative_path FROM submodules ORDER BY relative_path")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(), // table may not exist in older DBs
    };

    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
    match rows {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Raw convention node data loaded from the DB.
#[derive(Debug, Clone)]
struct ConventionRow {
    description: String,
    confidence: f64,
    nature: String,
    ext_data: Option<String>,
}

/// Query language breakdown from `files_ir` grouped by language.
fn query_language_breakdown(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<LanguageInfo>, GraphError> {
    let conn = crate::lock_conn(conn)?;

    let mut stmt = conn
        .prepare(
            "SELECT language, COUNT(*) as cnt
             FROM files_ir
             WHERE branch_id = ?1
             GROUP BY language
             ORDER BY cnt DESC",
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare language breakdown query: {e}"
            )))
        })?;

    let rows = stmt
        .query_map(params![branch_id], |row| {
            Ok(LanguageInfo {
                language: row.get(0)?,
                file_count: row.get::<_, i64>(1)? as usize,
            })
        })
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Language breakdown query failed: {e}"
            )))
        })?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(info) => results.push(info),
            Err(e) => tracing::warn!("Skipping language row: {e}"),
        }
    }

    Ok(results)
}

/// Query module-type nodes from the `nodes` table.
fn query_modules(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<ModuleInfo>, GraphError> {
    let conn = crate::lock_conn(conn)?;

    // Module-type nodes are tagged with source = 'module_structure' in ext_data.
    // GROUP BY module_path deduplicates nodes with the same path (e.g. from
    // incremental rescans that may produce duplicate inserts before cleanup).
    let mut stmt = conn
        .prepare(
            "SELECT description, ext_data
             FROM nodes
             WHERE branch_id = ?1
               AND nature = 'fact'
               AND json_extract(ext_data, '$.source') = 'module_structure'
             GROUP BY json_extract(ext_data, '$.module_path')
             ORDER BY json_extract(ext_data, '$.module_path')",
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare modules query: {e}"
            )))
        })?;

    let rows = stmt
        .query_map(params![branch_id], |row| {
            let _description: String = row.get(0)?;
            let ext_raw: Option<String> = row.get(1)?;
            Ok(ext_raw)
        })
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Modules query failed: {e}"
            )))
        })?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(Some(ext_raw)) => match serde_json::from_str::<serde_json::Value>(&ext_raw) {
                Ok(ext) => {
                    let raw_name = ext
                        .get("module_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)");
                    // An empty module_path means the project root — files
                    // that live directly in the scanned directory with no
                    // sub-directory.  Give it an unambiguous display name.
                    let name = if raw_name.is_empty() {
                        "(project root)".to_owned()
                    } else {
                        raw_name.to_owned()
                    };

                    let files: Vec<String> = ext
                        .get("files")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();

                    let purpose = ext
                        .get("purpose")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);

                    results.push(ModuleInfo {
                        name,
                        purpose,
                        files,
                    });
                }
                Err(e) => tracing::warn!("Failed to parse module ext_data: {e}"),
            },
            Ok(None) => tracing::warn!("Module node has no ext_data — skipping"),
            Err(e) => tracing::warn!("Skipping module row: {e}"),
        }
    }

    // Note: SQL GROUP BY json_extract(ext_data, '$.module_path') above ensures
    // that duplicate module nodes (same module_path) produce at most one row.
    // No additional in-memory deduplication is needed.

    Ok(results)
}

/// Query all convention nodes for a branch.
fn query_conventions(
    conn: &Arc<Mutex<Connection>>,
    branch_id: &str,
) -> Result<Vec<ConventionRow>, GraphError> {
    let conn = crate::lock_conn(conn)?;

    let mut stmt = conn
        .prepare(&format!(
            "SELECT description, confidence, nature, ext_data
             FROM nodes
             WHERE branch_id = ?1
               AND json_extract(ext_data, '$.source') IN ('{SOURCE_AUTO_DETECTED}', '{SOURCE_USER}')
               AND {SQL_NOT_REMOVED}",
        ))
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare conventions query: {e}"
            )))
        })?;

    let rows = stmt
        .query_map(params![branch_id], |row| {
            Ok(ConventionRow {
                description: row.get(0)?,
                confidence: row.get(1)?,
                nature: row.get(2)?,
                ext_data: row.get(3)?,
            })
        })
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Conventions query failed: {e}"
            )))
        })?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(c) => results.push(c),
            Err(e) => tracing::warn!("Skipping convention row: {e}"),
        }
    }

    Ok(results)
}

/// Build dependency info from convention nodes.
///
/// Extracts only `detector_name == "dependency_usage"` **Convention** findings
/// (nature == "convention") where `finding_category == "dependency"`. Groups by
/// domain, deduplicates packages, and picks the most-used (highest appearance
/// count across files) package per domain.
///
/// Wrapper/facade findings (wrapper_declaration, wrapper_violation) and
/// Observation findings (heuristic, conflicting, dead-dep) are intentionally
/// excluded — they must not pollute the dependency summary.
fn build_dependency_info(conventions: &[ConventionRow]) -> DependencyInfo {
    // domain → package_name → appearance count (how many files emit this package
    // as a Convention finding for this domain)
    let mut domain_packages: HashMap<String, HashMap<String, usize>> = HashMap::new();

    for conv in conventions {
        if conv.nature != "convention" {
            continue;
        }

        let ext = match &conv.ext_data {
            Some(s) => s,
            None => continue,
        };

        let ext_val: serde_json::Value = match serde_json::from_str(ext) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let detector = ext_val
            .get("detector_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if detector != "dependency_usage" {
            continue;
        }

        let category = ext_val
            .get("finding_category")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if category != "dependency" {
            continue;
        }

        let (domain, package_name) = extract_domain_and_package(&conv.description);

        *domain_packages
            .entry(domain)
            .or_default()
            .entry(package_name)
            .or_insert(0) += 1;
    }

    let mut by_domain: Vec<DomainDependency> = domain_packages
        .into_iter()
        .map(|(domain, packages_map)| {
            // most_used = package appearing in the most files for this domain
            let most_used = packages_map
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(name, _)| name.clone())
                .unwrap_or_default();

            let mut packages: Vec<String> = packages_map.into_keys().collect();
            packages.sort();

            DomainDependency {
                domain,
                most_used,
                packages,
            }
        })
        .collect();

    by_domain.sort_by(|a, b| b.packages.len().cmp(&a.packages.len()));
    let total = by_domain.iter().map(|d| d.packages.len()).sum();

    DependencyInfo { total, by_domain }
}

/// Extract domain name and package name from a dependency_usage convention description.
///
/// Supported patterns (as emitted by `DependencyUsageDetector`):
/// - `"Canonical {domain} library: {package}"` — primary detector output
/// - `"Likely {domain} library (heuristic): {package}"` — heuristic detector output
/// - `"Uses {pkg} for {domain} ({lang})"` — alternative pattern
/// - `"Uses {pkg} ({lang})"` — package only, no explicit domain
fn extract_domain_and_package(description: &str) -> (String, String) {
    // Pattern: "Canonical {domain} library: {package}"
    if let Some(rest) = description.strip_prefix("Canonical ") {
        if let Some(lib_idx) = rest.find(" library: ") {
            let domain = rest[..lib_idx].trim();
            let package = rest[lib_idx + " library: ".len()..].trim();
            return (domain.to_owned(), package.to_owned());
        }
    }

    // Pattern: "Likely {domain} library (heuristic): {package}"
    if let Some(rest) = description.strip_prefix("Likely ") {
        if let Some(lib_idx) = rest.find(" library (heuristic): ") {
            let domain = rest[..lib_idx].trim();
            let package = rest[lib_idx + " library (heuristic): ".len()..].trim();
            return (domain.to_owned(), package.to_owned());
        }
    }

    // Pattern: "Uses {pkg} for {domain} ({lang})"
    if let Some(rest) = description.strip_prefix("Uses ") {
        if let Some(for_idx) = rest.find(" for ") {
            let pkg = rest[..for_idx].trim();
            let domain_rest = &rest[for_idx + 5..];
            let domain = if let Some(paren_idx) = domain_rest.rfind(" (") {
                domain_rest[..paren_idx].trim()
            } else {
                domain_rest.trim()
            };
            return (domain.to_owned(), pkg.to_owned());
        }
        // "Uses {pkg} ({lang})" — package only.
        if let Some(paren_idx) = rest.rfind(" (") {
            let pkg = rest[..paren_idx].trim();
            return ("general".to_owned(), pkg.to_owned());
        }
    }

    // Fallback: use the whole description as domain.
    ("other".to_owned(), description.to_owned())
}

/// Build confidence summary from convention nodes.
fn build_confidence_summary(conventions: &[ConventionRow]) -> ConfidenceSummary {
    let mut high = 0usize;
    let mut medium = 0usize;
    let mut low = 0usize;

    for conv in conventions {
        if conv.confidence > 0.85 {
            high += 1;
        } else if conv.confidence >= 0.50 {
            medium += 1;
        } else {
            low += 1;
        }
    }

    let total = conventions.len();
    let high_ratio = if total > 0 {
        high as f64 / total as f64
    } else {
        0.0
    };

    ConfidenceSummary {
        high_count: high,
        medium_count: medium,
        low_count: low,
        high_ratio,
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::test_helpers::make_project_file;
    use seshat_core::{
        BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, Language, NodeId,
    };
    use seshat_storage::{
        FileIRRepository, NodeRepository, SqliteFileIRRepository, SqliteNodeRepository,
    };

    use crate::test_helpers::test_conn;

    /// Insert a file_ir record with the given language.
    fn insert_file(conn: &Arc<Mutex<Connection>>, path: &str, lang: Language) {
        let repo = SqliteFileIRRepository::new(conn.clone());
        let branch = BranchId::from("main");
        let mut file = make_project_file(lang);
        file.path = path.into();
        file.content_hash = format!("hash_{path}");
        repo.upsert(&branch, &file, None).unwrap();
    }

    /// Insert a convention node with ext_data.
    fn insert_convention(
        conn: &Arc<Mutex<Connection>>,
        description: &str,
        confidence: f64,
        detector_name: &str,
    ) {
        let repo = SqliteNodeRepository::new(conn.clone());
        let branch = BranchId::from("main");

        let mut ext = serde_json::Map::new();
        ext.insert("source".into(), "auto_detected".into());
        ext.insert("detector_name".into(), detector_name.into());
        ext.insert("adoption_rate".into(), serde_json::json!(confidence));

        let node = KnowledgeNode {
            id: NodeId(0),
            branch_id: branch,
            nature: KnowledgeNature::Convention,
            weight: KnowledgeWeight::Strong,
            confidence,
            adoption_count: (confidence * 10.0) as u32,
            total_count: 10,
            description: description.to_owned(),
            ext_data: Some(serde_json::Value::Object(ext)),
        };

        repo.insert(&node).unwrap();
    }

    #[test]
    fn language_breakdown_groups_by_language() {
        let conn = test_conn();
        insert_file(&conn, "src/main.rs", Language::Rust);
        insert_file(&conn, "src/lib.rs", Language::Rust);
        insert_file(&conn, "src/utils.ts", Language::TypeScript);

        let langs = query_language_breakdown(&conn, "main").unwrap();
        assert_eq!(langs.len(), 2);
        assert_eq!(langs[0].language, "rust");
        assert_eq!(langs[0].file_count, 2);
        assert_eq!(langs[1].language, "typescript");
        assert_eq!(langs[1].file_count, 1);
    }

    #[test]
    fn language_breakdown_empty_db() {
        let conn = test_conn();
        let langs = query_language_breakdown(&conn, "main").unwrap();
        assert!(langs.is_empty());
    }

    #[test]
    fn conventions_query_filters_by_source() {
        let conn = test_conn();

        // Insert an auto-detected convention.
        insert_convention(
            &conn,
            "Uses thiserror for error handling (Rust)",
            0.9,
            "dependency_usage",
        );

        // Insert a user decision.
        {
            let repo = SqliteNodeRepository::new(conn.clone());
            let mut ext = serde_json::Map::new();
            ext.insert("source".into(), "user".into());
            let node = KnowledgeNode {
                id: NodeId(0),
                branch_id: BranchId::from("main"),
                nature: KnowledgeNature::Decision,
                weight: KnowledgeWeight::Rule,
                confidence: 1.0,
                adoption_count: 1,
                total_count: 1,
                description: "Always use Result for errors".to_owned(),
                ext_data: Some(serde_json::Value::Object(ext)),
            };
            repo.insert(&node).unwrap();
        }

        // Insert a node without source (should be excluded).
        {
            let repo = SqliteNodeRepository::new(conn.clone());
            let node = KnowledgeNode {
                id: NodeId(0),
                branch_id: BranchId::from("main"),
                nature: KnowledgeNature::Fact,
                weight: KnowledgeWeight::Info,
                confidence: 1.0,
                adoption_count: 1,
                total_count: 1,
                description: "Some fact".to_owned(),
                ext_data: None,
            };
            repo.insert(&node).unwrap();
        }

        let conventions = query_conventions(&conn, "main").unwrap();
        assert_eq!(conventions.len(), 2); // auto_detected + user
    }

    #[test]
    fn confidence_summary_categorizes_correctly() {
        let conventions = vec![
            ConventionRow {
                description: "a".into(),
                confidence: 0.95,
                nature: "convention".into(),
                ext_data: None,
            },
            ConventionRow {
                description: "b".into(),
                confidence: 0.90,
                nature: "convention".into(),
                ext_data: None,
            },
            ConventionRow {
                description: "c".into(),
                confidence: 0.70,
                nature: "convention".into(),
                ext_data: None,
            },
            ConventionRow {
                description: "d".into(),
                confidence: 0.50,
                nature: "convention".into(),
                ext_data: None,
            },
            ConventionRow {
                description: "e".into(),
                confidence: 0.30,
                nature: "convention".into(),
                ext_data: None,
            },
        ];

        let summary = build_confidence_summary(&conventions);
        assert_eq!(summary.high_count, 2); // 0.95, 0.90
        assert_eq!(summary.medium_count, 2); // 0.70, 0.50
        assert_eq!(summary.low_count, 1); // 0.30
        assert!((summary.high_ratio - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn confidence_summary_empty() {
        let summary = build_confidence_summary(&[]);
        assert_eq!(summary.high_count, 0);
        assert_eq!(summary.medium_count, 0);
        assert_eq!(summary.low_count, 0);
        assert!((summary.high_ratio).abs() < f64::EPSILON);
    }

    #[test]
    fn dependency_info_groups_by_domain() {
        // Use the actual detector output format: "Canonical {domain} library: {package}"
        let dep_conv = |desc: &str| {
            ConventionRow {
            description: desc.to_owned(),
            confidence: 0.9,
            nature: "convention".into(),
            ext_data: Some(
                r#"{"source":"auto_detected","detector_name":"dependency_usage","finding_category":"dependency"}"#.into(),
            ),
        }
        };

        let conventions = vec![
            dep_conv("Canonical HTTP library: reqwest"),
            // tracing appears in 2 files → count=2; log in 1 file → count=1
            dep_conv("Canonical logging library: tracing"),
            dep_conv("Canonical logging library: tracing"),
            dep_conv("Canonical logging library: log"),
            // Non-dependency convention — must be ignored.
            ConventionRow {
                description: "snake_case naming".into(),
                confidence: 0.95,
                nature: "convention".into(),
                ext_data: Some(r#"{"source":"auto_detected","detector_name":"naming"}"#.into()),
            },
            // Observation finding — must NOT appear in dependency summary.
            ConventionRow {
                description: "Conflicting logging libraries: tracing, log".into(),
                confidence: 0.5,
                nature: "observation".into(),
                ext_data: Some(
                    r#"{"source":"auto_detected","detector_name":"dependency_usage"}"#.into(),
                ),
            },
        ];

        let deps = build_dependency_info(&conventions);
        assert_eq!(deps.total, 3, "reqwest + tracing + log = 3 unique packages");
        assert_eq!(deps.by_domain.len(), 2);

        // Logging has 2 unique packages; tracing has count=2 so it's most_used.
        let logging = deps
            .by_domain
            .iter()
            .find(|d| d.domain == "logging")
            .unwrap();
        assert_eq!(logging.packages.len(), 2);
        assert_eq!(
            logging.most_used, "tracing",
            "tracing seen in 2 files vs log in 1"
        );
        assert!(logging.packages.contains(&"tracing".to_owned()));
        assert!(logging.packages.contains(&"log".to_owned()));

        let http = deps.by_domain.iter().find(|d| d.domain == "HTTP").unwrap();
        assert_eq!(http.packages.len(), 1);
        assert_eq!(http.most_used, "reqwest");
        assert_eq!(http.packages, vec!["reqwest".to_owned()]);
    }

    #[test]
    fn extract_domain_and_package_uses_pattern() {
        let (domain, pkg) = extract_domain_and_package("Uses reqwest for HTTP client (Rust)");
        assert_eq!(domain, "HTTP client");
        assert_eq!(pkg, "reqwest");
    }

    #[test]
    fn extract_domain_and_package_without_lang() {
        let (domain, pkg) = extract_domain_and_package("Uses axios for HTTP client");
        assert_eq!(domain, "HTTP client");
        assert_eq!(pkg, "axios");
    }

    #[test]
    fn extract_domain_and_package_fallback() {
        let (domain, pkg) = extract_domain_and_package("Some other pattern");
        assert_eq!(domain, "other");
        assert_eq!(pkg, "Some other pattern");
    }

    #[test]
    fn extract_domain_and_package_heuristic_pattern() {
        let (domain, pkg) =
            extract_domain_and_package("Likely HTTP library (heuristic): websockets");
        assert_eq!(domain, "HTTP");
        assert_eq!(pkg, "websockets");
    }

    #[test]
    fn dependency_info_deduplicates_packages_across_files() {
        // Same package appearing in multiple files should only count once.
        let conv = |desc: &str, rate: f64| ConventionRow {
            description: desc.to_owned(),
            confidence: 0.9,
            nature: "convention".into(),
            ext_data: Some(format!(
                r#"{{"source":"auto_detected","detector_name":"dependency_usage","finding_category":"dependency","adoption_rate":{rate}}}"#
            )),
        };

        let conventions = vec![
            conv("Canonical database library: sqlalchemy", 0.9),
            conv("Canonical database library: sqlalchemy", 0.9), // duplicate from another file
            conv("Canonical database library: redis", 0.5),
        ];

        let deps = build_dependency_info(&conventions);
        assert_eq!(deps.total, 2, "sqlalchemy and redis are 2 unique packages");
        let db = deps
            .by_domain
            .iter()
            .find(|d| d.domain == "database")
            .unwrap();
        assert_eq!(db.packages.len(), 2);
        assert_eq!(db.most_used, "sqlalchemy"); // higher count
        assert_eq!(
            db.packages,
            vec!["redis".to_owned(), "sqlalchemy".to_owned()]
        );
    }

    #[test]
    fn full_query_project_context_integration() {
        let conn = test_conn();

        // Insert files.
        insert_file(&conn, "src/main.rs", Language::Rust);
        insert_file(&conn, "src/lib.rs", Language::Rust);
        insert_file(&conn, "app.ts", Language::TypeScript);

        // Insert conventions.
        insert_convention(
            &conn,
            "Uses reqwest for HTTP client (Rust)",
            0.9,
            "dependency_usage",
        );
        insert_convention(&conn, "snake_case naming (Rust)", 0.95, "naming");

        let ctx = query_project_context(&conn, "main", None).unwrap();

        assert_eq!(ctx.languages.len(), 2);
        assert_eq!(ctx.conventions_count, 2);
        assert_eq!(ctx.confidence_summary.high_count, 2);
        assert!(ctx.submodules.is_empty());
    }

    #[test]
    fn focus_area_filters_conventions() {
        let conn = test_conn();

        insert_convention(
            &conn,
            "Uses reqwest for HTTP client (Rust)",
            0.9,
            "dependency_usage",
        );
        insert_convention(&conn, "snake_case naming (Rust)", 0.95, "naming");
        insert_convention(
            &conn,
            "Uses thiserror for error handling (Rust)",
            0.85,
            "dependency_usage",
        );

        let ctx = query_project_context(&conn, "main", Some("naming")).unwrap();
        assert_eq!(ctx.conventions_count, 1);

        let ctx_http = query_project_context(&conn, "main", Some("HTTP")).unwrap();
        assert_eq!(ctx_http.conventions_count, 1);
    }

    /// Insert a module_structure fact node (as the scanner produces).
    fn insert_module_node(conn: &Arc<Mutex<Connection>>, module_path: &str, files: &[&str]) {
        let repo = SqliteNodeRepository::new(conn.clone());
        let description = format!(
            "Module '{}' containing {} file(s)",
            module_path,
            files.len()
        );
        let ext = serde_json::json!({
            "source": "module_structure",
            "module_path": module_path,
            "files": files,
            "file_count": files.len(),
            "languages": ["rust"],
        });
        let node = KnowledgeNode {
            id: NodeId(0),
            branch_id: BranchId::from("main"),
            nature: KnowledgeNature::Fact,
            weight: KnowledgeWeight::Info,
            confidence: 1.0,
            adoption_count: 1,
            total_count: 1,
            description,
            ext_data: Some(ext),
        };
        repo.insert(&node).unwrap();
    }

    /// Insert a documentation fact node (markdown heading, list item, etc.).
    fn insert_doc_node(conn: &Arc<Mutex<Connection>>, description: &str) {
        let repo = SqliteNodeRepository::new(conn.clone());
        let mut ext = serde_json::Map::new();
        ext.insert("source".into(), "documentation".into());
        ext.insert("doc_type".into(), "markdown".into());
        let node = KnowledgeNode {
            id: NodeId(0),
            branch_id: BranchId::from("main"),
            nature: KnowledgeNature::Fact,
            weight: KnowledgeWeight::Info,
            confidence: 1.0,
            adoption_count: 1,
            total_count: 1,
            description: description.to_owned(),
            ext_data: Some(serde_json::Value::Object(ext)),
        };
        repo.insert(&node).unwrap();
    }

    #[test]
    fn query_modules_excludes_documentation_nodes() {
        let conn = test_conn();

        // Insert a real module node.
        insert_module_node(&conn, "src/handlers", &["src/handlers/mod.rs"]);

        // Insert documentation nodes that must NOT appear in modules.
        insert_doc_node(&conn, "Are there admin, support, or oversight roles?");
        insert_doc_node(&conn, "\"Absolutely essential\" (just \"essential\")");
        insert_doc_node(&conn, "Some markdown heading");

        let modules = query_modules(&conn, "main").unwrap();

        // Only the module_structure node should appear.
        assert_eq!(modules.len(), 1, "Expected 1 module, got: {modules:?}");
        assert_eq!(modules[0].name, "src/handlers");
    }

    #[test]
    fn query_modules_returns_files() {
        let conn = test_conn();

        insert_module_node(
            &conn,
            "src/handlers",
            &["src/handlers/user.rs", "src/handlers/auth.rs"],
        );

        let modules = query_modules(&conn, "main").unwrap();

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "src/handlers");
        assert_eq!(modules[0].files.len(), 2);
        assert!(
            modules[0]
                .files
                .contains(&"src/handlers/user.rs".to_owned())
        );
        assert!(
            modules[0]
                .files
                .contains(&"src/handlers/auth.rs".to_owned())
        );
        assert!(modules[0].purpose.is_none(), "purpose is null until PR D");
    }

    #[test]
    fn query_modules_deduplicates_by_module_path() {
        let conn = test_conn();

        // Insert duplicate module nodes (same module_path).
        insert_module_node(&conn, "src/handlers", &["src/handlers/mod.rs"]);
        insert_module_node(&conn, "src/handlers", &["src/handlers/mod.rs"]);
        insert_module_node(&conn, "src/models", &["src/models/user.rs"]);

        let modules = query_modules(&conn, "main").unwrap();

        // GROUP BY module_path should collapse duplicates.
        assert_eq!(modules.len(), 2);
        let names: Vec<&str> = modules.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"src/handlers"));
        assert!(names.contains(&"src/models"));
    }

    #[test]
    fn query_modules_purpose_from_ext_data() {
        let conn = test_conn();

        // Simulate a module with purpose already set in ext_data (as PR D will produce).
        let repo = SqliteNodeRepository::new(conn.clone());
        let ext = serde_json::json!({
            "source": "module_structure",
            "module_path": "src/auth",
            "files": ["src/auth/mod.rs"],
            "file_count": 1,
            "languages": ["rust"],
            "purpose": "Handles authentication and session management",
        });
        let node = KnowledgeNode {
            id: NodeId(0),
            branch_id: BranchId::from("main"),
            nature: KnowledgeNature::Fact,
            weight: KnowledgeWeight::Info,
            confidence: 1.0,
            adoption_count: 1,
            total_count: 1,
            description: "Module 'src/auth'".to_owned(),
            ext_data: Some(ext),
        };
        repo.insert(&node).unwrap();

        let modules = query_modules(&conn, "main").unwrap();

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "src/auth");
        assert_eq!(
            modules[0].purpose.as_deref(),
            Some("Handles authentication and session management")
        );
    }

    #[test]
    fn query_modules_root_module_gets_display_name() {
        let conn = test_conn();

        // Insert a module node with empty module_path (project root files).
        let repo = SqliteNodeRepository::new(conn.clone());
        let ext = serde_json::json!({
            "source": "module_structure",
            "module_path": "",    // ← empty = project root
            "files": ["main.py", "server.py"],
            "file_count": 2,
            "languages": ["python"],
        });
        let node = KnowledgeNode {
            id: NodeId(0),
            branch_id: BranchId::from("main"),
            nature: KnowledgeNature::Fact,
            weight: KnowledgeWeight::Info,
            confidence: 1.0,
            adoption_count: 1,
            total_count: 1,
            description: "Module '(root)'".to_owned(),
            ext_data: Some(ext),
        };
        repo.insert(&node).unwrap();

        let modules = query_modules(&conn, "main").unwrap();

        assert_eq!(modules.len(), 1);
        assert_eq!(
            modules[0].name, "(project root)",
            "empty module_path must map to '(project root)'"
        );
    }

    #[test]
    fn query_submodule_paths_returns_empty_when_no_submodules_registered() {
        // In-memory DB has the submodules table (via migrations) but no rows inserted.
        // Must return empty vec without error.
        let conn = test_conn();
        let paths = query_submodule_paths(&conn);
        assert!(
            paths.is_empty(),
            "no submodules registered → must return empty, got: {paths:?}"
        );
    }

    #[test]
    fn query_submodule_paths_returns_registered_submodules() {
        let conn = test_conn();

        // Manually create the submodules table and insert a row.
        {
            let c = conn.lock().unwrap();
            c.execute_batch(
                "CREATE TABLE IF NOT EXISTS submodules (
                    id INTEGER PRIMARY KEY,
                    relative_path TEXT UNIQUE NOT NULL,
                    name TEXT NOT NULL,
                    db_path TEXT NOT NULL,
                    commit_hash TEXT
                );
                INSERT INTO submodules (relative_path, name, db_path)
                    VALUES ('external/walt-portal', 'walt-portal', '/tmp/wp.db');
                INSERT INTO submodules (relative_path, name, db_path)
                    VALUES ('external/other', 'other', '/tmp/other.db');",
            )
            .unwrap();
        }

        let paths = query_submodule_paths(&conn);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"external/walt-portal".to_owned()));
        assert!(paths.contains(&"external/other".to_owned()));
    }
}
