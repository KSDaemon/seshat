//! Test factory functions for creating common types.
//!
//! Available behind the `test-helpers` feature flag. Other crates can use
//! these via `seshat-core = { ..., features = ["test-helpers"] }` in
//! `[dev-dependencies]`.

use crate::ids::{BranchId, NodeId};
use crate::ir::{Language, LanguageIR, ProjectFile, RustIR};
use crate::knowledge::{KnowledgeNature, KnowledgeNode, KnowledgeWeight};
use std::path::PathBuf;

/// Create a `KnowledgeNode` with the given nature and confidence.
pub fn make_knowledge_node(nature: KnowledgeNature, confidence: f64) -> KnowledgeNode {
    let weight = if confidence > 0.85 {
        KnowledgeWeight::Strong
    } else if confidence > 0.50 {
        KnowledgeWeight::Moderate
    } else if confidence > 0.20 {
        KnowledgeWeight::Weak
    } else {
        KnowledgeWeight::Info
    };

    KnowledgeNode {
        id: NodeId(0),
        branch_id: BranchId::from("main"),
        nature,
        weight,
        confidence,
        adoption_count: 0,
        total_count: 0,
        description: String::new(),
        ext_data: None,
    }
}

/// Create a minimal `ProjectFile` for the given language.
pub fn make_project_file(language: Language) -> ProjectFile {
    let language_ir = match language {
        Language::Rust => LanguageIR::Rust(RustIR::default()),
        Language::TypeScript => LanguageIR::TypeScript(crate::ir::TypeScriptIR::default()),
        Language::JavaScript => LanguageIR::JavaScript(crate::ir::JavaScriptIR::default()),
        Language::Python => LanguageIR::Python(crate::ir::PythonIR::default()),
    };

    ProjectFile {
        path: PathBuf::from("test.rs"),
        language,
        content_hash: String::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        functions: Vec::new(),
        types: Vec::new(),
        dependencies_used: Vec::new(),
        language_ir,
        file_doc: None,
    }
}
