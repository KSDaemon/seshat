//! Integration tests for module structure detection and dependency graph.
//!
//! Uses the module_project fixture which has multiple directories with
//! cross-module imports to verify module detection, DependsOn edges,
//! and PartOf hierarchy edges.

use std::path::{Path, PathBuf};

use seshat_core::{BranchId, EdgeType, KnowledgeNature, KnowledgeWeight, Language};
use seshat_scanner::{build_module_graph, parse_file};

/// Parse all fixture files and build a module graph.
fn build_fixture_graph() -> seshat_scanner::ModuleGraph {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("module_project");

    let files_to_parse = vec![
        ("src/main.rs", Language::Rust),
        ("src/handlers/api.rs", Language::Rust),
        ("src/handlers/web.rs", Language::Rust),
        ("src/models/user.rs", Language::Rust),
        ("src/utils/format.rs", Language::Rust),
    ];

    let mut parsed_files = Vec::new();
    for (rel_path, lang) in &files_to_parse {
        let full_path = fixture_root.join(rel_path);
        let source = std::fs::read_to_string(&full_path)
            .unwrap_or_else(|e| panic!("Failed to read {rel_path}: {e}"));
        let pf = parse_file(&full_path, &source, *lang);
        parsed_files.push(pf);
    }

    build_module_graph(&fixture_root, &parsed_files, &BranchId::from("main"))
}

// -------------------------------------------------------------------------
// Module detection
// -------------------------------------------------------------------------

#[test]
fn fixture_detects_all_modules() {
    let graph = build_fixture_graph();

    // Expected modules: src, src/handlers, src/models, src/utils
    assert_eq!(graph.modules.len(), 4);
    assert!(graph.modules.contains_key(&PathBuf::from("src")));
    assert!(graph.modules.contains_key(&PathBuf::from("src/handlers")));
    assert!(graph.modules.contains_key(&PathBuf::from("src/models")));
    assert!(graph.modules.contains_key(&PathBuf::from("src/utils")));
}

#[test]
fn fixture_module_file_counts() {
    let graph = build_fixture_graph();

    assert_eq!(graph.modules[&PathBuf::from("src")].files.len(), 1); // main.rs
    assert_eq!(graph.modules[&PathBuf::from("src/handlers")].files.len(), 2); // api.rs, web.rs
    assert_eq!(graph.modules[&PathBuf::from("src/models")].files.len(), 1); // user.rs
    assert_eq!(graph.modules[&PathBuf::from("src/utils")].files.len(), 1); // format.rs
}

#[test]
fn fixture_all_modules_are_rust() {
    let graph = build_fixture_graph();

    for info in graph.modules.values() {
        assert!(info.languages.contains("rust"));
        assert_eq!(info.languages.len(), 1);
    }
}

// -------------------------------------------------------------------------
// Knowledge nodes
// -------------------------------------------------------------------------

#[test]
fn fixture_creates_correct_number_of_nodes() {
    let graph = build_fixture_graph();
    assert_eq!(graph.nodes.len(), 4); // 4 modules
}

#[test]
fn fixture_nodes_are_facts() {
    let graph = build_fixture_graph();
    for node in &graph.nodes {
        assert_eq!(node.nature, KnowledgeNature::Fact);
        assert_eq!(node.weight, KnowledgeWeight::Info);
        assert_eq!(node.confidence, 1.0);
        assert_eq!(node.branch_id, BranchId::from("main"));
    }
}

#[test]
fn fixture_nodes_have_ext_data() {
    let graph = build_fixture_graph();
    for node in &graph.nodes {
        let ext = node.ext_data.as_ref().expect("should have ext_data");
        assert_eq!(ext["source"], "module_structure");
        assert!(ext["module_path"].is_string());
        assert!(ext["file_count"].is_number());
        assert!(ext["languages"].is_array());
        assert!(ext["files"].is_array());
    }
}

// -------------------------------------------------------------------------
// DependsOn edges
// -------------------------------------------------------------------------

#[test]
fn fixture_has_depends_on_edges() {
    let graph = build_fixture_graph();
    let depends_on: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::DependsOn)
        .collect();

    // Expected dependencies:
    // src/main.rs imports from crate::handlers and crate::models → src depends on src/handlers, src/models
    // src/handlers/api.rs imports from crate::models and crate::utils → src/handlers depends on src/models, src/utils
    // src/handlers/web.rs imports from crate::models → src/handlers depends on src/models (deduplicated)
    // So: src → src/handlers, src → src/models, src/handlers → src/models, src/handlers → src/utils
    assert!(
        depends_on.len() >= 3,
        "Expected at least 3 DependsOn edges, got {}",
        depends_on.len()
    );
}

#[test]
fn fixture_handlers_depends_on_models() {
    let graph = build_fixture_graph();

    let deps = graph.dependencies_of(Path::new("src/handlers"));
    let dep_strs: Vec<String> = deps
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(
        dep_strs.contains(&"src/models".to_string()),
        "src/handlers should depend on src/models, got: {dep_strs:?}"
    );
}

#[test]
fn fixture_handlers_depends_on_utils() {
    let graph = build_fixture_graph();

    let deps = graph.dependencies_of(Path::new("src/handlers"));
    let dep_strs: Vec<String> = deps
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(
        dep_strs.contains(&"src/utils".to_string()),
        "src/handlers should depend on src/utils, got: {dep_strs:?}"
    );
}

#[test]
fn fixture_utils_has_no_dependencies() {
    let graph = build_fixture_graph();

    let deps = graph.dependencies_of(Path::new("src/utils"));
    assert!(
        deps.is_empty(),
        "src/utils should have no dependencies, got: {deps:?}"
    );
}

#[test]
fn fixture_models_depended_on_by_handlers() {
    let graph = build_fixture_graph();

    let dependents = graph.dependents_of(Path::new("src/models"));
    let dep_strs: Vec<String> = dependents
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(
        dep_strs.contains(&"src/handlers".to_string()),
        "src/models should be depended on by src/handlers, got: {dep_strs:?}"
    );
}

// -------------------------------------------------------------------------
// PartOf edges
// -------------------------------------------------------------------------

#[test]
fn fixture_has_part_of_edges() {
    let graph = build_fixture_graph();
    let part_of: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::PartOf)
        .collect();

    // src/handlers PartOf src
    // src/models PartOf src
    // src/utils PartOf src
    assert_eq!(
        part_of.len(),
        3,
        "Expected 3 PartOf edges, got {}",
        part_of.len()
    );
}

#[test]
fn fixture_part_of_edges_point_to_parent() {
    let graph = build_fixture_graph();
    let part_of: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::PartOf)
        .collect();

    // All PartOf edges should point from child to parent (src).
    for edge in &part_of {
        let metadata = edge.metadata.as_ref().expect("should have metadata");
        let parent = metadata["parent_module"].as_str().unwrap();
        assert_eq!(parent, "src", "All submodules should be PartOf 'src'");
    }
}

// -------------------------------------------------------------------------
// No self-dependency edges
// -------------------------------------------------------------------------

#[test]
fn fixture_no_self_dependency_edges() {
    let graph = build_fixture_graph();
    for edge in &graph.edges {
        if edge.edge_type == EdgeType::DependsOn {
            assert_ne!(
                edge.source_id, edge.target_id,
                "DependsOn edge should not be self-referential"
            );
        }
    }
}

// -------------------------------------------------------------------------
// Edge weight and metadata
// -------------------------------------------------------------------------

#[test]
fn fixture_all_edges_have_weight() {
    let graph = build_fixture_graph();
    for edge in &graph.edges {
        assert_eq!(edge.weight, 1.0);
    }
}

#[test]
fn fixture_all_edges_have_metadata() {
    let graph = build_fixture_graph();
    for edge in &graph.edges {
        assert!(edge.metadata.is_some(), "Edge should have metadata");
    }
}
