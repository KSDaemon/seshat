//! Stub Python parser.
//!
//! Full tree-sitter-based parsing is implemented in US-006.
//! This stub provides the structural scaffolding and graceful degradation.

use std::path::Path;

use seshat_core::{Language, LanguageIR, ProjectFile, PythonIR};

use super::Parser;
use crate::ScanError;

/// Parser for Python source files.
pub struct PythonParser;

impl Parser for PythonParser {
    fn parse(&self, path: &Path, _source: &str) -> Result<ProjectFile, ScanError> {
        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::Python,
            content_hash: String::new(), // filled by parse_file
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
        })
    }
}
