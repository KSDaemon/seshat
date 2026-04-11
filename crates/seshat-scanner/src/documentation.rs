//! Documentation ingestion for the knowledge graph.
//!
//! Parses structured information from documentation files into
//! [`KnowledgeNode`]s that enrich the knowledge graph. Supports:
//!
//! - **Markdown** (`.md`): headings and lists extracted as Fact/Rule nodes
//! - **JSON Schema** (`.json`): data structure definitions extracted as Fact nodes
//! - **OpenAPI** (`.yaml`, `.yml`): endpoint definitions extracted as Fact nodes
//!
//! All documentation-sourced nodes are tagged with `"source": "documentation"`
//! in their `ext_data` field. No NLP or prose-level convention extraction is
//! performed — only structured information is extracted.

use std::path::{Path, PathBuf};

use seshat_core::{BranchId, KnowledgeNature, KnowledgeNode, KnowledgeWeight, NodeId};

use crate::error::ScanError;

/// The type of documentation file being parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocType {
    /// Markdown documentation (`.md`).
    Markdown,
    /// JSON Schema definition (`.json`).
    JsonSchema,
    /// OpenAPI specification (`.yaml` / `.yml`).
    OpenApi,
}

impl DocType {
    /// Detect documentation type from file extension.
    ///
    /// Returns `None` if the extension is not a recognised documentation format.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "md" => Some(Self::Markdown),
            "json" => Some(Self::JsonSchema),
            "yaml" | "yml" => Some(Self::OpenApi),
            _ => None,
        }
    }
}

/// Result of parsing a single documentation file.
#[derive(Debug, Clone)]
pub struct DocumentationResult {
    /// The path to the documentation file (relative to project root).
    pub path: PathBuf,
    /// The type of documentation file.
    pub doc_type: DocType,
    /// Knowledge nodes extracted from this file.
    pub nodes: Vec<KnowledgeNode>,
}

/// Parse a documentation file and extract structured knowledge nodes.
///
/// # Arguments
///
/// * `path` - Relative path from the project root.
/// * `content` - The raw file content as a string.
/// * `branch_id` - The branch identifier for the knowledge graph nodes.
///
/// # Returns
///
/// A [`DocumentationResult`] containing the extracted knowledge nodes, or a
/// [`ScanError::DocumentationError`] if the file cannot be parsed.
pub fn parse_documentation(
    path: &Path,
    content: &str,
    branch_id: &BranchId,
) -> Result<DocumentationResult, ScanError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let doc_type = DocType::from_extension(ext).ok_or_else(|| ScanError::DocumentationError {
        path: path.to_path_buf(),
        reason: format!("Unsupported documentation extension: {ext}"),
    })?;

    let nodes = match doc_type {
        DocType::Markdown => parse_markdown(path, content, branch_id),
        DocType::JsonSchema => parse_json_schema(path, content, branch_id)?,
        DocType::OpenApi => parse_openapi(path, content, branch_id)?,
    };

    Ok(DocumentationResult {
        path: path.to_path_buf(),
        doc_type,
        nodes,
    })
}

// ---------------------------------------------------------------------------
// Markdown parsing
// ---------------------------------------------------------------------------

/// Parse Markdown content and extract H1/H2 sections as knowledge nodes.
///
/// Each H1 or H2 heading starts a new section node. Everything between two
/// H1/H2 headings — including H3-H6 sub-headings, list items, and prose —
/// is collected as the section's `content` in `ext_data` rather than
/// generating separate nodes. This prevents a single large Markdown file
/// from producing thousands of noisy nodes in the knowledge graph.
///
/// Files with no H1/H2 headings produce no nodes (prose-only files are
/// intentionally skipped).
fn parse_markdown(path: &Path, content: &str, branch_id: &BranchId) -> Vec<KnowledgeNode> {
    /// Emit a completed section as a [`KnowledgeNode`], if `section` is `Some`.
    fn flush_section(
        counter: &mut i64,
        nodes: &mut Vec<KnowledgeNode>,
        section: Option<(String, u32, Vec<String>)>,
        path: &Path,
        branch_id: &BranchId,
    ) {
        let Some((title, level, body_lines)) = section else {
            return;
        };
        // Trim trailing blank lines from body.
        let body = body_lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned();
        *counter += 1;
        nodes.push(make_doc_node(
            NodeId(*counter),
            branch_id,
            KnowledgeNature::Fact,
            KnowledgeWeight::Info,
            title,
            serde_json::json!({
                "source": "documentation",
                "doc_type": "markdown",
                "file": path.to_string_lossy(),
                "element": "section",
                "level": level,
                "content": body,
            }),
        ));
    }

    let mut nodes = Vec::new();
    let mut node_counter: i64 = 0;

    // Current open section: (heading_text, heading_level, content_lines).
    let mut current: Option<(String, u32, Vec<String>)> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(heading) = parse_heading(trimmed) {
            // Only H1 and H2 open new section nodes; H3+ are body content.
            if heading.level <= 2 {
                // Flush the previous section.
                flush_section(
                    &mut node_counter,
                    &mut nodes,
                    current.take(),
                    path,
                    branch_id,
                );
                current = Some((heading.text, heading.level, Vec::new()));
                continue;
            }
        }

        // Everything else (H3+, lists, prose, blank lines) is body content
        // of the current section.
        if let Some((_, _, ref mut body)) = current {
            body.push(line.to_owned());
        }
        // Lines before the first H1/H2 are silently discarded.
    }

    // Flush the final section.
    flush_section(&mut node_counter, &mut nodes, current, path, branch_id);

    nodes
}

/// A parsed Markdown heading.
struct HeadingInfo {
    level: u32,
    text: String,
}

/// Try to parse a line as a Markdown heading (`# Heading`).
fn parse_heading(line: &str) -> Option<HeadingInfo> {
    if !line.starts_with('#') {
        return None;
    }

    let hashes = line.chars().take_while(|&c| c == '#').count() as u32;
    if hashes > 6 {
        return None;
    }

    let rest = &line[hashes as usize..];
    // Must be followed by a space (ATX heading requirement)
    if !rest.starts_with(' ') {
        return None;
    }

    let text = rest.trim().to_string();
    if text.is_empty() {
        return None;
    }

    Some(HeadingInfo {
        level: hashes,
        text,
    })
}

// ---------------------------------------------------------------------------
// JSON Schema parsing
// ---------------------------------------------------------------------------

/// Parse a JSON Schema file and extract data structure definitions.
///
/// Extracts the schema title/description and all property definitions as
/// Fact/Info knowledge nodes.
fn parse_json_schema(
    path: &Path,
    content: &str,
    branch_id: &BranchId,
) -> Result<Vec<KnowledgeNode>, ScanError> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| ScanError::DocumentationError {
            path: path.to_path_buf(),
            reason: format!("Invalid JSON: {e}"),
        })?;

    // Verify this looks like a JSON Schema (has "$schema", "type", or "properties")
    let obj = value
        .as_object()
        .ok_or_else(|| ScanError::DocumentationError {
            path: path.to_path_buf(),
            reason: "JSON Schema must be an object".to_string(),
        })?;

    let is_schema = obj.contains_key("$schema")
        || obj.contains_key("properties")
        || (obj.contains_key("type") && obj.contains_key("title"));

    if !is_schema {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    let mut node_counter: i64 = 0;

    // Extract the root schema definition
    let schema_title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Schema");

    let schema_description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let description = if schema_description.is_empty() {
        format!("JSON Schema: {schema_title}")
    } else {
        format!("JSON Schema: {schema_title} — {schema_description}")
    };

    node_counter += 1;
    nodes.push(make_doc_node(
        NodeId(node_counter),
        branch_id,
        KnowledgeNature::Fact,
        KnowledgeWeight::Info,
        description,
        serde_json::json!({
            "source": "documentation",
            "doc_type": "json_schema",
            "file": path.to_string_lossy(),
            "element": "schema",
            "schema_title": schema_title,
        }),
    ));

    // Extract properties as individual nodes
    if let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) {
        let required: Vec<&str> = obj
            .get("required")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        for (prop_name, prop_value) in properties {
            let prop_type = prop_value
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let prop_desc = prop_value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let is_required = required.contains(&prop_name.as_str());

            let desc = if prop_desc.is_empty() {
                format!(
                    "Property: {prop_name} ({prop_type}{})",
                    if is_required { ", required" } else { "" }
                )
            } else {
                format!(
                    "Property: {prop_name} ({prop_type}{}) — {prop_desc}",
                    if is_required { ", required" } else { "" }
                )
            };

            node_counter += 1;
            nodes.push(make_doc_node(
                NodeId(node_counter),
                branch_id,
                KnowledgeNature::Fact,
                KnowledgeWeight::Info,
                desc,
                serde_json::json!({
                    "source": "documentation",
                    "doc_type": "json_schema",
                    "file": path.to_string_lossy(),
                    "element": "property",
                    "schema_title": schema_title,
                    "property_name": prop_name,
                    "property_type": prop_type,
                    "required": is_required,
                }),
            ));
        }
    }

    // Extract definitions/$defs as additional type nodes
    let defs = obj
        .get("definitions")
        .or_else(|| obj.get("$defs"))
        .and_then(|v| v.as_object());

    if let Some(definitions) = defs {
        for (def_name, def_value) in definitions {
            let def_desc = def_value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let def_type = def_value
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("object");

            let desc = if def_desc.is_empty() {
                format!("Definition: {def_name} ({def_type})")
            } else {
                format!("Definition: {def_name} ({def_type}) — {def_desc}")
            };

            node_counter += 1;
            nodes.push(make_doc_node(
                NodeId(node_counter),
                branch_id,
                KnowledgeNature::Fact,
                KnowledgeWeight::Info,
                desc,
                serde_json::json!({
                    "source": "documentation",
                    "doc_type": "json_schema",
                    "file": path.to_string_lossy(),
                    "element": "definition",
                    "definition_name": def_name,
                    "definition_type": def_type,
                }),
            ));
        }
    }

    Ok(nodes)
}

// ---------------------------------------------------------------------------
// OpenAPI parsing
// ---------------------------------------------------------------------------

/// Parse an OpenAPI specification and extract endpoint definitions.
///
/// Extracts each path + method combination as a Fact/Info knowledge node.
fn parse_openapi(
    path: &Path,
    content: &str,
    branch_id: &BranchId,
) -> Result<Vec<KnowledgeNode>, ScanError> {
    let value: serde_yml::Value =
        serde_yml::from_str(content).map_err(|e| ScanError::DocumentationError {
            path: path.to_path_buf(),
            reason: format!("Invalid YAML: {e}"),
        })?;

    // Verify this looks like an OpenAPI spec
    let mapping = value
        .as_mapping()
        .ok_or_else(|| ScanError::DocumentationError {
            path: path.to_path_buf(),
            reason: "OpenAPI spec must be a YAML mapping".to_string(),
        })?;

    let has_openapi = mapping.contains_key(yaml_key("openapi"));
    let has_swagger = mapping.contains_key(yaml_key("swagger"));

    if !has_openapi && !has_swagger {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    let mut node_counter: i64 = 0;

    // Extract API title from info.title
    let api_title = yaml_get_mapping(mapping, "info")
        .and_then(|m| yaml_get_str(m, "title"))
        .unwrap_or("Untitled API");

    let api_version = yaml_get_mapping(mapping, "info")
        .and_then(|m| yaml_get_str(m, "version"))
        .unwrap_or("");

    let api_desc = if api_version.is_empty() {
        format!("API: {api_title}")
    } else {
        format!("API: {api_title} (v{api_version})")
    };

    node_counter += 1;
    nodes.push(make_doc_node(
        NodeId(node_counter),
        branch_id,
        KnowledgeNature::Fact,
        KnowledgeWeight::Info,
        api_desc,
        serde_json::json!({
            "source": "documentation",
            "doc_type": "openapi",
            "file": path.to_string_lossy(),
            "element": "api",
            "api_title": api_title,
            "api_version": api_version,
        }),
    ));

    // Extract paths/endpoints
    if let Some(paths) = yaml_get_mapping(mapping, "paths") {
        let http_methods = [
            "get", "post", "put", "delete", "patch", "options", "head", "trace",
        ];

        for (path_key, path_value) in paths {
            let endpoint_path = match path_key.as_str() {
                Some(p) => p,
                None => continue,
            };

            let methods = match path_value.as_mapping() {
                Some(m) => m,
                None => continue,
            };

            for method_name in &http_methods {
                let method_key = serde_yml::Value::String(method_name.to_string());
                if let Some(method_value) = methods.get(&method_key) {
                    let method_map = method_value.as_mapping();

                    let summary = method_map
                        .and_then(|m| yaml_get_str(m, "summary"))
                        .unwrap_or("");

                    let operation_id = method_map
                        .and_then(|m| yaml_get_str(m, "operationId"))
                        .unwrap_or("");

                    let method_upper = method_name.to_uppercase();
                    let desc = if summary.is_empty() {
                        format!("Endpoint: {method_upper} {endpoint_path}")
                    } else {
                        format!("Endpoint: {method_upper} {endpoint_path} — {summary}")
                    };

                    // Extract response codes
                    let response_codes: Vec<String> = method_map
                        .and_then(|m| yaml_get_mapping(m, "responses"))
                        .map(|responses| {
                            responses
                                .keys()
                                .filter_map(|k| k.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    // Extract tags
                    let tags: Vec<String> = method_map
                        .and_then(|m| yaml_get_seq(m, "tags"))
                        .map(|seq| {
                            seq.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    node_counter += 1;
                    nodes.push(make_doc_node(
                        NodeId(node_counter),
                        branch_id,
                        KnowledgeNature::Fact,
                        KnowledgeWeight::Info,
                        desc,
                        serde_json::json!({
                            "source": "documentation",
                            "doc_type": "openapi",
                            "file": path.to_string_lossy(),
                            "element": "endpoint",
                            "api_title": api_title,
                            "path": endpoint_path,
                            "method": method_upper,
                            "operation_id": operation_id,
                            "response_codes": response_codes,
                            "tags": tags,
                        }),
                    ));
                }
            }
        }
    }

    // Extract component schemas (OpenAPI 3.x)
    if let Some(schemas) =
        yaml_get_mapping(mapping, "components").and_then(|m| yaml_get_mapping(m, "schemas"))
    {
        for (schema_key, schema_value) in schemas {
            let schema_name = match schema_key.as_str() {
                Some(n) => n,
                None => continue,
            };

            let schema_map = schema_value.as_mapping();

            let schema_type = schema_map
                .and_then(|m| yaml_get_str(m, "type"))
                .unwrap_or("object");

            let schema_desc = schema_map
                .and_then(|m| yaml_get_str(m, "description"))
                .unwrap_or("");

            let desc = if schema_desc.is_empty() {
                format!("Schema: {schema_name} ({schema_type})")
            } else {
                format!("Schema: {schema_name} ({schema_type}) — {schema_desc}")
            };

            node_counter += 1;
            nodes.push(make_doc_node(
                NodeId(node_counter),
                branch_id,
                KnowledgeNature::Fact,
                KnowledgeWeight::Info,
                desc,
                serde_json::json!({
                    "source": "documentation",
                    "doc_type": "openapi",
                    "file": path.to_string_lossy(),
                    "element": "schema",
                    "api_title": api_title,
                    "schema_name": schema_name,
                    "schema_type": schema_type,
                }),
            ));
        }
    }

    // Extract Swagger 2.0 definitions
    if let Some(definitions) = yaml_get_mapping(mapping, "definitions") {
        for (def_key, def_value) in definitions {
            let def_name = match def_key.as_str() {
                Some(n) => n,
                None => continue,
            };

            let def_map = def_value.as_mapping();

            let def_type = def_map
                .and_then(|m| yaml_get_str(m, "type"))
                .unwrap_or("object");

            let def_desc = def_map
                .and_then(|m| yaml_get_str(m, "description"))
                .unwrap_or("");

            let desc = if def_desc.is_empty() {
                format!("Schema: {def_name} ({def_type})")
            } else {
                format!("Schema: {def_name} ({def_type}) — {def_desc}")
            };

            node_counter += 1;
            nodes.push(make_doc_node(
                NodeId(node_counter),
                branch_id,
                KnowledgeNature::Fact,
                KnowledgeWeight::Info,
                desc,
                serde_json::json!({
                    "source": "documentation",
                    "doc_type": "openapi",
                    "file": path.to_string_lossy(),
                    "element": "schema",
                    "api_title": api_title,
                    "schema_name": def_name,
                    "schema_type": def_type,
                }),
            ));
        }
    }

    Ok(nodes)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a `serde_yml::Value::String` key for YAML mapping lookups.
fn yaml_key(key: &str) -> serde_yml::Value {
    serde_yml::Value::String(key.to_string())
}

/// Get a string value from a YAML mapping by key.
fn yaml_get_str<'a>(mapping: &'a serde_yml::Mapping, key: &str) -> Option<&'a str> {
    mapping.get(yaml_key(key)).and_then(|v| v.as_str())
}

/// Get a nested mapping from a YAML mapping by key.
fn yaml_get_mapping<'a>(
    mapping: &'a serde_yml::Mapping,
    key: &str,
) -> Option<&'a serde_yml::Mapping> {
    mapping.get(yaml_key(key)).and_then(|v| v.as_mapping())
}

/// Get a nested sequence from a YAML mapping by key.
fn yaml_get_seq<'a>(mapping: &'a serde_yml::Mapping, key: &str) -> Option<&'a serde_yml::Sequence> {
    mapping.get(yaml_key(key)).and_then(|v| v.as_sequence())
}

/// Create a documentation-sourced knowledge node with standard fields.
fn make_doc_node(
    id: NodeId,
    branch_id: &BranchId,
    nature: KnowledgeNature,
    weight: KnowledgeWeight,
    description: String,
    ext_data: serde_json::Value,
) -> KnowledgeNode {
    KnowledgeNode {
        id,
        branch_id: branch_id.clone(),
        nature,
        weight,
        confidence: 1.0,
        adoption_count: 1,
        total_count: 1,
        description,
        ext_data: Some(ext_data),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::BranchId;

    fn branch() -> BranchId {
        BranchId::from("test")
    }

    // -----------------------------------------------------------------------
    // DocType detection
    // -----------------------------------------------------------------------

    #[test]
    fn doc_type_from_extension_markdown() {
        assert_eq!(DocType::from_extension("md"), Some(DocType::Markdown));
    }

    #[test]
    fn doc_type_from_extension_json() {
        assert_eq!(DocType::from_extension("json"), Some(DocType::JsonSchema));
    }

    #[test]
    fn doc_type_from_extension_yaml() {
        assert_eq!(DocType::from_extension("yaml"), Some(DocType::OpenApi));
        assert_eq!(DocType::from_extension("yml"), Some(DocType::OpenApi));
    }

    #[test]
    fn doc_type_from_extension_unknown() {
        assert_eq!(DocType::from_extension("rs"), None);
        assert_eq!(DocType::from_extension("txt"), None);
    }

    #[test]
    fn doc_type_case_insensitive() {
        assert_eq!(DocType::from_extension("MD"), Some(DocType::Markdown));
        assert_eq!(DocType::from_extension("YAML"), Some(DocType::OpenApi));
        assert_eq!(DocType::from_extension("Json"), Some(DocType::JsonSchema));
    }

    // -----------------------------------------------------------------------
    // parse_documentation dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn parse_documentation_unsupported_extension() {
        let result = parse_documentation(Path::new("file.txt"), "content", &branch());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ScanError::DocumentationError { .. }));
    }

    #[test]
    fn parse_documentation_routes_to_markdown() {
        // "# Hello\n- item" is one H1 section → one node
        let content = "# Hello\n- item";
        let result = parse_documentation(Path::new("README.md"), content, &branch()).unwrap();
        assert_eq!(result.doc_type, DocType::Markdown);
        assert_eq!(result.nodes.len(), 1);
    }

    #[test]
    fn parse_documentation_routes_to_json_schema() {
        let content = r#"{"$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "title": "Test"}"#;
        let result = parse_documentation(Path::new("schema.json"), content, &branch()).unwrap();
        assert_eq!(result.doc_type, DocType::JsonSchema);
        assert!(!result.nodes.is_empty());
    }

    #[test]
    fn parse_documentation_routes_to_openapi() {
        let content = "openapi: '3.0.0'\ninfo:\n  title: Test\n  version: '1.0'\npaths: {}";
        let result = parse_documentation(Path::new("api.yaml"), content, &branch()).unwrap();
        assert_eq!(result.doc_type, DocType::OpenApi);
        assert!(!result.nodes.is_empty());
    }

    // -----------------------------------------------------------------------
    // Markdown: section-based parsing (H1/H2 = one node, body = content)
    // -----------------------------------------------------------------------

    #[test]
    fn markdown_extracts_h1_h2_as_sections() {
        // H3 (Subsection) must NOT generate its own node — it is body of Section.
        let content = "# Title\n\nSome text\n\n## Section\n\nMore text\n\n### Subsection";
        let nodes = parse_markdown(Path::new("doc.md"), content, &branch());

        assert_eq!(nodes.len(), 2, "only H1 and H2 create nodes");
        assert_eq!(nodes[0].description, "Title");
        assert_eq!(nodes[1].description, "Section");

        // Levels stored in ext_data
        assert_eq!(nodes[0].ext_data.as_ref().unwrap()["level"], 1);
        assert_eq!(nodes[1].ext_data.as_ref().unwrap()["level"], 2);

        // H3 sub-heading is part of Section's content
        let section_content = nodes[1].ext_data.as_ref().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(
            section_content.contains("### Subsection"),
            "H3 should appear in H2 section content"
        );
    }

    #[test]
    fn markdown_heading_requires_space() {
        let content = "#NoSpace\n# Has Space";
        let nodes = parse_markdown(Path::new("doc.md"), content, &branch());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].description, "Has Space");
    }

    #[test]
    fn markdown_heading_max_level() {
        // H6 opens a section; H7 is invalid → treated as body content.
        // But H6 > 2, so it is body too. Only H1/H2 create nodes.
        let content = "# Top\n###### H6 content\n####### H7 content";
        let nodes = parse_markdown(Path::new("doc.md"), content, &branch());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].description, "Top");
        let body = nodes[0].ext_data.as_ref().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(body.contains("H6 content"));
    }

    #[test]
    fn markdown_list_items_are_body_content() {
        // Lists under an H1 must appear in content, not as separate nodes.
        let content = "# Section\n- First item\n- Second item\n* Third item";
        let nodes = parse_markdown(Path::new("doc.md"), content, &branch());

        assert_eq!(nodes.len(), 1, "only one node for the H1 section");
        assert_eq!(nodes[0].description, "Section");

        let body = nodes[0].ext_data.as_ref().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(body.contains("First item"));
        assert!(body.contains("Second item"));
        assert!(body.contains("Third item"));
    }

    #[test]
    fn markdown_multiple_h2_sections() {
        let content = "# Doc\n\npreamble\n\n## Section A\n- item A\n## Section B\n- item B";
        let nodes = parse_markdown(Path::new("doc.md"), content, &branch());

        // H1 + H2 A + H2 B = 3 nodes
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].description, "Doc");
        assert_eq!(nodes[1].description, "Section A");
        assert_eq!(nodes[2].description, "Section B");

        let body_a = nodes[1].ext_data.as_ref().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(body_a.contains("item A"));
        assert!(!body_a.contains("item B"));
    }

    #[test]
    fn markdown_orphan_content_before_first_heading_discarded() {
        // Content before the first H1/H2 produces no node.
        let content = "some preamble\n# First heading\nbody";
        let nodes = parse_markdown(Path::new("doc.md"), content, &branch());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].description, "First heading");
    }

    #[test]
    fn markdown_all_nodes_tagged_with_source() {
        let content = "# Heading\n- Item\n## Sub\ntext";
        let nodes = parse_markdown(Path::new("doc.md"), content, &branch());
        for node in &nodes {
            let ext = node.ext_data.as_ref().unwrap();
            assert_eq!(ext["source"], "documentation");
            assert_eq!(ext["doc_type"], "markdown");
            assert_eq!(ext["element"], "section");
        }
    }

    #[test]
    fn markdown_empty_content() {
        let content = "";
        let nodes = parse_markdown(Path::new("empty.md"), content, &branch());
        assert!(nodes.is_empty());
    }

    #[test]
    fn markdown_prose_only_no_structured_content() {
        // No H1/H2 → no nodes.
        let content = "This is just a paragraph.\nWith no headings or lists.";
        let nodes = parse_markdown(Path::new("prose.md"), content, &branch());
        assert!(nodes.is_empty());
    }

    // -----------------------------------------------------------------------
    // JSON Schema: basic
    // -----------------------------------------------------------------------

    #[test]
    fn json_schema_extracts_title_and_properties() {
        let content = r#"{
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "User",
            "description": "A user account",
            "type": "object",
            "required": ["id", "email"],
            "properties": {
                "id": {"type": "integer", "description": "Unique identifier"},
                "email": {"type": "string", "description": "Email address"},
                "name": {"type": "string"}
            }
        }"#;

        let nodes = parse_json_schema(Path::new("user.json"), content, &branch()).unwrap();

        // 1 schema node + 3 property nodes
        assert_eq!(nodes.len(), 4);
        assert!(nodes[0].description.contains("User"));
        assert!(nodes[0].description.contains("A user account"));

        // Check properties
        let id_node = nodes.iter().find(|n| n.description.contains("id")).unwrap();
        assert!(id_node.description.contains("integer"));
        assert!(id_node.description.contains("required"));

        let email_node = nodes
            .iter()
            .find(|n| n.description.contains("email"))
            .unwrap();
        assert!(email_node.description.contains("required"));

        let name_node = nodes
            .iter()
            .find(|n| n.description.contains("name") && !n.description.contains("User"))
            .unwrap();
        assert!(!name_node.description.contains("required"));
    }

    #[test]
    fn json_schema_extracts_definitions() {
        let content = r#"{
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "API",
            "type": "object",
            "definitions": {
                "Address": {
                    "type": "object",
                    "description": "A postal address"
                },
                "PhoneNumber": {
                    "type": "string"
                }
            }
        }"#;

        let nodes = parse_json_schema(Path::new("api.json"), content, &branch()).unwrap();

        // 1 schema + 2 definitions
        assert_eq!(nodes.len(), 3);

        let addr = nodes
            .iter()
            .find(|n| n.description.contains("Address"))
            .unwrap();
        assert!(addr.description.contains("A postal address"));

        let phone = nodes
            .iter()
            .find(|n| n.description.contains("PhoneNumber"))
            .unwrap();
        assert!(phone.description.contains("string"));
    }

    #[test]
    fn json_schema_extracts_defs_key() {
        let content = r#"{
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Modern",
            "type": "object",
            "$defs": {
                "Color": {"type": "string", "description": "A color value"}
            }
        }"#;

        let nodes = parse_json_schema(Path::new("modern.json"), content, &branch()).unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes[1].description.contains("Color"));
    }

    #[test]
    fn json_schema_not_a_schema() {
        let content = r#"{"name": "John", "age": 30}"#;
        let nodes = parse_json_schema(Path::new("data.json"), content, &branch()).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn json_schema_invalid_json() {
        let result = parse_json_schema(Path::new("bad.json"), "not json", &branch());
        assert!(result.is_err());
    }

    #[test]
    fn json_schema_not_object() {
        let result = parse_json_schema(Path::new("array.json"), "[1,2,3]", &branch());
        assert!(result.is_err());
    }

    #[test]
    fn json_schema_all_nodes_tagged_with_source() {
        let content = r#"{
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "T",
            "type": "object",
            "properties": {"x": {"type": "string"}}
        }"#;
        let nodes = parse_json_schema(Path::new("t.json"), content, &branch()).unwrap();
        for node in &nodes {
            let ext = node.ext_data.as_ref().unwrap();
            assert_eq!(ext["source"], "documentation");
            assert_eq!(ext["doc_type"], "json_schema");
        }
    }

    // -----------------------------------------------------------------------
    // OpenAPI: basic
    // -----------------------------------------------------------------------

    #[test]
    fn openapi_extracts_api_info_and_endpoints() {
        let content = r#"
openapi: '3.0.0'
info:
  title: Pet Store
  version: '1.0.0'
paths:
  /pets:
    get:
      summary: List all pets
      operationId: listPets
      tags:
        - pets
      responses:
        '200':
          description: A list of pets
    post:
      summary: Create a pet
      operationId: createPet
      responses:
        '201':
          description: Pet created
  /pets/{petId}:
    get:
      summary: Get a pet by ID
      operationId: showPetById
      responses:
        '200':
          description: A single pet
        '404':
          description: Pet not found
"#;

        let nodes = parse_openapi(Path::new("api.yaml"), content, &branch()).unwrap();

        // 1 API node + 3 endpoint nodes
        assert_eq!(nodes.len(), 4);

        let api_node = &nodes[0];
        assert!(api_node.description.contains("Pet Store"));
        assert!(api_node.description.contains("v1.0.0"));

        // Check endpoints
        let get_pets = nodes
            .iter()
            .find(|n| n.description.contains("GET /pets") && !n.description.contains("{petId}"))
            .unwrap();
        assert!(get_pets.description.contains("List all pets"));

        let post_pets = nodes
            .iter()
            .find(|n| n.description.contains("POST /pets"))
            .unwrap();
        assert!(post_pets.description.contains("Create a pet"));

        let get_pet = nodes
            .iter()
            .find(|n| n.description.contains("GET /pets/{petId}"))
            .unwrap();
        assert!(get_pet.description.contains("Get a pet by ID"));

        // Check ext_data for endpoint
        let ext = get_pets.ext_data.as_ref().unwrap();
        assert_eq!(ext["source"], "documentation");
        assert_eq!(ext["operation_id"], "listPets");
        assert_eq!(ext["tags"], serde_json::json!(["pets"]));
        assert_eq!(ext["response_codes"], serde_json::json!(["200"]));
    }

    #[test]
    fn openapi_extracts_component_schemas() {
        let content = r#"
openapi: '3.0.0'
info:
  title: Test API
  version: '1.0'
paths: {}
components:
  schemas:
    Pet:
      type: object
      description: A pet in the store
    Error:
      type: object
      description: An error response
"#;

        let nodes = parse_openapi(Path::new("api.yml"), content, &branch()).unwrap();

        // 1 API + 2 schemas
        assert_eq!(nodes.len(), 3);

        let pet = nodes
            .iter()
            .find(|n| n.description.contains("Pet"))
            .unwrap();
        assert!(pet.description.contains("A pet in the store"));

        let error = nodes
            .iter()
            .find(|n| n.description.contains("Error"))
            .unwrap();
        assert!(error.description.contains("An error response"));
    }

    #[test]
    fn openapi_swagger_2_definitions() {
        let content = r#"
swagger: '2.0'
info:
  title: Legacy API
  version: '0.1'
paths:
  /users:
    get:
      summary: List users
      responses:
        '200':
          description: OK
definitions:
  User:
    type: object
    description: A user object
"#;

        let nodes = parse_openapi(Path::new("legacy.yaml"), content, &branch()).unwrap();

        // 1 API + 1 endpoint + 1 definition
        assert_eq!(nodes.len(), 3);

        let user = nodes
            .iter()
            .find(|n| n.description.contains("User"))
            .unwrap();
        assert!(user.description.contains("A user object"));
    }

    #[test]
    fn openapi_not_an_api_spec() {
        let content = "name: John\nage: 30";
        let nodes = parse_openapi(Path::new("data.yaml"), content, &branch()).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn openapi_invalid_yaml() {
        let result = parse_openapi(Path::new("bad.yaml"), "{{invalid yaml", &branch());
        assert!(result.is_err());
    }

    #[test]
    fn openapi_not_mapping() {
        let result = parse_openapi(Path::new("list.yaml"), "- item1\n- item2", &branch());
        assert!(result.is_err());
    }

    #[test]
    fn openapi_all_nodes_tagged_with_source() {
        let content = r#"
openapi: '3.0.0'
info:
  title: T
  version: '1'
paths:
  /x:
    get:
      summary: X
      responses:
        '200':
          description: OK
"#;
        let nodes = parse_openapi(Path::new("api.yaml"), content, &branch()).unwrap();
        for node in &nodes {
            let ext = node.ext_data.as_ref().unwrap();
            assert_eq!(ext["source"], "documentation");
            assert_eq!(ext["doc_type"], "openapi");
        }
    }

    #[test]
    fn openapi_endpoint_without_summary() {
        let content = r#"
openapi: '3.0.0'
info:
  title: Minimal
  version: '1'
paths:
  /health:
    get:
      responses:
        '200':
          description: OK
"#;
        let nodes = parse_openapi(Path::new("api.yaml"), content, &branch()).unwrap();
        let endpoint = nodes
            .iter()
            .find(|n| n.description.contains("GET /health"))
            .unwrap();
        // No summary means just method + path
        assert_eq!(endpoint.description, "Endpoint: GET /health");
    }

    // -----------------------------------------------------------------------
    // Node properties
    // -----------------------------------------------------------------------

    #[test]
    fn all_nodes_are_facts_with_info_weight() {
        let md = "# Title\n- Item";
        let md_nodes = parse_markdown(Path::new("doc.md"), md, &branch());
        for node in &md_nodes {
            assert_eq!(node.nature, KnowledgeNature::Fact);
            assert_eq!(node.weight, KnowledgeWeight::Info);
            assert!((node.confidence - 1.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn documentation_result_contains_correct_path() {
        let result = parse_documentation(Path::new("docs/README.md"), "# Hi", &branch()).unwrap();
        assert_eq!(result.path, Path::new("docs/README.md"));
    }
}
