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
use tree_sitter::Node;

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

// ---------------------------------------------------------------------------
// Shared tree-sitter helpers used by all language parsers.
// ---------------------------------------------------------------------------

/// Extract UTF-8 text for a tree-sitter node from source bytes.
///
/// Returns `""` if the node's byte range is not valid UTF-8.
pub(super) fn node_text<'a>(node: &Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

/// Find the first direct child of `node` whose `kind()` equals `kind`.
pub(super) fn find_child_node<'a>(node: &'a Node, kind: &str) -> Option<Node<'a>> {
    (0..node.child_count() as u32)
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == kind)
}

/// Find the first child of `kind` and return its text as an owned `String`.
pub(super) fn find_child_text(node: &Node, kind: &str, source: &[u8]) -> Option<String> {
    find_child_node(node, kind).map(|n| node_text(&n, source).to_string())
}

/// Check whether `node` has any direct child whose `kind()` equals `kind`.
pub(super) fn has_child_kind(node: &Node, kind: &str) -> bool {
    find_child_node(node, kind).is_some()
}

/// Extract the string content from a `string` node (strips surrounding quotes).
///
/// Shared between the TypeScript and JavaScript parsers for ESM import paths.
pub(super) fn extract_string_value(node: &Node, source: &[u8]) -> Option<String> {
    let string_node = find_child_node(node, "string")?;
    let fragment = find_child_node(&string_node, "string_fragment")?;
    Some(node_text(&fragment, source).to_string())
}

/// Extract names from an ESM `import_clause` node.
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn extract_import_names(clause: &Node, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();

    for i in 0..(clause.child_count() as u32) {
        let Some(child) = clause.child(i) else {
            continue;
        };
        match child.kind() {
            "identifier" => {
                // Default import: `import Foo from ...`
                names.push(node_text(&child, source).to_string());
            }
            "named_imports" => {
                // Named imports: `import { Foo, Bar } from ...`
                for j in 0..(child.child_count() as u32) {
                    if let Some(spec) = child.child(j) {
                        if spec.kind() == "import_specifier" {
                            if let Some(name_node) = spec.child(0) {
                                names.push(node_text(&name_node, source).to_string());
                            }
                        }
                    }
                }
            }
            "namespace_import" => {
                // Namespace import: `import * as ns from ...`
                if let Some(alias) = find_child_text(&child, "identifier", source) {
                    names.push(format!("* as {alias}"));
                } else {
                    names.push("*".to_string());
                }
            }
            _ => {}
        }
    }

    names
}

/// Extract exports and functions from `export const/let/var` (lexical) declarations.
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn extract_exported_lexical(
    node: &Node,
    source: &[u8],
    exports: &mut Vec<seshat_core::Export>,
    functions: &mut Vec<seshat_core::Function>,
    is_default: bool,
    line: usize,
) {
    for i in 0..(node.child_count() as u32) {
        let Some(child) = node.child(i) else { continue };
        if child.kind() == "variable_declarator" {
            let name = find_child_text(&child, "identifier", source).unwrap_or_default();

            // Check if the value is an arrow function or function expression
            let is_func = has_child_kind(&child, "arrow_function")
                || has_child_kind(&child, "function_expression");

            if is_func {
                let is_async = child_has_async_value(&child, source);
                functions.push(seshat_core::Function {
                    name: name.clone(),
                    is_public: true,
                    is_async,
                    line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                });
            }

            if !name.is_empty() {
                exports.push(seshat_core::Export {
                    name,
                    is_default,
                    is_type_only: false,
                    line,
                });
            }
        }
    }
}

/// Extract a `function_declaration` node into a [`Function`].
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn extract_function_declaration(node: &Node, source: &[u8]) -> seshat_core::Function {
    let name = find_child_text(node, "identifier", source).unwrap_or_default();
    let is_async = has_child_kind(node, "async");

    seshat_core::Function {
        name,
        is_public: false, // will be set to true by export handling
        is_async,
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    }
}

/// Check if a `variable_declarator` value child (arrow_function or
/// function_expression) is async.
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn child_has_async_value(declarator: &Node, source: &[u8]) -> bool {
    for i in 0..(declarator.child_count() as u32) {
        if let Some(child) = declarator.child(i) {
            if child.kind() == "arrow_function" || child.kind() == "function_expression" {
                return has_child_kind(&child, "async");
            }
        }
    }
    // Fallback: check the whole declarator text
    node_text(declarator, source).contains("async")
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
