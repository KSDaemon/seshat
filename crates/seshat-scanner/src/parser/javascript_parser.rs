//! Tree-sitter–based JavaScript parser.
//!
//! Extracts imports (ESM and CommonJS `require`), exports (ESM and
//! `module.exports`), functions, types (classes), and JavaScript-specific
//! IR from source files. Handles `.js`, `.jsx`, `.mjs`, and `.cjs` files.
//! Detects whether a file uses CommonJS or ESM module system.

use std::path::Path;

use seshat_core::{
    Export, Function, Import, JavaScriptIR, Language, LanguageIR, ModuleSystem, ProjectFile,
    TypeDef, TypeDefKind,
};
use tree_sitter::{Node, Parser as TsParser};

use super::{
    Parser, child_has_async_value, collect_js_doc_comment, extract_exported_lexical,
    extract_function_declaration, extract_import_names, extract_js_ts_parameters,
    extract_string_value, find_arrow_or_function_expr, find_child_node, find_child_text,
    has_child_kind, node_text, ts_dep_from_import,
};
use crate::ScanError;

/// Parser for JavaScript source files.
pub struct JavaScriptParser;

impl Parser for JavaScriptParser {
    fn parse(&self, path: &Path, source: &str) -> Result<ProjectFile, ScanError> {
        let mut ts_parser = TsParser::new();
        ts_parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
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
        let mut require_calls = Vec::new();
        let mut has_module_exports = false;
        let mut has_esm_import = false;
        let mut has_esm_export = false;
        let mut has_cjs_require = false;
        let mut has_cjs_module_exports = false;

        let source_bytes = source.as_bytes();

        // File-level doc: leading /** */ or // comment.
        let file_doc = super::extract_js_ts_file_doc(&root, source_bytes);

        for i in 0..(root.child_count() as u32) {
            let Some(child) = root.child(i) else { continue };
            match child.kind() {
                "import_statement" => {
                    has_esm_import = true;
                    if let Some(imp) = extract_import(&child, source_bytes) {
                        imports.push(imp);
                    }
                }
                "export_statement" => {
                    has_esm_export = true;
                    extract_export(
                        &child,
                        source_bytes,
                        &mut exports,
                        &mut functions,
                        &mut types,
                    );
                }
                "function_declaration" => {
                    let mut func = extract_function_declaration(&child, source_bytes);
                    func.doc_comment = collect_js_doc_comment(&child, source_bytes);
                    functions.push(func);
                }
                "class_declaration" => {
                    let mut td = extract_class(&child, source_bytes);
                    td.doc_comment = collect_js_doc_comment(&child, source_bytes);
                    types.push(td);
                }
                "lexical_declaration" | "variable_declaration" => {
                    // Top-level `const fn = () => {}`, `const x = require('...')`, etc.
                    extract_top_level_declaration(
                        &child,
                        source_bytes,
                        &mut imports,
                        &mut functions,
                        &mut require_calls,
                        &mut has_cjs_require,
                    );
                }
                "expression_statement" => {
                    // `module.exports = ...` or `exports.foo = ...` or standalone `require(...)`
                    extract_expression_statement(
                        &child,
                        source_bytes,
                        &mut exports,
                        &mut imports,
                        &mut require_calls,
                        &mut has_module_exports,
                        &mut has_cjs_module_exports,
                        &mut has_cjs_require,
                    );
                }
                _ => {}
            }
        }

        // Detect module system from file extension or content
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let module_system = detect_module_system(
            ext,
            has_esm_import,
            has_esm_export,
            has_cjs_require,
            has_cjs_module_exports,
        );

        // Deduplicate by package name: multiple require/import statements for
        // the same package produce a single DependencyUsage entry.
        let mut seen_packages = std::collections::HashSet::new();
        let dependencies_used: Vec<_> = imports
            .iter()
            .filter_map(|imp| ts_dep_from_import(&imp.module, imp.line))
            .filter(|dep| seen_packages.insert(dep.package.clone()))
            .collect();

        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::JavaScript,
            content_hash: String::new(), // filled by parse_file
            imports,
            exports,
            functions,
            types,
            dependencies_used,
            language_ir: LanguageIR::JavaScript(JavaScriptIR {
                module_system,
                has_module_exports,
                require_calls,
            }),
            file_doc,
        })
    }
}

// ---------------------------------------------------------------------------
// Module system detection
// ---------------------------------------------------------------------------

/// Detect the module system based on file extension and code content.
///
/// - `.mjs` → ESM
/// - `.cjs` → CommonJS
/// - `.js`/`.jsx`: heuristic based on import/export vs require/module.exports
fn detect_module_system(
    ext: &str,
    has_esm_import: bool,
    has_esm_export: bool,
    has_cjs_require: bool,
    has_cjs_module_exports: bool,
) -> ModuleSystem {
    match ext {
        "mjs" => ModuleSystem::ESM,
        "cjs" => ModuleSystem::CommonJS,
        _ => {
            let has_esm = has_esm_import || has_esm_export;
            let has_cjs = has_cjs_require || has_cjs_module_exports;
            if has_esm && !has_cjs {
                ModuleSystem::ESM
            } else if has_cjs && !has_esm {
                ModuleSystem::CommonJS
            } else if has_esm && has_cjs {
                // Mixed — ESM takes precedence (unlikely in practice)
                ModuleSystem::ESM
            } else {
                ModuleSystem::Unknown
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Import extraction (ESM)
// ---------------------------------------------------------------------------

/// Extract an `import_statement` into an [`Import`].
///
/// Handles:
/// - `import { Foo, Bar } from 'module';`         (named)
/// - `import Foo from 'module';`                   (default)
/// - `import * as ns from 'module';`               (namespace)
fn extract_import(node: &Node, source: &[u8]) -> Option<Import> {
    let line = node.start_position().row + 1;

    let module = extract_string_value(node, source)?;
    let import_clause = find_child_node(node, "import_clause")?;
    let names = extract_import_names(&import_clause, source);

    if names.is_empty() {
        return None;
    }

    Some(Import {
        module,
        names,
        is_type_only: false, // JS has no type-only imports
        line,
    })
}

// ---------------------------------------------------------------------------
// Export extraction (ESM)
// ---------------------------------------------------------------------------

/// Extract an `export_statement` into exports, functions, types.
fn extract_export(
    node: &Node,
    source: &[u8],
    exports: &mut Vec<Export>,
    functions: &mut Vec<Function>,
    types: &mut Vec<TypeDef>,
) {
    let line = node.start_position().row + 1;
    let is_default = has_child_kind(node, "default");

    // Check for barrel export: `export * from '...'`
    let has_from = has_child_kind(node, "from");
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            if child.kind() == "*" {
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
                        is_type_only: false,
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
                "class_declaration" => {
                    let mut td = extract_class(&child, source);
                    td.is_public = true;
                    let export_name = td.name.clone();
                    types.push(td);
                    exports.push(Export {
                        name: export_name,
                        is_default,
                        is_type_only: false,
                        line,
                    });
                }
                "lexical_declaration" => {
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

// ---------------------------------------------------------------------------
// CommonJS extraction
// ---------------------------------------------------------------------------

/// Extract top-level `const/let/var x = require('...')` or arrow/function expressions.
fn extract_top_level_declaration(
    node: &Node,
    source: &[u8],
    imports: &mut Vec<Import>,
    functions: &mut Vec<Function>,
    require_calls: &mut Vec<String>,
    has_cjs_require: &mut bool,
) {
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            if child.kind() == "variable_declarator" {
                let name = find_child_text(&child, "identifier", source).unwrap_or_default();

                // Check for destructured require first: `const { a, b } = require('module')`
                if let Some(req_info) = extract_destructured_require(&child, source) {
                    *has_cjs_require = true;
                    require_calls.push(req_info.module.clone());
                    imports.push(Import {
                        module: req_info.module,
                        names: req_info.names,
                        is_type_only: false,
                        line: child.start_position().row + 1,
                    });
                } else if let Some(req_module) = extract_require_from_declarator(&child, source) {
                    // const x = require('module')
                    *has_cjs_require = true;
                    require_calls.push(req_module.clone());
                    imports.push(Import {
                        module: req_module,
                        names: vec![name.clone()],
                        is_type_only: false,
                        line: child.start_position().row + 1,
                    });
                } else {
                    // Check for arrow function or function expression
                    let func_node = find_arrow_or_function_expr(&child);

                    if let Some(ref fn_node) = func_node {
                        let is_async = child_has_async_value(&child, source);
                        let parameters = extract_js_ts_parameters(fn_node, source);
                        functions.push(Function {
                            name,
                            is_public: false,
                            is_async,
                            line: child.start_position().row + 1,
                            end_line: child.end_position().row + 1,
                            parameters,
                            // doc_comment for lexical functions is not extracted here.
                            doc_comment: None,
                        });
                    }
                }
            }
        }
    }
}

/// Extract a `require('module')` call from a variable_declarator value.
///
/// Matches: `const x = require('module')` → returns `Some("module")`
fn extract_require_from_declarator(node: &Node, source: &[u8]) -> Option<String> {
    let call = find_child_node(node, "call_expression")?;
    extract_require_module(&call, source)
}

/// Extract module name from a `require(...)` call expression.
fn extract_require_module(call: &Node, source: &[u8]) -> Option<String> {
    let func = call.child(0)?;
    if node_text(&func, source) != "require" {
        return None;
    }
    let args = find_child_node(call, "arguments")?;
    let string_node = find_child_node(&args, "string")?;
    let fragment = find_child_node(&string_node, "string_fragment")?;
    Some(node_text(&fragment, source).to_string())
}

/// Info about a destructured require: `const { a, b } = require('mod')`
struct DestructuredRequire {
    module: String,
    names: Vec<String>,
}

/// Extract destructured require: `const { a, b } = require('module')`
fn extract_destructured_require(node: &Node, source: &[u8]) -> Option<DestructuredRequire> {
    // Look for object_pattern (destructuring) and call_expression (require)
    let pattern = find_child_node(node, "object_pattern")?;
    let call = find_child_node(node, "call_expression")?;
    let module = extract_require_module(&call, source)?;

    let mut names = Vec::new();
    for i in 0..(pattern.child_count() as u32) {
        if let Some(child) = pattern.child(i) {
            match child.kind() {
                "shorthand_property_identifier_pattern" => {
                    names.push(node_text(&child, source).to_string());
                }
                "pair_pattern" => {
                    // `{ a: b }` — extract the key name `a`
                    if let Some(key) = child.child(0) {
                        names.push(node_text(&key, source).to_string());
                    }
                }
                _ => {}
            }
        }
    }

    if names.is_empty() {
        return None;
    }

    Some(DestructuredRequire { module, names })
}

/// Extract `module.exports = ...`, `exports.foo = ...`, or standalone `require(...)`.
#[allow(clippy::too_many_arguments)]
fn extract_expression_statement(
    node: &Node,
    source: &[u8],
    exports: &mut Vec<Export>,
    imports: &mut Vec<Import>,
    require_calls: &mut Vec<String>,
    has_module_exports: &mut bool,
    has_cjs_module_exports: &mut bool,
    has_cjs_require: &mut bool,
) {
    let line = node.start_position().row + 1;

    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "assignment_expression" => {
                    extract_cjs_assignment(
                        &child,
                        source,
                        exports,
                        has_module_exports,
                        has_cjs_module_exports,
                        line,
                    );
                }
                "call_expression" => {
                    // Standalone require('module') call
                    if let Some(module) = extract_require_module(&child, source) {
                        *has_cjs_require = true;
                        require_calls.push(module.clone());
                        imports.push(Import {
                            module,
                            names: vec!["*".to_string()],
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

/// Extract CommonJS assignment patterns.
///
/// Matches:
/// - `module.exports = { ... }` — object with named exports
/// - `module.exports = Foo` — single default-like export
/// - `module.exports.foo = ...` — named member export
/// - `exports.foo = ...` — named member export
fn extract_cjs_assignment(
    node: &Node,
    source: &[u8],
    exports: &mut Vec<Export>,
    has_module_exports: &mut bool,
    has_cjs_module_exports: &mut bool,
    line: usize,
) {
    // The left side is child 0, `=` is child 1, right side is child 2
    let Some(left) = node.child(0) else { return };
    let right = node.child(2);

    let left_text = node_text(&left, source);

    if left_text == "module.exports" {
        *has_module_exports = true;
        *has_cjs_module_exports = true;

        // Check if RHS is an object literal — extract property names as exports
        if let Some(rhs) = right {
            if rhs.kind() == "object" {
                extract_object_exports(&rhs, source, exports, line);
            } else {
                // Single export: `module.exports = Foo`
                let name = node_text(&rhs, source).to_string();
                exports.push(Export {
                    name,
                    is_default: true,
                    is_type_only: false,
                    line,
                });
            }
        }
    } else if left.kind() == "member_expression" {
        // `module.exports.foo = ...` or `exports.foo = ...`
        let object_text = find_member_object_text(&left, source);
        if object_text == "module.exports" || object_text == "exports" {
            *has_module_exports = true;
            *has_cjs_module_exports = true;

            let property = find_member_property_text(&left, source);
            if !property.is_empty() {
                exports.push(Export {
                    name: property.to_string(),
                    is_default: false,
                    is_type_only: false,
                    line,
                });
            }
        }
    }
}

/// Extract property names from an object literal used in `module.exports = { ... }`.
fn extract_object_exports(node: &Node, source: &[u8], exports: &mut Vec<Export>, line: usize) {
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "pair" => {
                    // `{ foo: bar }` — extract key
                    if let Some(key) = child.child(0) {
                        let name = node_text(&key, source).to_string();
                        exports.push(Export {
                            name,
                            is_default: false,
                            is_type_only: false,
                            line,
                        });
                    }
                }
                "shorthand_property_identifier" => {
                    // `{ foo }` shorthand
                    let name = node_text(&child, source).to_string();
                    exports.push(Export {
                        name,
                        is_default: false,
                        is_type_only: false,
                        line,
                    });
                }
                "method_definition" => {
                    // `{ greet() { ... } }` — method in object
                    if let Some(name_node) = find_child_node(&child, "property_identifier") {
                        let name = node_text(&name_node, source).to_string();
                        exports.push(Export {
                            name,
                            is_default: false,
                            is_type_only: false,
                            line,
                        });
                    }
                }
                "spread_element" => {
                    // `{ ...otherExports }` — can't easily resolve, skip
                }
                _ => {}
            }
        }
    }
}

/// Get the object part of a `member_expression` as text.
///
/// For `module.exports.foo`, this returns `"module.exports"`.
/// For `exports.foo`, this returns `"exports"`.
fn find_member_object_text<'a>(node: &Node, source: &'a [u8]) -> &'a str {
    // member_expression has: object, `.`, property
    node.child(0)
        .map(|obj| node_text(&obj, source))
        .unwrap_or("")
}

/// Get the property part of a `member_expression` as text.
///
/// For `module.exports.foo`, this returns `"foo"`.
fn find_member_property_text<'a>(node: &Node, source: &'a [u8]) -> &'a str {
    find_child_node(node, "property_identifier")
        .map(|prop| node_text(&prop, source))
        .unwrap_or("")
}

// ---------------------------------------------------------------------------
// Type (class) extraction
// ---------------------------------------------------------------------------

/// Extract a `class_declaration`.
fn extract_class(node: &Node, source: &[u8]) -> TypeDef {
    let name = find_child_text(node, "identifier", source).unwrap_or_default();
    TypeDef {
        name,
        kind: TypeDefKind::Class,
        is_public: false,
        line: node.start_position().row + 1,
        // doc_comment is set by the caller via collect_js_doc_comment.
        doc_comment: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_js(source: &str) -> ProjectFile {
        let parser = JavaScriptParser;
        parser
            .parse(Path::new("test.js"), source)
            .expect("parse should succeed")
    }

    fn parse_js_ext(source: &str, filename: &str) -> ProjectFile {
        let parser = JavaScriptParser;
        parser
            .parse(Path::new(filename), source)
            .expect("parse should succeed")
    }

    fn js_ir(pf: &ProjectFile) -> &JavaScriptIR {
        match &pf.language_ir {
            LanguageIR::JavaScript(ir) => ir,
            _ => panic!("expected JavaScriptIR"),
        }
    }

    // -- ESM Imports --

    #[test]
    fn extracts_named_import() {
        let pf = parse_js("import { Foo, Bar } from 'module';");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "module");
        assert!(pf.imports[0].names.contains(&"Foo".to_string()));
        assert!(pf.imports[0].names.contains(&"Bar".to_string()));
        assert!(!pf.imports[0].is_type_only);
    }

    #[test]
    fn extracts_default_import() {
        let pf = parse_js("import React from 'react';");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "react");
        assert_eq!(pf.imports[0].names, vec!["React"]);
    }

    #[test]
    fn extracts_namespace_import() {
        let pf = parse_js("import * as utils from './utils';");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "./utils");
        assert_eq!(pf.imports[0].names, vec!["* as utils"]);
    }

    #[test]
    fn extracts_multiple_imports() {
        let source = r#"
import React from 'react';
import { useState, useEffect } from 'react';
import * as fs from 'fs';
"#;
        let pf = parse_js(source);
        assert_eq!(pf.imports.len(), 3);
    }

    // -- ESM Exports --

    #[test]
    fn extracts_named_export_function() {
        let pf = parse_js("export function greet(name) { return `Hello ${name}`; }");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "greet");
        assert!(!pf.exports[0].is_default);

        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_public);
    }

    #[test]
    fn extracts_default_export_function() {
        let pf = parse_js("export default function handler() {}");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "handler");
        assert!(pf.exports[0].is_default);
    }

    #[test]
    fn extracts_async_exported_function() {
        let pf = parse_js("export async function fetchData() {}");
        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_async);
        assert!(pf.functions[0].is_public);
    }

    #[test]
    fn extracts_export_const() {
        let pf = parse_js("export const API_URL = 'http://example.com';");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "API_URL");
    }

    #[test]
    fn extracts_export_const_arrow() {
        let pf = parse_js("export const handler = () => {};");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "handler");

        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "handler");
        assert!(pf.functions[0].is_public);
    }

    #[test]
    fn extracts_export_const_async_arrow() {
        let pf = parse_js("export const handler = async () => {};");
        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_async);
    }

    #[test]
    fn extracts_re_export() {
        let pf = parse_js("export { Foo, Bar } from './module';");
        assert_eq!(pf.exports.len(), 2);
        assert!(
            pf.exports
                .iter()
                .any(|e| e.name.contains("Foo") && e.name.contains("./module"))
        );
    }

    #[test]
    fn extracts_barrel_re_export() {
        let pf = parse_js("export * from './module';");
        assert_eq!(pf.exports.len(), 1);
        assert!(pf.exports[0].name.contains("* from"));
    }

    #[test]
    fn extracts_default_export_identifier() {
        let pf = parse_js("export default App;");
        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "App");
        assert!(pf.exports[0].is_default);
    }

    #[test]
    fn extracts_exported_class() {
        let pf = parse_js("export class UserService {}");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "UserService");
        assert_eq!(pf.types[0].kind, TypeDefKind::Class);
        assert!(pf.types[0].is_public);

        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "UserService");
    }

    // -- CommonJS require --

    #[test]
    fn extracts_require_call() {
        let pf = parse_js("const fs = require('fs');");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "fs");
        assert_eq!(pf.imports[0].names, vec!["fs"]);

        let ir = js_ir(&pf);
        assert!(ir.require_calls.contains(&"fs".to_string()));
    }

    #[test]
    fn extracts_destructured_require() {
        let pf = parse_js("const { readFile, writeFile } = require('fs');");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "fs");
        assert!(pf.imports[0].names.contains(&"readFile".to_string()));
        assert!(pf.imports[0].names.contains(&"writeFile".to_string()));

        let ir = js_ir(&pf);
        assert!(ir.require_calls.contains(&"fs".to_string()));
    }

    #[test]
    fn extracts_multiple_require_calls() {
        let source = r#"
const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');
"#;
        let pf = parse_js(source);
        assert_eq!(pf.imports.len(), 3);

        let ir = js_ir(&pf);
        assert_eq!(ir.require_calls.len(), 3);
    }

    // -- CommonJS module.exports --

    #[test]
    fn extracts_module_exports_object() {
        let source = r#"
function greet() {}
function farewell() {}
module.exports = { greet, farewell };
"#;
        let pf = parse_js(source);
        let ir = js_ir(&pf);
        assert!(ir.has_module_exports);

        assert!(pf.exports.iter().any(|e| e.name == "greet"));
        assert!(pf.exports.iter().any(|e| e.name == "farewell"));
    }

    #[test]
    fn extracts_module_exports_single() {
        let pf = parse_js("module.exports = MyClass;");
        let ir = js_ir(&pf);
        assert!(ir.has_module_exports);

        assert_eq!(pf.exports.len(), 1);
        assert!(pf.exports[0].is_default);
        assert_eq!(pf.exports[0].name, "MyClass");
    }

    #[test]
    fn extracts_exports_member() {
        let source = r#"
exports.greet = function() {};
exports.farewell = function() {};
"#;
        let pf = parse_js(source);
        let ir = js_ir(&pf);
        assert!(ir.has_module_exports);

        assert!(pf.exports.iter().any(|e| e.name == "greet"));
        assert!(pf.exports.iter().any(|e| e.name == "farewell"));
    }

    #[test]
    fn extracts_module_exports_member() {
        let pf = parse_js("module.exports.handler = function() {};");
        let ir = js_ir(&pf);
        assert!(ir.has_module_exports);

        assert_eq!(pf.exports.len(), 1);
        assert_eq!(pf.exports[0].name, "handler");
        assert!(!pf.exports[0].is_default);
    }

    // -- Module system detection --

    #[test]
    fn detects_esm_from_imports() {
        let pf = parse_js("import { foo } from 'bar';");
        let ir = js_ir(&pf);
        assert_eq!(ir.module_system, ModuleSystem::ESM);
    }

    #[test]
    fn detects_esm_from_exports() {
        let pf = parse_js("export function foo() {}");
        let ir = js_ir(&pf);
        assert_eq!(ir.module_system, ModuleSystem::ESM);
    }

    #[test]
    fn detects_commonjs_from_require() {
        let pf = parse_js("const x = require('foo');");
        let ir = js_ir(&pf);
        assert_eq!(ir.module_system, ModuleSystem::CommonJS);
    }

    #[test]
    fn detects_commonjs_from_module_exports() {
        let pf = parse_js("module.exports = {};");
        let ir = js_ir(&pf);
        assert_eq!(ir.module_system, ModuleSystem::CommonJS);
    }

    #[test]
    fn mjs_always_esm() {
        let pf = parse_js_ext("const x = require('foo');", "test.mjs");
        let ir = js_ir(&pf);
        assert_eq!(ir.module_system, ModuleSystem::ESM);
    }

    #[test]
    fn cjs_always_commonjs() {
        let pf = parse_js_ext("import { x } from 'foo';", "test.cjs");
        let ir = js_ir(&pf);
        assert_eq!(ir.module_system, ModuleSystem::CommonJS);
    }

    #[test]
    fn unknown_module_system_for_empty_file() {
        let pf = parse_js("const x = 42;");
        let ir = js_ir(&pf);
        assert_eq!(ir.module_system, ModuleSystem::Unknown);
    }

    // -- Functions --

    #[test]
    fn extracts_non_exported_function() {
        let pf = parse_js("function helper() {}");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "helper");
        assert!(!pf.functions[0].is_public);
    }

    #[test]
    fn extracts_arrow_function() {
        let pf = parse_js("const greet = (name) => {};");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "greet");
        assert!(!pf.functions[0].is_public);
    }

    #[test]
    fn extracts_async_function() {
        let pf = parse_js("async function fetchData() {}");
        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].is_async);
    }

    // -- Classes --

    #[test]
    fn extracts_class() {
        let pf = parse_js("class AppService {}");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "AppService");
        assert_eq!(pf.types[0].kind, TypeDefKind::Class);
        assert!(!pf.types[0].is_public);
    }

    #[test]
    fn extracts_default_export_class() {
        let pf = parse_js("export default class Foo {}");
        assert_eq!(pf.exports.len(), 1);
        assert!(pf.exports[0].is_default);
        assert_eq!(pf.types.len(), 1);
        assert!(pf.types[0].is_public);
    }

    // -- Edge cases --

    #[test]
    fn graceful_on_empty_source() {
        let pf = parse_js("");
        assert!(pf.imports.is_empty());
        assert!(pf.exports.is_empty());
        assert!(pf.functions.is_empty());
        assert!(pf.types.is_empty());
    }

    #[test]
    fn language_is_javascript() {
        let pf = parse_js("const x = 1;");
        assert_eq!(pf.language, Language::JavaScript);
        assert!(matches!(pf.language_ir, LanguageIR::JavaScript(_)));
    }

    #[test]
    fn jsx_file_parses() {
        let source = r#"
import React from 'react';

function App() {
    return <div>Hello</div>;
}

export default App;
"#;
        let pf = parse_js_ext(source, "app.jsx");
        assert_eq!(pf.language, Language::JavaScript);
        assert!(pf.imports.iter().any(|i| i.module == "react"));
        assert!(pf.functions.iter().any(|f| f.name == "App"));
        assert!(pf.exports.iter().any(|e| e.name == "App" && e.is_default));
    }

    #[test]
    fn combined_esm_file() {
        let source = r#"
import { useState } from 'react';
import * as utils from './utils';

export function greet(name) {
    return `Hello, ${name}`;
}

export default function App() {
    return null;
}

export const VERSION = '1.0.0';

class InternalHelper {}

const privateFn = () => {};
"#;
        let pf = parse_js(source);
        assert_eq!(pf.imports.len(), 2);
        assert!(
            pf.functions
                .iter()
                .any(|f| f.name == "greet" && f.is_public)
        );
        assert!(pf.functions.iter().any(|f| f.name == "App" && f.is_public));
        assert!(
            pf.functions
                .iter()
                .any(|f| f.name == "privateFn" && !f.is_public)
        );
        assert!(pf.exports.iter().any(|e| e.name == "greet"));
        assert!(pf.exports.iter().any(|e| e.name == "App" && e.is_default));
        assert!(pf.exports.iter().any(|e| e.name == "VERSION"));
        assert!(pf.types.iter().any(|t| t.name == "InternalHelper"));
    }

    #[test]
    fn combined_commonjs_file() {
        let source = r#"
const fs = require('fs');
const { join } = require('path');

function readConfig(path) {
    return fs.readFileSync(path, 'utf8');
}

function writeConfig(path, data) {
    fs.writeFileSync(path, data);
}

module.exports = { readConfig, writeConfig };
"#;
        let pf = parse_js(source);
        let ir = js_ir(&pf);

        assert_eq!(ir.module_system, ModuleSystem::CommonJS);
        assert!(ir.has_module_exports);
        assert!(ir.require_calls.contains(&"fs".to_string()));
        assert!(ir.require_calls.contains(&"path".to_string()));

        assert_eq!(pf.imports.len(), 2);
        assert!(pf.functions.iter().any(|f| f.name == "readConfig"));
        assert!(pf.functions.iter().any(|f| f.name == "writeConfig"));
        assert!(pf.exports.iter().any(|e| e.name == "readConfig"));
        assert!(pf.exports.iter().any(|e| e.name == "writeConfig"));
    }

    // -----------------------------------------------------------------------
    // Parameter extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn extracts_function_declaration_parameters() {
        let pf = parse_js("function greet(name, age) { return name; }");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "greet");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["name".to_string(), "age".to_string()]
        );
    }

    #[test]
    fn extracts_arrow_function_parameters() {
        let pf = parse_js("const add = (a, b) => a + b;");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "add");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn extracts_exported_function_parameters() {
        let source = r#"
export function process(input, options) { return input; }
"#;
        let pf = parse_js(source);
        let func = pf.functions.iter().find(|f| f.name == "process").unwrap();
        assert_eq!(
            func.parameters,
            vec!["input".to_string(), "options".to_string()]
        );
    }

    #[test]
    fn extracts_export_const_arrow_parameters() {
        let source = r#"
export const multiply = (x, y) => x * y;
"#;
        let pf = parse_js(source);
        let func = pf.functions.iter().find(|f| f.name == "multiply").unwrap();
        assert_eq!(func.parameters, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn extracts_default_parameter_names() {
        let pf = parse_js("function connect(host, port = 3000) {}");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["host".to_string(), "port".to_string()]
        );
    }

    #[test]
    fn no_parameters_for_nullary_function() {
        let pf = parse_js("function init() {}");
        assert!(pf.functions[0].parameters.is_empty());
    }

    #[test]
    fn extracts_commonjs_function_parameters() {
        let source = r#"
function readConfig(path) {
    return path;
}
module.exports = { readConfig };
"#;
        let pf = parse_js(source);
        let func = pf
            .functions
            .iter()
            .find(|f| f.name == "readConfig")
            .unwrap();
        assert_eq!(func.parameters, vec!["path".to_string()]);
    }
}
