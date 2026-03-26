//! Stub TypeScript parser.
//!
//! Full tree-sitter-based parsing is implemented in US-004.
//! This stub provides the structural scaffolding and graceful degradation.

use std::path::Path;

use seshat_core::{Language, LanguageIR, ProjectFile, TypeScriptIR};

use super::Parser;
use crate::ScanError;

/// Parser for TypeScript source files.
pub struct TypeScriptParser;

impl Parser for TypeScriptParser {
    fn parse(&self, path: &Path, _source: &str) -> Result<ProjectFile, ScanError> {
        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::TypeScript,
            content_hash: String::new(), // filled by parse_file
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
        })
    }
}
