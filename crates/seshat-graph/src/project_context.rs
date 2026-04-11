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
    pub name: String,
    pub description: String,
}

/// Dependency information with per-domain canonical packages.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyInfo {
    /// Total number of dependencies detected.
    pub total: usize,
    /// Dependencies grouped by domain, with canonical (most-adopted) package.
    pub by_domain: Vec<DomainDependency>,
}

/// A dependency domain with its canonical package.
#[derive(Debug, Clone, Serialize)]
pub struct DomainDependency {
    /// Domain name (e.g., "HTTP", "logging", "testing").
    pub domain: String,
    /// The most-adopted package in this domain.
    pub canonical: String,
    /// Number of convention nodes related to this domain.
    pub conventions_count: usize,
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
    let golden = golden_files::get_golden_files(conn, golden_files::DEFAULT_GOLDEN_FILES_LIMIT)?;

    Ok(ProjectContextData {
        languages,
        modules,
        dependencies,
        conventions_count: filtered_conventions.len(),
        confidence_summary,
        golden_files: golden,
        submodules: Vec::new(),
    })
}

// ── Internal helpers ─────────────────────────────────────────

/// Raw convention node data loaded from the DB.
#[derive(Debug, Clone)]
struct ConventionRow {
    description: String,
    confidence: f64,
    #[allow(dead_code)]
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
    // Using DISTINCT on description to deduplicate nodes with identical descriptions
    // (can occur when multiple documentation files share the same content).
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT description, ext_data
             FROM nodes
             WHERE branch_id = ?1
               AND nature = 'fact'
               AND json_extract(ext_data, '$.source') = 'module_structure'
             ORDER BY description",
        )
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Failed to prepare modules query: {e}"
            )))
        })?;

    let rows = stmt
        .query_map(params![branch_id], |row| {
            let description: String = row.get(0)?;
            Ok(ModuleInfo {
                name: extract_module_name(&description),
                description,
            })
        })
        .map_err(|e| {
            GraphError::Storage(seshat_storage::StorageError::QueryError(format!(
                "Modules query failed: {e}"
            )))
        })?;

    let mut results = Vec::new();
    for row in rows {
        match row {
            Ok(info) => results.push(info),
            Err(e) => tracing::warn!("Skipping module row: {e}"),
        }
    }

    Ok(results)
}

/// Extract a short module name from a description string.
///
/// Heuristic: use the first word or phrase before a colon/dash.
fn extract_module_name(description: &str) -> String {
    if let Some(idx) = description.find(':') {
        description[..idx].trim().to_owned()
    } else if let Some(idx) = description.find(" — ") {
        description[..idx].trim().to_owned()
    } else if let Some(idx) = description.find(" - ") {
        description[..idx].trim().to_owned()
    } else {
        description
            .chars()
            .take(60)
            .collect::<String>()
            .trim()
            .to_owned()
    }
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
/// Extracts detector_name == "dependency_usage" conventions, groups by domain,
/// and picks the canonical (most-adopted) package per domain.
fn build_dependency_info(conventions: &[ConventionRow]) -> DependencyInfo {
    let mut domain_entries: HashMap<String, Vec<(String, usize)>> = HashMap::new();

    for conv in conventions {
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

        // Extract domain from description pattern "Uses X for Y"
        // or from the description containing domain keywords.
        let (domain, package_name) = extract_domain_and_package(&conv.description);

        let adoption = ext_val
            .get("adoption_rate")
            .and_then(|v| v.as_f64())
            .map(|r| (r * 100.0) as usize)
            .unwrap_or(0);

        domain_entries
            .entry(domain)
            .or_default()
            .push((package_name, adoption));
    }

    let mut by_domain: Vec<DomainDependency> = domain_entries
        .into_iter()
        .map(|(domain, entries)| {
            // Pick canonical = entry with highest adoption.
            let canonical = entries
                .iter()
                .max_by_key(|(_, adoption)| *adoption)
                .map(|(name, _)| name.clone())
                .unwrap_or_default();
            DomainDependency {
                domain,
                canonical,
                conventions_count: entries.len(),
            }
        })
        .collect();

    by_domain.sort_by(|a, b| b.conventions_count.cmp(&a.conventions_count));
    let total = by_domain.iter().map(|d| d.conventions_count).sum();

    DependencyInfo { total, by_domain }
}

/// Extract domain name and package name from a dependency_usage convention description.
///
/// Common patterns:
/// - "Uses reqwest for HTTP client (Rust)"
/// - "Uses axios for HTTP client (TypeScript)"
/// - "HTTP client: reqwest (Rust)"
fn extract_domain_and_package(description: &str) -> (String, String) {
    // Try "Uses <pkg> for <domain> (<lang>)" pattern.
    if let Some(rest) = description.strip_prefix("Uses ") {
        if let Some(for_idx) = rest.find(" for ") {
            let pkg = rest[..for_idx].trim();
            let domain_rest = &rest[for_idx + 5..];
            // Strip trailing " (Language)" if present.
            let domain = if let Some(paren_idx) = domain_rest.rfind(" (") {
                domain_rest[..paren_idx].trim()
            } else {
                domain_rest.trim()
            };
            return (domain.to_owned(), pkg.to_owned());
        }
        // "Uses <pkg> (<lang>)" — package only, no explicit domain.
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
        let conventions = vec![
            ConventionRow {
                description: "Uses reqwest for HTTP client (Rust)".into(),
                confidence: 0.9,
                nature: "convention".into(),
                ext_data: Some(r#"{"source":"auto_detected","detector_name":"dependency_usage","adoption_rate":0.9}"#.into()),
            },
            ConventionRow {
                description: "Uses tracing for logging (Rust)".into(),
                confidence: 0.8,
                nature: "convention".into(),
                ext_data: Some(r#"{"source":"auto_detected","detector_name":"dependency_usage","adoption_rate":0.8}"#.into()),
            },
            ConventionRow {
                description: "Uses log for logging (Rust)".into(),
                confidence: 0.3,
                nature: "convention".into(),
                ext_data: Some(r#"{"source":"auto_detected","detector_name":"dependency_usage","adoption_rate":0.3}"#.into()),
            },
            // Non-dependency convention should be ignored.
            ConventionRow {
                description: "snake_case naming".into(),
                confidence: 0.95,
                nature: "convention".into(),
                ext_data: Some(r#"{"source":"auto_detected","detector_name":"naming"}"#.into()),
            },
        ];

        let deps = build_dependency_info(&conventions);
        assert_eq!(deps.total, 3);
        assert_eq!(deps.by_domain.len(), 2);

        // Logging has 2 conventions, HTTP has 1.
        let logging = deps
            .by_domain
            .iter()
            .find(|d| d.domain == "logging")
            .unwrap();
        assert_eq!(logging.conventions_count, 2);
        assert_eq!(logging.canonical, "tracing"); // higher adoption

        let http = deps
            .by_domain
            .iter()
            .find(|d| d.domain == "HTTP client")
            .unwrap();
        assert_eq!(http.conventions_count, 1);
        assert_eq!(http.canonical, "reqwest");
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
    fn insert_module_node(conn: &Arc<Mutex<Connection>>, description: &str) {
        let repo = SqliteNodeRepository::new(conn.clone());
        let mut ext = serde_json::Map::new();
        ext.insert("source".into(), "module_structure".into());
        ext.insert("module_path".into(), description.into());
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
        insert_module_node(&conn, "src/handlers");

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
    fn query_modules_deduplicates_by_description() {
        let conn = test_conn();

        // Insert two module nodes with identical descriptions (e.g. from two
        // branches or duplicate insertions).
        insert_module_node(&conn, "src/handlers");
        insert_module_node(&conn, "src/handlers");
        insert_module_node(&conn, "src/models");

        let modules = query_modules(&conn, "main").unwrap();

        // DISTINCT should collapse identical descriptions.
        assert_eq!(modules.len(), 2);
        let names: Vec<&str> = modules.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"src/handlers"));
        assert!(names.contains(&"src/models"));
    }

    #[test]
    fn extract_module_name_with_colon() {
        assert_eq!(extract_module_name("auth: Authentication module"), "auth");
    }

    #[test]
    fn extract_module_name_with_em_dash() {
        assert_eq!(extract_module_name("auth — handles login"), "auth");
    }

    #[test]
    fn extract_module_name_with_dash() {
        assert_eq!(extract_module_name("auth - handles login"), "auth");
    }

    #[test]
    fn extract_module_name_plain() {
        assert_eq!(extract_module_name("short name"), "short name");
    }
}
