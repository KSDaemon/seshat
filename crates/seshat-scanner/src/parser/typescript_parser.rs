//! Tree-sitter–based TypeScript parser.
//!
//! Extracts imports (named, default, type-only, namespace), exports
//! (named, default, re-exports), functions, types (interfaces, type
//! aliases, classes, enums), and TypeScript-specific IR from source files.
//! Handles both `.ts` and `.tsx` files.

use std::path::Path;

use seshat_core::{
    Export, Function, Import, Language, LanguageIR, ProjectFile, TypeDef, TypeDefKind, TypeScriptIR,
};
use tree_sitter::{Node, Parser as TsParser};

use super::{
    Parser, child_has_async_value, collect_js_doc_comment, extract_exported_lexical,
    extract_function_declaration, extract_import_names, extract_js_ts_parameters,
    extract_string_value, find_arrow_or_function_expr, find_child_node, find_child_text,
    has_child_kind, node_text,
};
use crate::ScanError;

/// Parser for TypeScript (`.ts`) and TSX (`.tsx`) source files.
pub struct TypeScriptParser;

impl Parser for TypeScriptParser {
    fn parse(&self, path: &Path, source: &str) -> Result<ProjectFile, ScanError> {
        let is_tsx = path.extension().and_then(|e| e.to_str()) == Some("tsx");

        let mut ts_parser = TsParser::new();
        let language = if is_tsx {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        };
        ts_parser
            .set_language(&language)
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
        let mut type_only_imports = Vec::new();
        let mut decorators = Vec::new();
        let mut has_default_export = false;
        let mut has_barrel_exports = false;

        let source_bytes = source.as_bytes();

        // File-level doc: leading /** */ or // comment before first declaration.
        let file_doc = extract_ts_file_doc(&root, source_bytes);

        for i in 0..(root.child_count() as u32) {
            let Some(child) = root.child(i) else { continue };
            match child.kind() {
                "import_statement" => {
                    if let Some(imp) = extract_import(&child, source_bytes) {
                        if imp.is_type_only {
                            for name in &imp.names {
                                type_only_imports.push(name.clone());
                            }
                        }
                        imports.push(imp);
                    }
                }
                "export_statement" => {
                    extract_export(
                        &child,
                        source_bytes,
                        &mut exports,
                        &mut functions,
                        &mut types,
                        &mut decorators,
                        &mut has_default_export,
                        &mut has_barrel_exports,
                    );
                }
                "function_declaration" => {
                    let mut func = extract_function_declaration(&child, source_bytes);
                    func.doc_comment = collect_js_doc_comment(&child, source_bytes);
                    functions.push(func);
                }
                "interface_declaration" => {
                    let mut td = extract_interface(&child, source_bytes);
                    td.doc_comment = collect_js_doc_comment(&child, source_bytes);
                    types.push(td);
                }
                "type_alias_declaration" => {
                    let mut td = extract_type_alias(&child, source_bytes);
                    td.doc_comment = collect_js_doc_comment(&child, source_bytes);
                    types.push(td);
                }
                "class_declaration" | "abstract_class_declaration" => {
                    let (mut td, class_decorators) = extract_class(&child, source_bytes);
                    td.doc_comment = collect_js_doc_comment(&child, source_bytes);
                    decorators.extend(class_decorators);
                    types.push(td);
                }
                "enum_declaration" => {
                    let mut td = extract_enum(&child, source_bytes);
                    td.doc_comment = collect_js_doc_comment(&child, source_bytes);
                    types.push(td);
                }
                "lexical_declaration" => {
                    // Top-level `const fn = () => {}` or `const fn = function() {}`
                    extract_lexical_functions(&child, source_bytes, &mut functions);
                }
                _ => {}
            }
        }

        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::TypeScript,
            content_hash: String::new(), // filled by parse_file
            imports,
            exports,
            functions,
            types,
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR {
                has_barrel_exports,
                type_only_imports,
                decorators,
                default_export: has_default_export,
            }),
            file_doc,
        })
    }
}

/// Extract a file-level doc comment from a TS/JS file.
///
/// Returns the text of the first `comment` node at the root level (before
/// any non-comment code). Captures both `/** ... */` JSDoc and `// ...`.
fn extract_ts_file_doc(root: &Node, source: &[u8]) -> Option<String> {
    for i in 0..(root.child_count() as u32) {
        let Some(child) = root.child(i) else { break };
        if child.kind() == "comment" {
            let raw = node_text(&child, source);
            let cleaned = super::clean_js_comment(raw);
            return if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            };
        }
        // Skip shebangs; stop on anything else.
        if child.kind() != "hash_bang_line" {
            break;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Import extraction
// ---------------------------------------------------------------------------

/// Extract an `import_statement` into an [`Import`].
///
/// Handles:
/// - `import { Foo, Bar } from 'module';`         (named)
/// - `import Foo from 'module';`                   (default)
/// - `import * as ns from 'module';`               (namespace)
/// - `import type { Foo } from 'module';`          (type-only)
fn extract_import(node: &Node, source: &[u8]) -> Option<Import> {
    let line = node.start_position().row + 1;

    // Check for type-only import: `import type { ... } from '...'`
    let is_type_only = has_child_kind(node, "type");

    // Extract the module path from the string literal
    let module = extract_string_value(node, source)?;

    // Extract imported names from import_clause
    let import_clause = find_child_node(node, "import_clause")?;
    let names = extract_import_names(&import_clause, source);

    if names.is_empty() {
        return None;
    }

    Some(Import {
        module,
        names,
        is_type_only,
        line,
    })
}

// ---------------------------------------------------------------------------
// Export extraction
// ---------------------------------------------------------------------------

/// Extract an `export_statement` into exports, functions, types, etc.
///
/// Handles:
/// - `export { Foo, Bar };`                    (named)
/// - `export { Foo } from './module';`         (re-export)
/// - `export * from './module';`               (barrel re-export)
/// - `export default function foo() {}`        (default function)
/// - `export default class Foo {}`             (default class)
/// - `export default <expr>;`                  (default expression)
/// - `export function foo() {}`                (named function)
/// - `export class Foo {}`                     (named class)
/// - `export const x = ...;`                   (named constant)
/// - `export interface Foo { ... }`            (named interface)
/// - `export type Foo = ...;`                  (named type alias)
/// - `export enum Foo { ... }`                 (named enum)
/// - `export type { Foo } from '...';`         (type-only re-export)
#[allow(clippy::too_many_arguments)]
fn extract_export(
    node: &Node,
    source: &[u8],
    exports: &mut Vec<Export>,
    functions: &mut Vec<Function>,
    types: &mut Vec<TypeDef>,
    decorators: &mut Vec<String>,
    has_default_export: &mut bool,
    has_barrel_exports: &mut bool,
) {
    let line = node.start_position().row + 1;
    let is_default = has_child_kind(node, "default");
    let is_type_only = has_child_kind(node, "type");

    if is_default {
        *has_default_export = true;
    }

    // Extract decorators that are direct children of the export_statement
    // (e.g., `@Injectable() export class Foo {}` — decorators are siblings of the class)
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            if child.kind() == "decorator" {
                let dec_name = extract_decorator_name(&child, source);
                if !dec_name.is_empty() {
                    decorators.push(dec_name);
                }
            }
        }
    }

    // Check for re-export with `from` keyword: `export { ... } from '...'` or `export * from '...'`
    let has_from = has_child_kind(node, "from");

    // Check for barrel export: `export * from '...'`
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            if child.kind() == "*" {
                *has_barrel_exports = true;
                if has_from {
                    let module = extract_string_value(node, source).unwrap_or_default();
                    exports.push(Export {
                        name: format!("* from {module}"),
                        is_default: false,
                        is_type_only: false,
                        line,
                    });
                }
                return;
            }
        }
    }

    // Check for `export_clause`: `export { Foo, Bar }` or `export { Foo } from '...'`
    if let Some(clause) = find_child_node(node, "export_clause") {
        let re_export_module = if has_from {
            extract_string_value(node, source)
        } else {
            None
        };

        for i in 0..(clause.child_count() as u32) {
            if let Some(spec) = clause.child(i) {
                if spec.kind() == "export_specifier" {
                    let name = node_text(&spec, source).to_string();
                    let is_default_specifier = name == "default";
                    let export_name = if let Some(ref module) = re_export_module {
                        format!("{name} from {module}")
                    } else {
                        name
                    };
                    exports.push(Export {
                        name: export_name,
                        is_default: is_default_specifier,
                        is_type_only,
                        line,
                    });
                }
            }
        }
        return;
    }

    // Exported declarations
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "function_declaration" => {
                    let mut func = extract_function_declaration(&child, source);
                    func.is_public = true;
                    let export_name = func.name.clone();
                    functions.push(func);
                    exports.push(Export {
                        name: export_name,
                        is_default,
                        is_type_only: false,
                        line,
                    });
                }
                "class_declaration" | "abstract_class_declaration" => {
                    let (mut td, class_decorators) = extract_class(&child, source);
                    td.is_public = true;
                    decorators.extend(class_decorators);
                    let export_name = td.name.clone();
                    types.push(td);
                    exports.push(Export {
                        name: export_name,
                        is_default,
                        is_type_only: false,
                        line,
                    });
                }
                "interface_declaration" => {
                    let mut td = extract_interface(&child, source);
                    td.is_public = true;
                    let export_name = td.name.clone();
                    types.push(td);
                    exports.push(Export {
                        name: export_name,
                        is_default: false,
                        is_type_only: true,
                        line,
                    });
                }
                "type_alias_declaration" => {
                    let mut td = extract_type_alias(&child, source);
                    td.is_public = true;
                    let export_name = td.name.clone();
                    types.push(td);
                    exports.push(Export {
                        name: export_name,
                        is_default: false,
                        is_type_only: true,
                        line,
                    });
                }
                "enum_declaration" => {
                    let mut td = extract_enum(&child, source);
                    td.is_public = true;
                    let export_name = td.name.clone();
                    types.push(td);
                    exports.push(Export {
                        name: export_name,
                        is_default: false,
                        is_type_only: false,
                        line,
                    });
                }
                "lexical_declaration" => {
                    // `export const x = ...;`
                    extract_exported_lexical(&child, source, exports, functions, is_default, line);
                }
                "identifier" => {
                    // `export default Foo;`
                    if is_default {
                        exports.push(Export {
                            name: node_text(&child, source).to_string(),
                            is_default: true,
                            is_type_only: false,
                            line,
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

/// Extract functions from a top-level `lexical_declaration` (non-exported).
fn extract_lexical_functions(node: &Node, source: &[u8], functions: &mut Vec<Function>) {
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            if child.kind() == "variable_declarator" {
                let func_node = find_arrow_or_function_expr(&child);

                if let Some(ref fn_node) = func_node {
                    let name = find_child_text(&child, "identifier", source).unwrap_or_default();
                    let is_async = child_has_async_value(&child, source);
                    let parameters = extract_js_ts_parameters(fn_node, source);
                    functions.push(Function {
                        name,
                        is_public: false,
                        is_async,
                        line: child.start_position().row + 1,
                        end_line: child.end_position().row + 1,
                        parameters,
                        doc_comment: None, // populated in PR C
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Type extraction
// ---------------------------------------------------------------------------

/// Extract an `interface_declaration`.
fn extract_interface(node: &Node, source: &[u8]) -> TypeDef {
    let name = find_child_text(node, "type_identifier", source).unwrap_or_default();
    TypeDef {
        name,
        kind: TypeDefKind::Interface,
        is_public: false,
        line: node.start_position().row + 1,
        doc_comment: None, // populated in PR C
    }
}

/// Extract a `type_alias_declaration`.
fn extract_type_alias(node: &Node, source: &[u8]) -> TypeDef {
    let name = find_child_text(node, "type_identifier", source).unwrap_or_default();
    TypeDef {
        name,
        kind: TypeDefKind::TypeAlias,
        is_public: false,
        line: node.start_position().row + 1,
        doc_comment: None, // populated in PR C
    }
}

/// Extract a `class_declaration` or `abstract_class_declaration`.
/// Returns the TypeDef and any decorators found on the class.
fn extract_class(node: &Node, source: &[u8]) -> (TypeDef, Vec<String>) {
    let name = find_child_text(node, "type_identifier", source).unwrap_or_default();
    let mut class_decorators = Vec::new();

    // Extract decorators (children of the class node)
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            if child.kind() == "decorator" {
                let dec_text = extract_decorator_name(&child, source);
                if !dec_text.is_empty() {
                    class_decorators.push(dec_text);
                }
            }
        }
    }

    let td = TypeDef {
        name,
        kind: TypeDefKind::Class,
        is_public: false,
        line: node.start_position().row + 1,
        doc_comment: None, // populated in PR C
    };
    (td, class_decorators)
}

/// Extract an `enum_declaration`.
fn extract_enum(node: &Node, source: &[u8]) -> TypeDef {
    let name = find_child_text(node, "identifier", source).unwrap_or_default();
    TypeDef {
        name,
        kind: TypeDefKind::Enum,
        is_public: false,
        line: node.start_position().row + 1,
        doc_comment: None, // populated in PR C
    }
}

/// Extract the name from a decorator node.
///
/// For `@Component({...})` returns `"Component"`.
/// For `@Injectable` returns `"Injectable"`.
fn extract_decorator_name(node: &Node, source: &[u8]) -> String {
    // Decorator structure: `@` followed by identifier or call_expression
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "identifier" => {
                    return node_text(&child, source).to_string();
                }
                "call_expression" => {
                    // call_expression has function name as first child
                    if let Some(fn_name) = child.child(0) {
                        return node_text(&fn_name, source).to_string();
                    }
                }
                _ => {}
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::TypeDefKind;

    fn parse_ts(source: &str) -> ProjectFile {
        let parser = TypeScriptParser;
        parser
            .parse(Path::new("test.ts"), source)
            .expect("parse should succeed")
    }

    fn parse_tsx(source: &str) -> ProjectFile {
        let parser = TypeScriptParser;
        parser
            .parse(Path::new("test.tsx"), source)
            .expect("parse should succeed")
    }

    fn ts_ir(pf: &ProjectFile) -> &TypeScriptIR {
        match &pf.language_ir {
            LanguageIR::TypeScript(ir) => ir,
            _ => panic!("expected TypeScriptIR"),
        }
    }

    // -- Imports --

    #[test]
    fn extracts_named_import() {
        let pf = parse_ts("import { Foo, Bar } from 'module';");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "module");
        assert!(pf.imports[0].names.contains(&"Foo".to_string()));
        assert!(pf.imports[0].names.contains(&"Bar".to_string()));
        assert!(!pf.imports[0].is_type_only);
    }

    #[test]
    fn extracts_default_import() {
        let pf = parse_ts("import React from 'react';");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "react");
        assert_eq!(pf.imports[0].names, vec!["React"]);
    }

    #[test]
    fn extracts_namespace_import() {
        let pf = parse_ts("import * as utils from './utils';");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "./utils");
        assert_eq!(pf.imports[0].names, vec!["* as utils"]);
    }

    #[test]
    fn extracts_type_only_import() {
        let pf = parse_ts("import type { User } from './types';");
        assert_eq!(pf.imports.len(), 1);
        assert!(pf.imports[0].is_type_only);
        assert_eq!(pf.imports[0].module, "./types");
        assert!(pf.imports[0].names.contains(&"User".to_string()));

        let ir = ts_ir(&pf);
        assert!(ir.type_only_imports.contains(&"User".to_string()));
    }

    #[test]
    fn extracts_multiple_imports() {
        let source = r#"
import React from 'react';
import { useState, useEffect } from 'react';
import type { FC } from 'react';
"#;
        let pf = parse_ts(source);
        assert_eq!(pf.imports.len(), 3);
    }

    // -- Exports --

    #[test]
    fn extracts_named_export_function() {
        let pf = parse_ts("export function greet(name: string): void {}");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "greet");
        assert!(!pf.exports[0].is_default);

        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_public);
    }

    #[test]
    fn extracts_default_export_function() {
        let pf = parse_ts("export default function handler() {}");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "handler");
        assert!(pf.exports[0].is_default);

        let ir = ts_ir(&pf);
        assert!(ir.default_export);
    }

    #[test]
    fn extracts_async_exported_function() {
        let pf = parse_ts("export async function fetchData(): Promise<void> {}");
        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_async);
        assert!(pf.functions[0].is_public);
    }

    #[test]
    fn extracts_export_const() {
        let pf = parse_ts("export const API_URL = 'http://example.com';");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "API_URL");
    }

    #[test]
    fn extracts_export_const_arrow() {
        let pf = parse_ts("export const handler = () => {};");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "handler");

        // Arrow function should also be tracked as a function
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "handler");
        assert!(pf.functions[0].is_public);
    }

    #[test]
    fn extracts_export_const_async_arrow() {
        let pf = parse_ts("export const handler = async () => {};");
        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_async);
    }

    #[test]
    fn extracts_re_export() {
        let pf = parse_ts("export { Foo, Bar } from './module';");
        assert_eq!(pf.exports.len(), 2);
        assert!(
            pf.exports
                .iter()
                .any(|e| e.name.contains("Foo") && e.name.contains("./module"))
        );
    }

    #[test]
    fn extracts_barrel_re_export() {
        let pf = parse_ts("export * from './module';");
        let ir = ts_ir(&pf);
        assert!(ir.has_barrel_exports);
        assert_eq!(pf.exports.len(), 1);
        assert!(pf.exports[0].name.contains("* from"));
    }

    #[test]
    fn extracts_type_only_re_export() {
        let pf = parse_ts("export type { Foo } from './types';");
        assert_eq!(pf.exports.len(), 1);
        assert!(pf.exports[0].is_type_only);
    }

    #[test]
    fn extracts_default_export_identifier() {
        let pf = parse_ts("export default App;");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "App");
        assert!(pf.exports[0].is_default);

        let ir = ts_ir(&pf);
        assert!(ir.default_export);
    }

    // -- Functions --

    #[test]
    fn extracts_non_exported_function() {
        let pf = parse_ts("function helper(): void {}");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "helper");
        assert!(!pf.functions[0].is_public);
        assert!(pf.exports.is_empty());
    }

    #[test]
    fn extracts_arrow_function() {
        let pf = parse_ts("const greet = (name: string) => {};");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "greet");
        assert!(!pf.functions[0].is_public);
    }

    // -- Types --

    #[test]
    fn extracts_interface() {
        let pf = parse_ts("interface User { name: string; age: number }");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "User");
        assert_eq!(pf.types[0].kind, TypeDefKind::Interface);
        assert!(!pf.types[0].is_public);
    }

    #[test]
    fn extracts_exported_interface() {
        let pf = parse_ts("export interface User { name: string }");
        assert_eq!(pf.types.len(), 1);
        assert!(pf.types[0].is_public);
        assert_eq!(pf.exports.len(), 1);
        assert!(pf.exports[0].is_type_only);
    }

    #[test]
    fn extracts_type_alias() {
        let pf = parse_ts("type ID = string | number;");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "ID");
        assert_eq!(pf.types[0].kind, TypeDefKind::TypeAlias);
    }

    #[test]
    fn extracts_exported_type_alias() {
        let pf = parse_ts("export type ID = string | number;");
        assert_eq!(pf.types.len(), 1);
        assert!(pf.types[0].is_public);
        assert!(pf.exports[0].is_type_only);
    }

    #[test]
    fn extracts_class() {
        let pf = parse_ts("class AppService {}");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "AppService");
        assert_eq!(pf.types[0].kind, TypeDefKind::Class);
    }

    #[test]
    fn extracts_exported_class() {
        let pf = parse_ts("export class AppService {}");
        assert_eq!(pf.types.len(), 1);
        assert!(pf.types[0].is_public);
        assert!(!pf.exports[0].is_type_only);
    }

    #[test]
    fn extracts_default_export_class() {
        let pf = parse_ts("export default class Foo {}");
        assert_eq!(pf.exports.len(), 1);
        assert!(pf.exports[0].is_default);

        let ir = ts_ir(&pf);
        assert!(ir.default_export);
    }

    #[test]
    fn extracts_abstract_class() {
        let pf = parse_ts("abstract class Base {}");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "Base");
        assert_eq!(pf.types[0].kind, TypeDefKind::Class);
    }

    #[test]
    fn extracts_enum() {
        let pf = parse_ts("enum Color { Red, Green, Blue }");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "Color");
        assert_eq!(pf.types[0].kind, TypeDefKind::Enum);
    }

    #[test]
    fn extracts_exported_enum() {
        let pf = parse_ts("export enum Direction { Up, Down, Left, Right }");
        assert_eq!(pf.types.len(), 1);
        assert!(pf.types[0].is_public);
        assert!(!pf.exports[0].is_type_only);
    }

    // -- Decorators --

    #[test]
    fn extracts_decorator() {
        let source = "@Component({selector: 'app'})\nclass AppComponent {}";
        let pf = parse_ts(source);
        let ir = ts_ir(&pf);
        assert!(ir.decorators.contains(&"Component".to_string()));
    }

    #[test]
    fn extracts_multiple_decorators() {
        let source = "@Injectable()\n@Singleton\nclass Service {}";
        let pf = parse_ts(source);
        let ir = ts_ir(&pf);
        assert!(ir.decorators.contains(&"Injectable".to_string()));
        assert!(ir.decorators.contains(&"Singleton".to_string()));
    }

    // -- TSX --

    #[test]
    fn tsx_does_not_break_parse() {
        let source = r#"
import React from 'react';

interface Props { name: string }

const App: React.FC<Props> = ({ name }) => {
    return <div>Hello {name}</div>;
};

export default App;
"#;
        let pf = parse_tsx(source);
        assert_eq!(pf.language, Language::TypeScript);
        assert!(pf.imports.iter().any(|i| i.module == "react"));
        assert!(pf.types.iter().any(|t| t.name == "Props"));
        assert!(pf.exports.iter().any(|e| e.name == "App"));
    }

    // -- Edge cases --

    #[test]
    fn graceful_on_empty_source() {
        let pf = parse_ts("");
        assert!(pf.imports.is_empty());
        assert!(pf.exports.is_empty());
        assert!(pf.functions.is_empty());
        assert!(pf.types.is_empty());
    }

    #[test]
    fn language_is_typescript() {
        let pf = parse_ts("const x = 1;");
        assert_eq!(pf.language, Language::TypeScript);
        assert!(matches!(pf.language_ir, LanguageIR::TypeScript(_)));
    }

    #[test]
    fn combined_file() {
        let source = r#"
import { useState } from 'react';
import type { FC } from 'react';

export interface UserProps {
    name: string;
    age: number;
}

type ID = string;

export function greet(user: UserProps): string {
    return `Hello, ${user.name}`;
}

export default function App() {
    return null;
}

export const VERSION = '1.0.0';
"#;
        let pf = parse_ts(source);
        assert_eq!(pf.imports.len(), 2);
        assert!(pf.types.iter().any(|t| t.name == "UserProps"));
        assert!(pf.types.iter().any(|t| t.name == "ID"));
        assert!(pf.functions.iter().any(|f| f.name == "greet"));
        assert!(pf.functions.iter().any(|f| f.name == "App"));
        assert!(pf.exports.iter().any(|e| e.name == "greet"));
        assert!(pf.exports.iter().any(|e| e.name == "App" && e.is_default));
        assert!(pf.exports.iter().any(|e| e.name == "VERSION"));

        let ir = ts_ir(&pf);
        assert!(ir.default_export);
        assert!(ir.type_only_imports.contains(&"FC".to_string()));
    }

    // -----------------------------------------------------------------------
    // Parameter extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn extracts_function_declaration_parameters() {
        let pf = parse_ts("function greet(name: string, age: number): string { return name; }");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "greet");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["name".to_string(), "age".to_string()]
        );
    }

    #[test]
    fn extracts_arrow_function_parameters() {
        let pf = parse_ts("const add = (a: number, b: number): number => a + b;");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "add");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn extracts_exported_function_parameters() {
        let pf = parse_ts("export function process(input: string, opts?: Options): void {}");
        let func = pf.functions.iter().find(|f| f.name == "process").unwrap();
        assert_eq!(
            func.parameters,
            vec!["input".to_string(), "opts".to_string()]
        );
    }

    #[test]
    fn extracts_export_const_arrow_parameters() {
        let pf = parse_ts("export const multiply = (x: number, y: number) => x * y;");
        let func = pf.functions.iter().find(|f| f.name == "multiply").unwrap();
        assert_eq!(func.parameters, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn no_parameters_for_nullary_function() {
        let pf = parse_ts("function init(): void {}");
        assert_eq!(pf.functions[0].parameters, Vec::<String>::new());
    }

    #[test]
    fn extracts_async_function_parameters() {
        let pf =
            parse_ts("async function fetch(url: string, timeout: number): Promise<Response> {}");
        assert!(pf.functions[0].is_async);
        assert_eq!(
            pf.functions[0].parameters,
            vec!["url".to_string(), "timeout".to_string()]
        );
    }

    #[test]
    fn extracts_function_jsdoc() {
        let source = r#"
/**
 * Handles an incoming request.
 * @param req - the request object
 */
function handleRequest(req: Request): Response {
    return new Response();
}
"#;
        let pf = parse_ts(source);
        assert_eq!(pf.functions.len(), 1);
        let doc = pf.functions[0].doc_comment.as_deref().unwrap_or("");
        assert!(doc.contains("Handles an incoming request."), "got: {doc}");
    }

    #[test]
    fn function_without_jsdoc_is_none() {
        let pf = parse_ts("function noDoc(): void {}");
        assert!(pf.functions[0].doc_comment.is_none());
    }

    #[test]
    fn extracts_file_level_jsdoc() {
        let source = r#"/**
 * Authentication utilities module.
 */
import { User } from './types';
"#;
        let pf = parse_ts(source);
        let file_doc = pf.file_doc.as_deref().unwrap_or("");
        assert!(
            file_doc.contains("Authentication utilities module."),
            "got: {file_doc}"
        );
    }
}
