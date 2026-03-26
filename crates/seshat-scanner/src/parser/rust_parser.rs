//! Tree-sitter–based Rust parser.
//!
//! Extracts imports (`use`), functions, types (structs, enums, traits),
//! exports (pub items), mod declarations, derive macros, trait implementations,
//! and error types from Rust source files.

use std::path::Path;

use seshat_core::{
    DeriveUsage, Export, Function, Import, Language, LanguageIR, ProjectFile, RustIR, TraitImpl,
    TypeDef, TypeDefKind,
};
use tree_sitter::{Node, Parser as TsParser};

use super::{Parser, find_child_node, find_child_text, node_text};
use crate::ScanError;

/// Parser for Rust source files.
pub struct RustParser;

impl Parser for RustParser {
    fn parse(&self, path: &Path, source: &str) -> Result<ProjectFile, ScanError> {
        let mut ts_parser = TsParser::new();
        ts_parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|e| ScanError::ParseError {
                path: path.to_path_buf(),
                reason: format!("Failed to set tree-sitter language: {e}"),
            })?;

        let tree = ts_parser
            .parse(source, None)
            .ok_or_else(|| ScanError::ParseError {
                path: path.to_path_buf(),
                reason: "tree-sitter returned no parse tree".to_string(),
            })?;

        let root = tree.root_node();

        let mut imports = Vec::new();
        let mut exports = Vec::new();
        let mut functions = Vec::new();
        let mut types = Vec::new();
        let mut mod_declarations = Vec::new();
        let mut derive_macros: Vec<DeriveUsage> = Vec::new();
        let mut trait_implementations = Vec::new();
        let mut error_types = Vec::new();

        // Track pending derive attributes for the next item
        let mut pending_derives: Vec<(Vec<String>, usize)> = Vec::new();

        let source_bytes = source.as_bytes();

        for i in 0..(root.child_count() as u32) {
            let Some(child) = root.child(i) else { continue };
            match child.kind() {
                "use_declaration" => {
                    if let Some(imp) = extract_use_declaration(&child, source_bytes) {
                        imports.push(imp);
                    }
                }
                "function_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let func = extract_function(&child, source_bytes, is_pub);
                    if is_pub {
                        exports.push(Export {
                            name: func.name.clone(),
                            is_default: false,
                            is_type_only: false,
                            line: func.line,
                        });
                    }
                    // Apply any pending derives (shouldn't happen for functions, but drain anyway)
                    pending_derives.clear();
                    functions.push(func);
                }
                "struct_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let td = extract_type_def(&child, source_bytes, TypeDefKind::Struct, is_pub);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                        });
                    }
                    // Check if it's an error type
                    if td.name.contains("Error") {
                        error_types.push(td.name.clone());
                    }
                    // Apply pending derives
                    for (derives, line) in pending_derives.drain(..) {
                        derive_macros.push(DeriveUsage {
                            type_name: td.name.clone(),
                            derives,
                            line,
                        });
                    }
                    types.push(td);
                }
                "enum_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let td = extract_type_def(&child, source_bytes, TypeDefKind::Enum, is_pub);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                        });
                    }
                    if td.name.contains("Error") {
                        error_types.push(td.name.clone());
                    }
                    for (derives, line) in pending_derives.drain(..) {
                        derive_macros.push(DeriveUsage {
                            type_name: td.name.clone(),
                            derives,
                            line,
                        });
                    }
                    types.push(td);
                }
                "trait_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let td = extract_type_def(&child, source_bytes, TypeDefKind::Trait, is_pub);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                        });
                    }
                    pending_derives.clear();
                    types.push(td);
                }
                "type_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let td = extract_type_def(&child, source_bytes, TypeDefKind::TypeAlias, is_pub);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                        });
                    }
                    pending_derives.clear();
                    types.push(td);
                }
                "impl_item" => {
                    if let Some(ti) = extract_impl(&child, source_bytes) {
                        trait_implementations.push(ti);
                    }
                    // Also extract methods from impl blocks
                    extract_impl_functions(&child, source_bytes, &mut functions, &mut exports);
                    pending_derives.clear();
                }
                "mod_item" => {
                    if let Some(name) = extract_mod_declaration(&child, source_bytes) {
                        mod_declarations.push(name);
                    }
                    pending_derives.clear();
                }
                "attribute_item" => {
                    if let Some((derives, line)) = extract_derive_attribute(&child, source_bytes) {
                        pending_derives.push((derives, line));
                    }
                }
                _ => {
                    // Other top-level items: skip, but clear pending derives
                    if child.kind() != "line_comment"
                        && child.kind() != "block_comment"
                        && child.kind() != "inner_attribute_item"
                    {
                        pending_derives.clear();
                    }
                }
            }
        }

        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::Rust,
            content_hash: String::new(), // filled by parse_file
            imports,
            exports,
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR {
                mod_declarations,
                derive_macros,
                trait_implementations,
                error_types,
            }),
        })
    }
}

/// Check if a node has a `visibility_modifier` child (i.e., `pub`).
fn has_visibility_modifier(node: &Node) -> bool {
    for i in 0..(node.child_count() as u32) {
        if let Some(c) = node.child(i) {
            if c.kind() == "visibility_modifier" {
                return true;
            }
        }
    }
    false
}

/// Extract a `use_declaration` into an [`Import`].
///
/// Handles various forms:
/// - `use std::io;`              -> module: "std::io", names: ["io"]
/// - `use std::io::Read;`       -> module: "std::io", names: ["Read"]
/// - `use std::io::{Read, Write};` -> module: "std::io", names: ["Read", "Write"]
/// - `use std::io::*;`          -> module: "std::io", names: ["*"]
fn extract_use_declaration(node: &Node, source: &[u8]) -> Option<Import> {
    let line = node.start_position().row + 1;

    // Find the argument child (scoped_identifier, scoped_use_list, use_wildcard, identifier, etc.)
    let arg = find_use_argument(node)?;

    let (module, names) = parse_use_path(&arg, source);

    Some(Import {
        module,
        names,
        is_type_only: false, // Rust doesn't have type-only imports
        line,
    })
}

/// Find the main argument node inside a `use_declaration`.
fn find_use_argument<'a>(node: &'a Node<'a>) -> Option<Node<'a>> {
    for i in 0..(node.child_count() as u32) {
        let child = node.child(i)?;
        match child.kind() {
            "scoped_identifier" | "scoped_use_list" | "use_wildcard" | "identifier"
            | "use_as_clause" => return Some(child),
            _ => {}
        }
    }
    None
}

/// Parse a use path into (module, names).
fn parse_use_path(node: &Node, source: &[u8]) -> (String, Vec<String>) {
    match node.kind() {
        "scoped_identifier" => {
            // e.g., std::io::Read -> module: "std::io", name: "Read"
            let full = node_text(node, source).to_string();
            if let Some(pos) = full.rfind("::") {
                let module = full[..pos].to_string();
                let name = full[pos + 2..].to_string();
                (module, vec![name])
            } else {
                (full.clone(), vec![full])
            }
        }
        "scoped_use_list" => {
            // e.g., std::io::{Read, Write}
            let mut module = String::new();
            let mut names = Vec::new();

            for i in 0..(node.child_count() as u32) {
                if let Some(child) = node.child(i) {
                    match child.kind() {
                        "scoped_identifier" | "identifier" => {
                            // The path part before the use_list
                            if names.is_empty() {
                                module = node_text(&child, source).to_string();
                            }
                        }
                        "use_list" => {
                            names = extract_use_list(&child, source);
                        }
                        _ => {}
                    }
                }
            }

            (module, names)
        }
        "use_wildcard" => {
            // e.g., std::io::*
            let full = node_text(node, source).to_string();
            if let Some(pos) = full.rfind("::") {
                (full[..pos].to_string(), vec!["*".to_string()])
            } else {
                (full, vec!["*".to_string()])
            }
        }
        "identifier" => {
            let name = node_text(node, source).to_string();
            (name.clone(), vec![name])
        }
        "use_as_clause" => {
            // e.g., `use foo as bar;`
            let full = node_text(node, source).to_string();
            if let Some(pos) = full.rfind("::") {
                let module = full[..pos].to_string();
                let rest = full[pos + 2..].to_string();
                (module, vec![rest])
            } else {
                (full.clone(), vec![full])
            }
        }
        _ => {
            let text = node_text(node, source).to_string();
            (text.clone(), vec![text])
        }
    }
}

/// Extract names from a `use_list` node.
fn extract_use_list(node: &Node, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "identifier" => {
                    names.push(node_text(&child, source).to_string());
                }
                "scoped_identifier" => {
                    // Nested path like `io::Read` inside a use list
                    names.push(node_text(&child, source).to_string());
                }
                "self" => {
                    names.push("self".to_string());
                }
                "use_as_clause" => {
                    // `Read as MyRead` — record the original name
                    if let Some(first) = child.child(0) {
                        names.push(node_text(&first, source).to_string());
                    }
                }
                _ => {}
            }
        }
    }
    names
}

/// Extract a function definition.
fn extract_function(node: &Node, source: &[u8], is_pub: bool) -> Function {
    let name = find_child_text(node, "identifier", source)
        .or_else(|| find_child_text(node, "name", source))
        .unwrap_or_default();

    let is_async = node
        .child_by_field_name("modifiers".as_bytes())
        .or_else(|| find_child_node(node, "function_modifiers"))
        .is_some_and(|m| node_text(&m, source).contains("async"));

    Function {
        name,
        is_public: is_pub,
        is_async,
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    }
}

/// Extract a type definition (struct, enum, trait, type alias).
fn extract_type_def(node: &Node, source: &[u8], kind: TypeDefKind, is_pub: bool) -> TypeDef {
    let name = find_child_text(node, "type_identifier", source).unwrap_or_default();

    TypeDef {
        name,
        kind,
        is_public: is_pub,
        line: node.start_position().row + 1,
    }
}

/// Extract `impl Trait for Type` or return None for inherent impls.
fn extract_impl(node: &Node, source: &[u8]) -> Option<TraitImpl> {
    // An `impl_item` has children like:
    //   impl <type_parameters> <trait> for <type> { ... }
    // We need to detect the `for` keyword to distinguish trait impls from inherent impls.

    let full_text = node_text(node, source);

    // Quick check: if there's no `for` keyword, it's an inherent impl
    if !full_text.contains(" for ") {
        return None;
    }

    // Find trait name and type name from children
    let mut trait_name = None;
    let mut type_name = None;
    let mut found_for = false;

    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "type_identifier" | "scoped_type_identifier" | "generic_type" => {
                    let text = node_text(&child, source).to_string();
                    if !found_for {
                        trait_name = Some(text);
                    } else {
                        type_name = Some(text);
                    }
                }
                "for" => {
                    found_for = true;
                }
                _ => {}
            }
        }
    }

    Some(TraitImpl {
        trait_name: trait_name?,
        type_name: type_name?,
        line: node.start_position().row + 1,
    })
}

/// Extract methods from an `impl` block and add them to functions/exports.
fn extract_impl_functions(
    node: &Node,
    source: &[u8],
    functions: &mut Vec<Function>,
    exports: &mut Vec<Export>,
) {
    // Find the declaration_list child
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            if child.kind() == "declaration_list" {
                for j in 0..(child.child_count() as u32) {
                    if let Some(item) = child.child(j) {
                        if item.kind() == "function_item" {
                            let is_pub = has_visibility_modifier(&item);
                            let func = extract_function(&item, source, is_pub);
                            if is_pub {
                                exports.push(Export {
                                    name: func.name.clone(),
                                    is_default: false,
                                    is_type_only: false,
                                    line: func.line,
                                });
                            }
                            functions.push(func);
                        }
                    }
                }
            }
        }
    }
}

/// Extract a `mod` declaration name.
fn extract_mod_declaration(node: &Node, source: &[u8]) -> Option<String> {
    find_child_text(node, "identifier", source)
}

/// Extract derive traits from an `#[derive(...)]` attribute.
fn extract_derive_attribute(node: &Node, source: &[u8]) -> Option<(Vec<String>, usize)> {
    // attribute_item has child: attribute
    let attr = find_child_node(node, "attribute")?;
    let full_text = node_text(&attr, source);

    // Check if it starts with "derive"
    if !full_text.starts_with("derive") {
        return None;
    }

    let line = node.start_position().row + 1;

    // Extract the content between parentheses
    let start = full_text.find('(')?;
    let end = full_text.rfind(')')?;
    let content = &full_text[start + 1..end];

    let derives: Vec<String> = content
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if derives.is_empty() {
        return None;
    }

    Some((derives, line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::TypeDefKind;

    fn parse_rust(source: &str) -> ProjectFile {
        let parser = RustParser;
        parser
            .parse(Path::new("test.rs"), source)
            .expect("parse should succeed")
    }

    #[test]
    fn extracts_simple_use_declaration() {
        let pf = parse_rust("use std::io::Read;");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "std::io");
        assert_eq!(pf.imports[0].names, vec!["Read"]);
        assert_eq!(pf.imports[0].line, 1);
    }

    #[test]
    fn extracts_grouped_use() {
        let pf = parse_rust("use std::io::{Read, Write};");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "std::io");
        assert!(pf.imports[0].names.contains(&"Read".to_string()));
        assert!(pf.imports[0].names.contains(&"Write".to_string()));
    }

    #[test]
    fn extracts_wildcard_use() {
        let pf = parse_rust("use std::io::*;");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "std::io");
        assert_eq!(pf.imports[0].names, vec!["*"]);
    }

    #[test]
    fn extracts_public_function() {
        let pf = parse_rust("pub fn hello() {}");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "hello");
        assert!(pf.functions[0].is_public);
        assert!(!pf.functions[0].is_async);

        // Public function should appear as export
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "hello");
    }

    #[test]
    fn extracts_private_function() {
        let pf = parse_rust("fn internal() {}");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "internal");
        assert!(!pf.functions[0].is_public);
        assert!(pf.exports.is_empty());
    }

    #[test]
    fn extracts_async_function() {
        let pf = parse_rust("pub async fn fetch_data() {}");
        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_async);
        assert!(pf.functions[0].is_public);
    }

    #[test]
    fn extracts_struct() {
        let pf = parse_rust("pub struct Config { pub name: String }");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "Config");
        assert_eq!(pf.types[0].kind, TypeDefKind::Struct);
        assert!(pf.types[0].is_public);

        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "Config");
        assert!(pf.exports[0].is_type_only);
    }

    #[test]
    fn extracts_private_struct() {
        let pf = parse_rust("struct Inner { value: i32 }");
        assert_eq!(pf.types.len(), 1);
        assert!(!pf.types[0].is_public);
        assert!(pf.exports.is_empty());
    }

    #[test]
    fn extracts_enum() {
        let pf = parse_rust("pub enum Status { Active, Inactive }");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "Status");
        assert_eq!(pf.types[0].kind, TypeDefKind::Enum);
        assert!(pf.types[0].is_public);
    }

    #[test]
    fn extracts_trait() {
        let pf = parse_rust("pub trait Greet { fn greet(&self); }");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "Greet");
        assert_eq!(pf.types[0].kind, TypeDefKind::Trait);
    }

    #[test]
    fn extracts_type_alias() {
        let pf = parse_rust("pub type Result<T> = std::result::Result<T, Error>;");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "Result");
        assert_eq!(pf.types[0].kind, TypeDefKind::TypeAlias);
    }

    #[test]
    fn extracts_trait_impl() {
        let pf = parse_rust(
            "impl Display for Config { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }",
        );
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert_eq!(ir.trait_implementations.len(), 1);
        assert_eq!(ir.trait_implementations[0].trait_name, "Display");
        assert_eq!(ir.trait_implementations[0].type_name, "Config");
    }

    #[test]
    fn inherent_impl_not_trait_impl() {
        let pf = parse_rust("impl Config { pub fn new() -> Self { Self {} } }");
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert!(ir.trait_implementations.is_empty());
        // But the method should be extracted
        assert!(pf.functions.iter().any(|f| f.name == "new"));
    }

    #[test]
    fn extracts_mod_declaration() {
        let pf = parse_rust("mod utils;");
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert_eq!(ir.mod_declarations, vec!["utils"]);
    }

    #[test]
    fn extracts_derive_macros() {
        let source = "#[derive(Debug, Clone, Serialize)]\npub struct Config {}";
        let pf = parse_rust(source);
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert_eq!(ir.derive_macros.len(), 1);
        assert_eq!(ir.derive_macros[0].type_name, "Config");
        assert!(ir.derive_macros[0].derives.contains(&"Debug".to_string()));
        assert!(ir.derive_macros[0].derives.contains(&"Clone".to_string()));
        assert!(
            ir.derive_macros[0]
                .derives
                .contains(&"Serialize".to_string())
        );
    }

    #[test]
    fn extracts_error_types() {
        let source = "pub enum ScanError { Io(std::io::Error), Parse(String) }";
        let pf = parse_rust(source);
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert!(ir.error_types.contains(&"ScanError".to_string()));
    }

    #[test]
    fn extracts_error_struct() {
        let pf = parse_rust("pub struct ParseError { message: String }");
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert!(ir.error_types.contains(&"ParseError".to_string()));
    }

    #[test]
    fn graceful_on_empty_source() {
        let pf = parse_rust("");
        assert!(pf.imports.is_empty());
        assert!(pf.exports.is_empty());
        assert!(pf.functions.is_empty());
        assert!(pf.types.is_empty());
    }

    #[test]
    fn extracts_impl_methods() {
        let source = r#"
impl Config {
    pub fn new() -> Self { Self {} }
    fn validate(&self) -> bool { true }
    pub async fn load() -> Self { Self {} }
}
"#;
        let pf = parse_rust(source);
        assert!(pf.functions.iter().any(|f| f.name == "new" && f.is_public));
        assert!(
            pf.functions
                .iter()
                .any(|f| f.name == "validate" && !f.is_public)
        );
        assert!(pf.functions.iter().any(|f| f.name == "load" && f.is_async));
        // Public methods should appear as exports
        assert!(pf.exports.iter().any(|e| e.name == "new"));
        assert!(pf.exports.iter().any(|e| e.name == "load"));
        assert!(!pf.exports.iter().any(|e| e.name == "validate"));
    }

    #[test]
    fn language_is_rust() {
        let pf = parse_rust("fn main() {}");
        assert_eq!(pf.language, Language::Rust);
        assert!(matches!(pf.language_ir, LanguageIR::Rust(_)));
    }

    #[test]
    fn multiple_items_combined() {
        let source = r#"
use std::io::Read;
use serde::Serialize;

pub struct Config {
    name: String,
}

pub fn create_config() -> Config {
    Config { name: String::new() }
}

mod tests;
"#;
        let pf = parse_rust(source);
        assert_eq!(pf.imports.len(), 2);
        assert_eq!(pf.types.len(), 1);
        assert!(pf.functions.iter().any(|f| f.name == "create_config"));

        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert!(ir.mod_declarations.contains(&"tests".to_string()));
    }
}
