//! Stub JavaScript parser.
//!
//! Full tree-sitter-based parsing is implemented in US-005.
//! This stub provides the structural scaffolding and graceful degradation.

use std::path::Path;

use seshat_core::{JavaScriptIR, Language, LanguageIR, ProjectFile};

use super::Parser;
use crate::ScanError;

/// Parser for JavaScript source files.
pub struct JavaScriptParser;

impl Parser for JavaScriptParser {
    fn parse(&self, path: &Path, _source: &str) -> Result<ProjectFile, ScanError> {
        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::JavaScript,
            content_hash: String::new(), // filled by parse_file
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR::default()),
        })
    }
}
