//! Integration tests for documentation ingestion.
//!
//! These tests exercise the full `parse_documentation` pipeline against
//! fixture files in `tests/fixtures/docs_project/`.

use std::path::Path;

use seshat_core::{BranchId, KnowledgeNature, KnowledgeWeight};
use seshat_scanner::{DocType, parse_documentation};

fn branch() -> BranchId {
    BranchId::from("test-branch")
}

fn read_fixture(name: &str) -> String {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("docs_project");
    std::fs::read_to_string(fixture_dir.join(name))
        .unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"))
}

// ---------------------------------------------------------------------------
// Markdown integration tests
// ---------------------------------------------------------------------------

#[test]
fn markdown_readme_produces_correct_node_count() {
    let content = read_fixture("README.md");
    let result = parse_documentation(Path::new("README.md"), &content, &branch()).unwrap();

    assert_eq!(result.doc_type, DocType::Markdown);
    assert_eq!(result.path, Path::new("README.md"));

    // README.md has H1/H2 sections only (H3 "Prerequisites" is body of "Getting Started"):
    //   H1: Project Name
    //   H2: Architecture
    //   H2: Getting Started
    //   H2: API Conventions
    // Total: 4 section nodes
    assert_eq!(result.nodes.len(), 4);

    // All nodes are sections
    for node in &result.nodes {
        assert_eq!(
            node.ext_data.as_ref().unwrap()["element"],
            "section",
            "all markdown nodes must be section elements"
        );
    }
}

#[test]
fn markdown_headings_have_correct_levels() {
    let content = read_fixture("README.md");
    let result = parse_documentation(Path::new("README.md"), &content, &branch()).unwrap();

    // H1: Project Name
    assert_eq!(result.nodes[0].description, "Project Name");
    assert_eq!(result.nodes[0].ext_data.as_ref().unwrap()["level"], 1);

    // H2: Architecture
    assert_eq!(result.nodes[1].description, "Architecture");
    assert_eq!(result.nodes[1].ext_data.as_ref().unwrap()["level"], 2);

    // H2: Getting Started (H3 "Prerequisites" is body content, not a separate node)
    assert_eq!(result.nodes[2].description, "Getting Started");
    assert_eq!(result.nodes[2].ext_data.as_ref().unwrap()["level"], 2);

    // H2: API Conventions
    assert_eq!(result.nodes[3].description, "API Conventions");
    assert_eq!(result.nodes[3].ext_data.as_ref().unwrap()["level"], 2);
}

#[test]
fn markdown_list_items_reference_parent_heading() {
    let content = read_fixture("README.md");
    let result = parse_documentation(Path::new("README.md"), &content, &branch()).unwrap();

    // "Architecture" section's content should include all four list items.
    let arch_node = result
        .nodes
        .iter()
        .find(|n| n.description == "Architecture")
        .expect("Architecture section must exist");

    let body = arch_node.ext_data.as_ref().unwrap()["content"]
        .as_str()
        .unwrap();

    assert!(
        body.contains("Core layer"),
        "Architecture body should contain 'Core layer'"
    );
    assert!(
        body.contains("Storage layer"),
        "Architecture body should contain 'Storage layer'"
    );
    assert!(
        body.contains("Scanner layer"),
        "Architecture body should contain 'Scanner layer'"
    );
    assert!(
        body.contains("Graph layer"),
        "Architecture body should contain 'Graph layer'"
    );
}

#[test]
fn markdown_all_nodes_tagged_documentation_source() {
    let content = read_fixture("README.md");
    let result = parse_documentation(Path::new("README.md"), &content, &branch()).unwrap();

    for node in &result.nodes {
        assert_eq!(node.nature, KnowledgeNature::Fact);
        assert_eq!(node.weight, KnowledgeWeight::Info);
        let ext = node.ext_data.as_ref().expect("ext_data should be set");
        assert_eq!(ext["source"], "documentation");
        assert_eq!(ext["doc_type"], "markdown");
        assert_eq!(ext["element"], "section");
    }
}

#[test]
fn markdown_contributing_guide() {
    let content = read_fixture("CONTRIBUTING.md");
    let result = parse_documentation(Path::new("CONTRIBUTING.md"), &content, &branch()).unwrap();

    assert_eq!(result.doc_type, DocType::Markdown);

    // CONTRIBUTING.md: H1 "Contributing" + H2 "Code Style" + H2 "Pull Request Process" = 3 nodes
    assert_eq!(result.nodes.len(), 3);
    let descriptions: Vec<&str> = result
        .nodes
        .iter()
        .map(|n| n.description.as_str())
        .collect();
    assert!(descriptions.contains(&"Contributing"));
    assert!(descriptions.contains(&"Code Style"));
    assert!(descriptions.contains(&"Pull Request Process"));
}

// ---------------------------------------------------------------------------
// JSON Schema integration tests
// ---------------------------------------------------------------------------

#[test]
fn json_schema_user_produces_correct_nodes() {
    let content = read_fixture("user-schema.json");
    let result = parse_documentation(Path::new("user-schema.json"), &content, &branch()).unwrap();

    assert_eq!(result.doc_type, DocType::JsonSchema);

    // 1 schema root + 5 properties + 2 definitions = 8 nodes
    assert_eq!(result.nodes.len(), 8);
}

#[test]
fn json_schema_root_node_has_title_and_description() {
    let content = read_fixture("user-schema.json");
    let result = parse_documentation(Path::new("user-schema.json"), &content, &branch()).unwrap();

    let root = &result.nodes[0];
    assert!(root.description.contains("User"));
    assert!(root.description.contains("A user account in the system"));
    assert_eq!(root.ext_data.as_ref().unwrap()["element"], "schema");
}

#[test]
fn json_schema_required_properties_flagged() {
    let content = read_fixture("user-schema.json");
    let result = parse_documentation(Path::new("user-schema.json"), &content, &branch()).unwrap();

    // id, email, role should be marked as required
    let id_node = result
        .nodes
        .iter()
        .find(|n| {
            n.ext_data
                .as_ref()
                .map(|e| e["property_name"] == "id")
                .unwrap_or(false)
        })
        .unwrap();
    assert_eq!(id_node.ext_data.as_ref().unwrap()["required"], true);

    // name should not be required
    let name_node = result
        .nodes
        .iter()
        .find(|n| {
            n.ext_data
                .as_ref()
                .map(|e| e["property_name"] == "name")
                .unwrap_or(false)
        })
        .unwrap();
    assert_eq!(name_node.ext_data.as_ref().unwrap()["required"], false);
}

#[test]
fn json_schema_definitions_extracted() {
    let content = read_fixture("user-schema.json");
    let result = parse_documentation(Path::new("user-schema.json"), &content, &branch()).unwrap();

    let definitions: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| {
            n.ext_data
                .as_ref()
                .map(|e| e["element"] == "definition")
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(definitions.len(), 2);

    let addr = definitions
        .iter()
        .find(|n| n.description.contains("Address"))
        .unwrap();
    assert!(addr.description.contains("A postal address"));
}

#[test]
fn json_schema_all_nodes_tagged_documentation_source() {
    let content = read_fixture("user-schema.json");
    let result = parse_documentation(Path::new("user-schema.json"), &content, &branch()).unwrap();

    for node in &result.nodes {
        assert_eq!(node.nature, KnowledgeNature::Fact);
        assert_eq!(node.weight, KnowledgeWeight::Info);
        let ext = node.ext_data.as_ref().expect("ext_data should be set");
        assert_eq!(ext["source"], "documentation");
        assert_eq!(ext["doc_type"], "json_schema");
    }
}

// ---------------------------------------------------------------------------
// OpenAPI integration tests
// ---------------------------------------------------------------------------

#[test]
fn openapi_spec_produces_correct_nodes() {
    let content = read_fixture("api-spec.yaml");
    let result = parse_documentation(Path::new("api-spec.yaml"), &content, &branch()).unwrap();

    assert_eq!(result.doc_type, DocType::OpenApi);

    // 1 API info + 6 endpoints + 3 component schemas = 10 nodes
    assert_eq!(result.nodes.len(), 10);
}

#[test]
fn openapi_api_info_extracted() {
    let content = read_fixture("api-spec.yaml");
    let result = parse_documentation(Path::new("api-spec.yaml"), &content, &branch()).unwrap();

    let api_node = &result.nodes[0];
    assert!(api_node.description.contains("Seshat API"));
    assert!(api_node.description.contains("v0.1.0"));
    assert_eq!(api_node.ext_data.as_ref().unwrap()["element"], "api");
}

#[test]
fn openapi_endpoints_extracted_with_methods() {
    let content = read_fixture("api-spec.yaml");
    let result = parse_documentation(Path::new("api-spec.yaml"), &content, &branch()).unwrap();

    let endpoints: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| {
            n.ext_data
                .as_ref()
                .map(|e| e["element"] == "endpoint")
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(endpoints.len(), 6);

    // Check specific endpoints exist
    let methods_and_paths: Vec<_> = endpoints
        .iter()
        .map(|n| {
            let ext = n.ext_data.as_ref().unwrap();
            format!(
                "{} {}",
                ext["method"].as_str().unwrap(),
                ext["path"].as_str().unwrap()
            )
        })
        .collect();

    assert!(methods_and_paths.contains(&"GET /projects".to_string()));
    assert!(methods_and_paths.contains(&"POST /projects".to_string()));
    assert!(methods_and_paths.contains(&"GET /projects/{projectId}".to_string()));
    assert!(methods_and_paths.contains(&"DELETE /projects/{projectId}".to_string()));
    assert!(methods_and_paths.contains(&"POST /projects/{projectId}/scan".to_string()));
    assert!(methods_and_paths.contains(&"GET /nodes".to_string()));
}

#[test]
fn openapi_endpoint_has_operation_id_and_tags() {
    let content = read_fixture("api-spec.yaml");
    let result = parse_documentation(Path::new("api-spec.yaml"), &content, &branch()).unwrap();

    let get_projects = result
        .nodes
        .iter()
        .find(|n| {
            n.ext_data
                .as_ref()
                .map(|e| e["operation_id"] == "listProjects")
                .unwrap_or(false)
        })
        .unwrap();

    let ext = get_projects.ext_data.as_ref().unwrap();
    assert_eq!(ext["tags"], serde_json::json!(["projects"]));
    assert_eq!(ext["response_codes"], serde_json::json!(["200"]));
}

#[test]
fn openapi_component_schemas_extracted() {
    let content = read_fixture("api-spec.yaml");
    let result = parse_documentation(Path::new("api-spec.yaml"), &content, &branch()).unwrap();

    let schemas: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| {
            n.ext_data
                .as_ref()
                .map(|e| e["element"] == "schema")
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(schemas.len(), 3);

    let schema_names: Vec<_> = schemas
        .iter()
        .map(|n| {
            n.ext_data.as_ref().unwrap()["schema_name"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();

    assert!(schema_names.contains(&"Project".to_string()));
    assert!(schema_names.contains(&"KnowledgeNode".to_string()));
    assert!(schema_names.contains(&"ScanResult".to_string()));
}

#[test]
fn openapi_all_nodes_tagged_documentation_source() {
    let content = read_fixture("api-spec.yaml");
    let result = parse_documentation(Path::new("api-spec.yaml"), &content, &branch()).unwrap();

    for node in &result.nodes {
        assert_eq!(node.nature, KnowledgeNature::Fact);
        assert_eq!(node.weight, KnowledgeWeight::Info);
        let ext = node.ext_data.as_ref().expect("ext_data should be set");
        assert_eq!(ext["source"], "documentation");
        assert_eq!(ext["doc_type"], "openapi");
    }
}

// ---------------------------------------------------------------------------
// Swagger 2.0 (legacy) integration tests
// ---------------------------------------------------------------------------

#[test]
fn swagger2_legacy_api_produces_correct_nodes() {
    let content = read_fixture("legacy-api.yml");
    let result = parse_documentation(Path::new("legacy-api.yml"), &content, &branch()).unwrap();

    assert_eq!(result.doc_type, DocType::OpenApi);

    // 1 API info + 2 endpoints + 2 definitions = 5 nodes
    assert_eq!(result.nodes.len(), 5);
}

#[test]
fn swagger2_definitions_extracted() {
    let content = read_fixture("legacy-api.yml");
    let result = parse_documentation(Path::new("legacy-api.yml"), &content, &branch()).unwrap();

    let schemas: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| {
            n.ext_data
                .as_ref()
                .map(|e| e["element"] == "schema")
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(schemas.len(), 2);

    let user = schemas
        .iter()
        .find(|n| n.description.contains("User"))
        .unwrap();
    assert!(user.description.contains("A user in the legacy system"));
}

// ---------------------------------------------------------------------------
// Cross-format consistency tests
// ---------------------------------------------------------------------------

#[test]
fn all_fixture_files_parse_without_errors() {
    let fixtures = [
        ("README.md", DocType::Markdown),
        ("CONTRIBUTING.md", DocType::Markdown),
        ("user-schema.json", DocType::JsonSchema),
        ("api-spec.yaml", DocType::OpenApi),
        ("legacy-api.yml", DocType::OpenApi),
    ];

    for (name, expected_type) in fixtures {
        let content = read_fixture(name);
        let result = parse_documentation(Path::new(name), &content, &branch())
            .unwrap_or_else(|e| panic!("Failed to parse {name}: {e}"));

        assert_eq!(result.doc_type, expected_type, "Wrong doc type for {name}");
        assert!(!result.nodes.is_empty(), "No nodes extracted from {name}");

        // Verify all nodes have source: documentation
        for node in &result.nodes {
            let ext = node.ext_data.as_ref().unwrap();
            assert_eq!(
                ext["source"], "documentation",
                "Missing source tag in {name}"
            );
        }
    }
}

#[test]
fn all_fixture_files_produce_fact_nodes_only() {
    let fixtures = [
        "README.md",
        "CONTRIBUTING.md",
        "user-schema.json",
        "api-spec.yaml",
        "legacy-api.yml",
    ];

    for name in fixtures {
        let content = read_fixture(name);
        let result = parse_documentation(Path::new(name), &content, &branch()).unwrap();

        for node in &result.nodes {
            assert_eq!(
                node.nature,
                KnowledgeNature::Fact,
                "Non-Fact node in {name}: {:?}",
                node.nature
            );
        }
    }
}
