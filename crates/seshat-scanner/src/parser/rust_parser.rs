//! Stub Rust parser.
//!
//! Full tree-sitter-based parsing is implemented in US-003.
//! This stub provides the structural scaffolding and graceful degradation.

use std::path::Path;

use seshat_core::{Language, LanguageIR, ProjectFile, RustIR};

use super::Parser;
use crate::ScanError;

/// Parser for Rust source files.
pub struct RustParser;

impl Parser for RustParser {
    fn parse(&self, path: &Path, _source: &str) -> Result<ProjectFile, ScanError> {
        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::Rust,
            content_hash: String::new(), // filled by parse_file
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        })
    }
}
