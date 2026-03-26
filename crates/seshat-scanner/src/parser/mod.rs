//! Parser trait, language dispatch, and content hashing.
//!
//! The [`Parser`] trait defines the interface all language parsers implement.
//! [`parse_file`] dispatches to the correct parser based on [`Language`],
//! and computes the SHA-256 content hash in shared code so individual parsers
//! do not duplicate that logic.

mod javascript_parser;
mod python_parser;
mod rust_parser;
mod typescript_parser;

use std::path::Path;

use seshat_core::{Language, ProjectFile};
use sha2::{Digest, Sha256};

use crate::ScanError;
use javascript_parser::JavaScriptParser;
use python_parser::PythonParser;
use rust_parser::RustParser;
use typescript_parser::TypeScriptParser;

/// Common trait for all language parsers.
///
/// Implementations extract imports, exports, functions, types, and
/// language-specific IR from source code. Content hashing is handled
/// by the shared [`parse_file`] function — parsers should **not**
/// compute the hash themselves.
pub trait Parser {
    /// Parse source code at `path` into a [`ProjectFile`].
    ///
    /// The `content_hash` field on the returned `ProjectFile` may be left
    /// empty; [`parse_file`] will overwrite it with the SHA-256 hash.
    fn parse(&self, path: &Path, source: &str) -> Result<ProjectFile, ScanError>;
}

/// Compute the SHA-256 hex digest of the given source content.
pub fn content_hash(source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Parse a source file by dispatching to the appropriate language parser.
///
/// This is the primary entry point for parsing. It:
/// 1. Selects the parser for the given [`Language`].
/// 2. Delegates to the parser's [`Parser::parse`] method.
/// 3. Overwrites `content_hash` with a SHA-256 digest of `source`.
/// 4. On parser error, returns an empty [`ProjectFile`] with a
///    `tracing::warn` log (graceful degradation).
pub fn parse_file(path: &Path, source: &str, language: Language) -> ProjectFile {
    let parser: &dyn Parser = match language {
        Language::Rust => &RustParser,
        Language::TypeScript => &TypeScriptParser,
        Language::JavaScript => &JavaScriptParser,
        Language::Python => &PythonParser,
    };

    let hash = content_hash(source);

    match parser.parse(path, source) {
        Ok(mut pf) => {
            pf.content_hash = hash;
            pf
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Parser failed; returning empty IR");
            empty_project_file(path, language, hash)
        }
    }
}

/// Create an empty `ProjectFile` for graceful degradation.
fn empty_project_file(path: &Path, language: Language, hash: String) -> ProjectFile {
    use seshat_core::*;

    let language_ir = match language {
        Language::Rust => LanguageIR::Rust(RustIR::default()),
        Language::TypeScript => LanguageIR::TypeScript(TypeScriptIR::default()),
        Language::JavaScript => LanguageIR::JavaScript(JavaScriptIR::default()),
        Language::Python => LanguageIR::Python(PythonIR::default()),
    };

    ProjectFile {
        path: path.to_path_buf(),
        language,
        content_hash: hash,
        imports: Vec::new(),
        exports: Vec::new(),
        functions: Vec::new(),
        types: Vec::new(),
        dependencies_used: Vec::new(),
        language_ir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn content_hash_deterministic() {
        let a = content_hash("hello world");
        let b = content_hash("hello world");
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn content_hash_differs_for_different_input() {
        let a = content_hash("hello");
        let b = content_hash("world");
        assert_ne!(a, b);
    }

    #[test]
    fn content_hash_is_sha256_hex() {
        let h = content_hash("hello world");
        // SHA-256 produces 64 hex characters
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn dispatch_selects_rust_parser() {
        let path = PathBuf::from("src/main.rs");
        let pf = parse_file(&path, "fn main() {}", Language::Rust);
        assert_eq!(pf.language, Language::Rust);
        assert_eq!(pf.path, path);
        assert!(!pf.content_hash.is_empty());
        assert!(matches!(pf.language_ir, seshat_core::LanguageIR::Rust(_)));
    }

    #[test]
    fn dispatch_selects_typescript_parser() {
        let path = PathBuf::from("src/index.ts");
        let pf = parse_file(&path, "export const x = 1;", Language::TypeScript);
        assert_eq!(pf.language, Language::TypeScript);
        assert!(matches!(
            pf.language_ir,
            seshat_core::LanguageIR::TypeScript(_)
        ));
    }

    #[test]
    fn dispatch_selects_javascript_parser() {
        let path = PathBuf::from("src/index.js");
        let pf = parse_file(&path, "const x = 1;", Language::JavaScript);
        assert_eq!(pf.language, Language::JavaScript);
        assert!(matches!(
            pf.language_ir,
            seshat_core::LanguageIR::JavaScript(_)
        ));
    }

    #[test]
    fn dispatch_selects_python_parser() {
        let path = PathBuf::from("src/main.py");
        let pf = parse_file(&path, "def main(): pass", Language::Python);
        assert_eq!(pf.language, Language::Python);
        assert!(matches!(pf.language_ir, seshat_core::LanguageIR::Python(_)));
    }

    #[test]
    fn content_hash_computed_in_shared_code() {
        let source = "fn main() {}";
        let expected_hash = content_hash(source);
        let pf = parse_file(Path::new("test.rs"), source, Language::Rust);
        assert_eq!(pf.content_hash, expected_hash);
    }

    #[test]
    fn all_language_variants_dispatched() {
        // Ensure every Language variant has a parser (no panics, no unreachable)
        let languages = [
            Language::Rust,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
        ];
        for lang in languages {
            let pf = parse_file(Path::new("test"), "source", lang);
            assert_eq!(pf.language, lang);
            assert!(!pf.content_hash.is_empty());
        }
    }
}
