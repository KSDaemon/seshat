//! Tree-sitter–based Python parser.
//!
//! Extracts imports (`import x`, `from x import y`), exports (`__all__`),
//! functions (sync/async, decorated), classes (with bases), and
//! Python-specific IR from source files.

use std::path::Path;

use seshat_core::{
    Export, Function, Import, Language, LanguageIR, ProjectFile, PythonIR, TypeDef, TypeDefKind,
};
use tree_sitter::{Node, Parser as TsParser};

use super::{
    Parser, extract_python_docstring, find_child_node, find_child_text, node_text,
    python_dep_from_import,
};
use crate::ScanError;

/// Parser for Python source files.
pub struct PythonParser;

impl Parser for PythonParser {
    fn parse(&self, path: &Path, source: &str) -> Result<ProjectFile, ScanError> {
        let mut ts_parser = TsParser::new();
        ts_parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
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
        let source_bytes = source.as_bytes();

        let mut imports = Vec::new();
        let mut exports = Vec::new();
        let mut functions = Vec::new();
        let mut types = Vec::new();
        let mut decorators: Vec<String> = Vec::new();
        let mut has_all_export = false;
        let mut type_hints_used = false;
        let mut all_decorators: Vec<String> = Vec::new();

        let is_init_file = path
            .file_name()
            .and_then(|f| f.to_str())
            .map(|f| f == "__init__.py")
            .unwrap_or(false);

        // Module-level docstring: the first named child that is an
        // `expression_statement` containing a bare `string` literal.
        let file_doc = extract_python_docstring(&root, source_bytes);

        for i in 0..(root.child_count() as u32) {
            let Some(child) = root.child(i) else { continue };
            match child.kind() {
                "import_statement" => {
                    if let Some(imp) = extract_import_statement(&child, source_bytes) {
                        imports.push(imp);
                    }
                }
                "import_from_statement" => {
                    if let Some(imp) = extract_import_from_statement(&child, source_bytes) {
                        imports.push(imp);
                    }
                }
                "function_definition" => {
                    let mut func = extract_function(&child, source_bytes, &mut type_hints_used);
                    // Docstring is the first statement of the function body.
                    if let Some(body) = find_child_node(&child, "block") {
                        func.doc_comment = extract_python_docstring(&body, source_bytes);
                    }
                    all_decorators.append(&mut decorators);
                    functions.push(func);
                }
                "class_definition" => {
                    let mut td = extract_class(&child, source_bytes, &mut type_hints_used);
                    // Docstring is the first statement of the class body.
                    if let Some(body) = find_child_node(&child, "block") {
                        td.doc_comment = extract_python_docstring(&body, source_bytes);
                    }
                    all_decorators.append(&mut decorators);
                    types.push(td);
                }
                "decorated_definition" => {
                    extract_decorated_definition(
                        &child,
                        source_bytes,
                        &mut functions,
                        &mut types,
                        &mut decorators,
                        &mut all_decorators,
                        &mut type_hints_used,
                    );
                }
                "expression_statement" => {
                    // Check for __all__ = [...] assignment
                    if let Some(all_exports) = extract_all_assignment(&child, source_bytes) {
                        has_all_export = true;
                        for name in all_exports {
                            exports.push(Export {
                                name,
                                is_default: false,
                                is_type_only: false,
                                line: child.start_position().row + 1,
                            });
                        }
                    }
                }
                _ => {
                    // Clear pending decorators on non-decorator, non-definition nodes
                    // (but keep them for decorated_definition which starts with decorator)
                    if child.kind() != "comment" {
                        decorators.clear();
                    }
                }
            }
        }

        // Deduplicate by package name: `import os` and `from os import path`
        // both yield `os` — keep only the first occurrence per package.
        let mut seen_packages = std::collections::HashSet::new();
        let dependencies_used: Vec<_> = imports
            .iter()
            .filter_map(|imp| python_dep_from_import(&imp.module, imp.line))
            .filter(|dep| seen_packages.insert(dep.package.clone()))
            .collect();

        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::Python,
            content_hash: String::new(), // filled by parse_file
            imports,
            exports,
            functions,
            types,
            dependencies_used,
            language_ir: LanguageIR::Python(PythonIR {
                has_all_export,
                is_init_file,
                type_hints_used,
                decorators: all_decorators,
            }),
            file_doc,
        })
    }
}

// ---------------------------------------------------------------------------
// Import extraction
// ---------------------------------------------------------------------------

/// Extract `import x` or `import x.y.z` or `import x as alias`.
///
/// Python's `import_statement` has children like:
/// - `import` keyword
/// - `dotted_name` or `aliased_import`
fn extract_import_statement(node: &Node, source: &[u8]) -> Option<Import> {
    let line = node.start_position().row + 1;
    let mut names = Vec::new();
    let mut module = String::new();

    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "dotted_name" => {
                    let name = node_text(&child, source).to_string();
                    if module.is_empty() {
                        module = name.clone();
                    }
                    names.push(name);
                }
                "aliased_import" => {
                    if let Some(dotted) = find_child_node(&child, "dotted_name") {
                        let name = node_text(&dotted, source).to_string();
                        if module.is_empty() {
                            module = name.clone();
                        }
                        // Use alias if present
                        if let Some(alias) = find_child_text(&child, "identifier", source) {
                            names.push(alias);
                        } else {
                            names.push(name);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if module.is_empty() {
        return None;
    }

    Some(Import {
        module,
        names,
        is_type_only: false,
        line,
    })
}

/// Extract `from x import y, z` or `from . import utils` or `from .models import User`.
///
/// Python's `import_from_statement` has children like:
/// - `from` keyword
/// - `dotted_name` or `relative_import` (the module)
/// - `import` keyword
/// - names: `dotted_name`, `aliased_import`, or `wildcard_import`
fn extract_import_from_statement(node: &Node, source: &[u8]) -> Option<Import> {
    let line = node.start_position().row + 1;

    let mut module = String::new();
    let mut names = Vec::new();
    let mut past_from = false;
    let mut past_import = false;

    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "from" => {
                    past_from = true;
                }
                "import" => {
                    past_import = true;
                }
                "dotted_name" if past_from && !past_import => {
                    module = node_text(&child, source).to_string();
                }
                "relative_import" => {
                    // Handles `from . import x` or `from .models import x`
                    module = node_text(&child, source).to_string();
                }
                "dotted_name" if past_import => {
                    names.push(node_text(&child, source).to_string());
                }
                "aliased_import" if past_import => {
                    if let Some(dotted) = find_child_node(&child, "dotted_name") {
                        let name = node_text(&dotted, source).to_string();
                        if let Some(alias) = find_child_text(&child, "identifier", source) {
                            names.push(alias);
                        } else {
                            names.push(name);
                        }
                    } else if let Some(ident) = find_child_text(&child, "identifier", source) {
                        names.push(ident);
                    }
                }
                "wildcard_import" => {
                    names.push("*".to_string());
                }
                _ => {}
            }
        }
    }

    if module.is_empty() && names.is_empty() {
        return None;
    }

    Some(Import {
        module,
        names,
        is_type_only: false,
        line,
    })
}

// ---------------------------------------------------------------------------
// Function extraction
// ---------------------------------------------------------------------------

/// Extract a `function_definition` node.
///
/// Detects `async` from the `async` keyword child node inside the definition.
fn extract_function(node: &Node, source: &[u8], type_hints_used: &mut bool) -> Function {
    let name = find_child_text(node, "identifier", source).unwrap_or_default();
    let line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;

    // Detect async: tree-sitter-python puts `async` keyword as a child of function_definition
    let is_async = find_child_node(node, "async").is_some();

    // Check for type hints: return type annotation or parameter annotations
    if has_type_annotations(node, source) {
        *type_hints_used = true;
    }

    let parameters = extract_python_parameters(node, source);

    Function {
        name,
        is_public: false, // Python doesn't have explicit visibility; all top-level are "public"
        is_async,
        line,
        end_line,
        parameters,
        // doc_comment is set by the caller via extract_python_docstring on the function body.
        doc_comment: None,
    }
}

/// Extract parameter names from a Python `function_definition` node.
///
/// Walks the `parameters` child and extracts identifier names from each
/// parameter type. Excludes `self` and `cls` as they are implicit.
fn extract_python_parameters(func_node: &Node, source: &[u8]) -> Vec<String> {
    let Some(params) = find_child_node(func_node, "parameters") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for i in 0..(params.child_count() as u32) {
        let Some(child) = params.child(i) else {
            continue;
        };
        let param_name = match child.kind() {
            // Simple parameter: `def f(x):`
            "identifier" => Some(node_text(&child, source).to_string()),
            // Typed parameter: `def f(x: int):`
            "typed_parameter" => find_child_text(&child, "identifier", source),
            // Default parameter: `def f(x=5):`
            "default_parameter" => {
                // First child is the parameter name
                child
                    .child(0)
                    .filter(|c| c.kind() == "identifier")
                    .map(|c| node_text(&c, source).to_string())
            }
            // Typed default parameter: `def f(x: int = 5):`
            "typed_default_parameter" => find_child_text(&child, "identifier", source),
            // *args
            "list_splat_pattern" => find_child_text(&child, "identifier", source),
            // **kwargs
            "dictionary_splat_pattern" => find_child_text(&child, "identifier", source),
            _ => None,
        };
        if let Some(name) = param_name {
            // Skip self and cls — they are implicit in Python methods
            if !name.is_empty() && name != "self" && name != "cls" {
                names.push(name);
            }
        }
    }
    names
}

/// Check if a function has type annotations (parameter annotations or return type).
fn has_type_annotations(node: &Node, _source: &[u8]) -> bool {
    // Check return type: `-> type`
    if find_child_node(node, "type").is_some() {
        return true;
    }

    // Check parameter annotations in the `parameters` node
    if let Some(params) = find_child_node(node, "parameters") {
        for i in 0..(params.child_count() as u32) {
            if let Some(param) = params.child(i) {
                match param.kind() {
                    "typed_parameter" | "typed_default_parameter" => return true,
                    // Also check inside *args, **kwargs
                    "list_splat_pattern" | "dictionary_splat_pattern" => {
                        if find_child_node(&param, "type").is_some() {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Class extraction
// ---------------------------------------------------------------------------

/// Extract a `class_definition` node.
///
/// Also scans the class body for type annotations (annotated assignments,
/// typed method parameters) to detect type hint usage.
fn extract_class(node: &Node, source: &[u8], type_hints_used: &mut bool) -> TypeDef {
    let name = find_child_text(node, "identifier", source).unwrap_or_default();
    let line = node.start_position().row + 1;

    // Scan class body for type hints
    if let Some(body) = find_child_node(node, "block") {
        check_body_for_type_hints(&body, source, type_hints_used);
    }

    TypeDef {
        name,
        kind: TypeDefKind::Class,
        is_public: false,
        line,
        // doc_comment is set by the caller via extract_python_docstring on the class body.
        doc_comment: None,
    }
}

/// Check a class body block for type annotations.
fn check_body_for_type_hints(body: &Node, source: &[u8], type_hints_used: &mut bool) {
    if *type_hints_used {
        return; // already detected
    }
    for i in 0..(body.child_count() as u32) {
        if let Some(child) = body.child(i) {
            match child.kind() {
                // Annotated assignment: `name: str = "default"` or `name: str`
                "expression_statement" => {
                    if let Some(inner) = child.child(0) {
                        if inner.kind() == "assignment" {
                            // Check for type annotation on left side
                            if find_child_node(&inner, "type").is_some() {
                                *type_hints_used = true;
                                return;
                            }
                        }
                        if inner.kind() == "type" {
                            *type_hints_used = true;
                            return;
                        }
                    }
                }
                "function_definition" | "decorated_definition" => {
                    let func_node = if child.kind() == "decorated_definition" {
                        find_child_node(&child, "function_definition")
                    } else {
                        Some(child)
                    };
                    if let Some(func) = func_node {
                        if has_type_annotations(&func, source) {
                            *type_hints_used = true;
                            return;
                        }
                    }
                }
                // type alias style: annotated assignment at class level
                "type_alias_statement" => {
                    *type_hints_used = true;
                    return;
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decorated definitions
// ---------------------------------------------------------------------------

/// Extract a `decorated_definition` which wraps decorators + function/class.
fn extract_decorated_definition(
    node: &Node,
    source: &[u8],
    functions: &mut Vec<Function>,
    types: &mut Vec<TypeDef>,
    pending_decorators: &mut Vec<String>,
    all_decorators: &mut Vec<String>,
    type_hints_used: &mut bool,
) {
    let mut local_decorators: Vec<String> = Vec::new();

    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "decorator" => {
                    let dec_text = extract_decorator_name(&child, source);
                    if !dec_text.is_empty() {
                        local_decorators.push(dec_text);
                    }
                }
                "function_definition" => {
                    let func = extract_function(&child, source, type_hints_used);
                    all_decorators.append(&mut local_decorators);
                    all_decorators.append(pending_decorators);
                    functions.push(func);
                }
                "class_definition" => {
                    let td = extract_class(&child, source, type_hints_used);
                    all_decorators.append(&mut local_decorators);
                    all_decorators.append(pending_decorators);
                    types.push(td);
                }
                _ => {}
            }
        }
    }
}

/// Extract the decorator name from a `decorator` node.
///
/// For `@dataclass` → "dataclass"
/// For `@app.route("/api")` → "app.route"
/// For `@property` → "property"
fn extract_decorator_name(node: &Node, source: &[u8]) -> String {
    // Decorator node children: `@`, then the expression (identifier, attribute, call)
    for i in 0..(node.child_count() as u32) {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "identifier" => {
                    return node_text(&child, source).to_string();
                }
                "attribute" => {
                    // e.g., `app.route` — return full dotted name
                    return node_text(&child, source).to_string();
                }
                "call" => {
                    // e.g., `app.route("/api")` — extract function part
                    if let Some(func) = child.child(0) {
                        return node_text(&func, source).to_string();
                    }
                }
                _ => {}
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// __all__ extraction
// ---------------------------------------------------------------------------

/// Extract `__all__ = [...]` assignment from an expression_statement.
///
/// Returns `Some(Vec<name>)` if the expression is an `__all__` assignment.
fn extract_all_assignment(node: &Node, source: &[u8]) -> Option<Vec<String>> {
    // expression_statement → assignment → left = __all__, right = list
    let assign = find_child_node(node, "assignment")?;

    // Left side should be `__all__`
    let left = assign.child(0)?;
    if node_text(&left, source) != "__all__" {
        return None;
    }

    // Right side should be a list
    let right = find_child_node(&assign, "list")?;

    let mut names = Vec::new();
    for i in 0..(right.child_count() as u32) {
        if let Some(child) = right.child(i) {
            if child.kind() == "string" {
                let text = extract_string_content(&child, source);
                if !text.is_empty() {
                    names.push(text);
                }
            }
        }
    }

    if names.is_empty() { None } else { Some(names) }
}

/// Extract the content of a Python string node (without quotes).
fn extract_string_content(node: &Node, source: &[u8]) -> String {
    // Python string nodes have `string_content` or `string_start`/`string_end` children
    // Or we can get the full text and strip quotes
    if let Some(content) = find_child_node(node, "string_content") {
        return node_text(&content, source).to_string();
    }

    // Fallback: strip quotes from full text
    let text = node_text(node, source);
    let stripped = text
        .strip_prefix("\"\"\"")
        .and_then(|s| s.strip_suffix("\"\"\""))
        .or_else(|| text.strip_prefix("'''").and_then(|s| s.strip_suffix("'''")))
        .or_else(|| text.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(text);
    stripped.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_py(source: &str) -> ProjectFile {
        let parser = PythonParser;
        parser
            .parse(Path::new("test.py"), source)
            .expect("parse should succeed")
    }

    fn parse_py_path(source: &str, filename: &str) -> ProjectFile {
        let parser = PythonParser;
        parser
            .parse(Path::new(filename), source)
            .expect("parse should succeed")
    }

    fn py_ir(pf: &ProjectFile) -> &PythonIR {
        match &pf.language_ir {
            LanguageIR::Python(ir) => ir,
            _ => panic!("expected PythonIR"),
        }
    }

    // -- Import statements --

    #[test]
    fn extracts_simple_import() {
        let pf = parse_py("import os");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "os");
        assert_eq!(pf.imports[0].names, vec!["os"]);
        assert!(!pf.imports[0].is_type_only);
    }

    #[test]
    fn extracts_dotted_import() {
        let pf = parse_py("import os.path");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "os.path");
        assert_eq!(pf.imports[0].names, vec!["os.path"]);
    }

    #[test]
    fn extracts_aliased_import() {
        let pf = parse_py("import numpy as np");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "numpy");
        assert_eq!(pf.imports[0].names, vec!["np"]);
    }

    #[test]
    fn extracts_from_import() {
        let pf = parse_py("from pathlib import Path");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "pathlib");
        assert_eq!(pf.imports[0].names, vec!["Path"]);
    }

    #[test]
    fn extracts_from_import_multiple() {
        let pf = parse_py("from typing import Optional, List, Dict");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "typing");
        assert_eq!(pf.imports[0].names.len(), 3);
        assert!(pf.imports[0].names.contains(&"Optional".to_string()));
        assert!(pf.imports[0].names.contains(&"List".to_string()));
        assert!(pf.imports[0].names.contains(&"Dict".to_string()));
    }

    #[test]
    fn extracts_relative_import() {
        let pf = parse_py("from . import utils");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, ".");
        assert_eq!(pf.imports[0].names, vec!["utils"]);
    }

    #[test]
    fn extracts_relative_import_from_submodule() {
        let pf = parse_py("from .models import User, Role");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, ".models");
        assert_eq!(pf.imports[0].names.len(), 2);
        assert!(pf.imports[0].names.contains(&"User".to_string()));
        assert!(pf.imports[0].names.contains(&"Role".to_string()));
    }

    #[test]
    fn extracts_wildcard_import() {
        let pf = parse_py("from os.path import *");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "os.path");
        assert_eq!(pf.imports[0].names, vec!["*"]);
    }

    #[test]
    fn extracts_multiple_import_statements() {
        let source = r#"
import os
import sys
from pathlib import Path
from typing import Optional
"#;
        let pf = parse_py(source);
        assert_eq!(pf.imports.len(), 4);
    }

    #[test]
    fn extracts_aliased_from_import() {
        let pf = parse_py("from collections import OrderedDict as OD");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "collections");
        assert_eq!(pf.imports[0].names, vec!["OD"]);
    }

    // -- Functions --

    #[test]
    fn extracts_simple_function() {
        let pf = parse_py("def greet():\n    pass");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "greet");
        assert!(!pf.functions[0].is_async);
    }

    #[test]
    fn extracts_function_with_type_hints() {
        let pf = parse_py("def greet(name: str) -> str:\n    return f'Hello, {name}'");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "greet");
        let ir = py_ir(&pf);
        assert!(ir.type_hints_used);
    }

    #[test]
    fn extracts_async_function_standalone() {
        let source = "async def fetch_data():\n    pass\n";
        let pf = parse_py(source);
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "fetch_data");
        assert!(pf.functions[0].is_async);
    }

    #[test]
    fn extracts_decorated_async_function() {
        let source = r#"
@some_decorator
async def fetch_data(url: str) -> dict:
    pass
"#;
        let pf = parse_py(source);
        assert!(!pf.functions.is_empty());
        let func = pf
            .functions
            .iter()
            .find(|f| f.name == "fetch_data")
            .unwrap();
        assert!(func.is_async);
        let ir = py_ir(&pf);
        assert!(ir.type_hints_used);
    }

    #[test]
    fn extracts_multiple_functions() {
        let source = r#"
def foo():
    pass

def bar():
    pass
"#;
        let pf = parse_py(source);
        assert_eq!(pf.functions.len(), 2);
        assert!(pf.functions.iter().any(|f| f.name == "foo"));
        assert!(pf.functions.iter().any(|f| f.name == "bar"));
    }

    // -- Classes --

    #[test]
    fn extracts_simple_class() {
        let pf = parse_py("class MyClass:\n    pass");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "MyClass");
        assert_eq!(pf.types[0].kind, TypeDefKind::Class);
    }

    #[test]
    fn extracts_class_with_bases() {
        let pf = parse_py("class DerivedClass(BaseClass, MixinClass):\n    pass");
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "DerivedClass");
    }

    #[test]
    fn extracts_decorated_class() {
        let source = r#"
@dataclass
class Config:
    host: str = "localhost"
    port: int = 8080
"#;
        let pf = parse_py(source);
        assert_eq!(pf.types.len(), 1);
        assert_eq!(pf.types[0].name, "Config");
        let ir = py_ir(&pf);
        assert!(ir.decorators.contains(&"dataclass".to_string()));
    }

    // -- __all__ exports --

    #[test]
    fn extracts_all_export() {
        let pf = parse_py(r#"__all__ = ["MyClass", "my_function"]"#);
        let ir = py_ir(&pf);
        assert!(ir.has_all_export);
        assert_eq!(pf.exports.len(), 2);
        assert!(pf.exports.iter().any(|e| e.name == "MyClass"));
        assert!(pf.exports.iter().any(|e| e.name == "my_function"));
    }

    #[test]
    fn no_all_export_when_absent() {
        let pf = parse_py("x = 42");
        let ir = py_ir(&pf);
        assert!(!ir.has_all_export);
        assert!(pf.exports.is_empty());
    }

    // -- __init__.py detection --

    #[test]
    fn detects_init_file() {
        let pf = parse_py_path("", "__init__.py");
        let ir = py_ir(&pf);
        assert!(ir.is_init_file);
    }

    #[test]
    fn non_init_file() {
        let pf = parse_py_path("", "main.py");
        let ir = py_ir(&pf);
        assert!(!ir.is_init_file);
    }

    // -- Decorators --

    #[test]
    fn extracts_simple_decorator() {
        let source = r#"
@property
def name(self):
    return self._name
"#;
        let pf = parse_py(source);
        let ir = py_ir(&pf);
        assert!(ir.decorators.contains(&"property".to_string()));
    }

    #[test]
    fn extracts_dotted_decorator() {
        let source = r#"
@app.route("/api")
def api_handler():
    pass
"#;
        let pf = parse_py(source);
        let ir = py_ir(&pf);
        assert!(ir.decorators.contains(&"app.route".to_string()));
    }

    #[test]
    fn extracts_multiple_decorators() {
        let source = r#"
@app.route("/api")
@login_required
def api_handler():
    pass
"#;
        let pf = parse_py(source);
        let ir = py_ir(&pf);
        assert!(ir.decorators.contains(&"app.route".to_string()));
        assert!(ir.decorators.contains(&"login_required".to_string()));
    }

    // -- Type hints --

    #[test]
    fn detects_type_hints_in_params() {
        let pf = parse_py("def greet(name: str):\n    pass");
        let ir = py_ir(&pf);
        assert!(ir.type_hints_used);
    }

    #[test]
    fn detects_type_hints_return() {
        let pf = parse_py("def greet() -> str:\n    pass");
        let ir = py_ir(&pf);
        assert!(ir.type_hints_used);
    }

    #[test]
    fn no_type_hints_when_absent() {
        let pf = parse_py("def greet(name):\n    pass");
        let ir = py_ir(&pf);
        assert!(!ir.type_hints_used);
    }

    // -- Edge cases --

    #[test]
    fn graceful_on_empty_source() {
        let pf = parse_py("");
        assert!(pf.imports.is_empty());
        assert!(pf.exports.is_empty());
        assert!(pf.functions.is_empty());
        assert!(pf.types.is_empty());
    }

    #[test]
    fn language_is_python() {
        let pf = parse_py("x = 1");
        assert_eq!(pf.language, Language::Python);
        assert!(matches!(pf.language_ir, LanguageIR::Python(_)));
    }

    #[test]
    fn combined_python_file() {
        let source = r#"
import os
from pathlib import Path
from typing import Optional

__all__ = ["Config", "load_config"]

@dataclass
class Config:
    host: str = "localhost"
    port: int = 8080

def load_config(path: Path) -> Optional[Config]:
    pass

async def save_config(config: Config) -> None:
    pass
"#;
        let pf = parse_py(source);
        let ir = py_ir(&pf);

        assert_eq!(pf.imports.len(), 3);
        assert!(ir.has_all_export);
        assert_eq!(pf.exports.len(), 2);
        assert!(pf.types.iter().any(|t| t.name == "Config"));
        assert!(pf.functions.iter().any(|f| f.name == "load_config"));
        assert!(
            pf.functions
                .iter()
                .any(|f| f.name == "save_config" && f.is_async)
        );
        assert!(ir.type_hints_used);
        assert!(ir.decorators.contains(&"dataclass".to_string()));
    }

    // -----------------------------------------------------------------------
    // Parameter extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn extracts_simple_parameters() {
        let pf = parse_py("def greet(name, age):\n    pass");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "greet");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["name".to_string(), "age".to_string()]
        );
    }

    #[test]
    fn extracts_typed_parameters() {
        let pf = parse_py("def process(input: str, count: int) -> bool:\n    pass");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["input".to_string(), "count".to_string()]
        );
    }

    #[test]
    fn extracts_default_parameters() {
        let pf = parse_py("def connect(host, port=3000):\n    pass");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["host".to_string(), "port".to_string()]
        );
    }

    #[test]
    fn extracts_typed_default_parameters() {
        let pf = parse_py("def connect(host: str, port: int = 3000):\n    pass");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["host".to_string(), "port".to_string()]
        );
    }

    #[test]
    fn extracts_args_kwargs() {
        let pf = parse_py("def variadic(*args, **kwargs):\n    pass");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["args".to_string(), "kwargs".to_string()]
        );
    }

    #[test]
    fn excludes_self_parameter() {
        // Test that `self` is excluded from parameter names.
        // Use a top-level function since the parser doesn't extract class methods
        // into the top-level functions list, but `self` filtering still applies.
        let pf = parse_py("def bar(self, x, y):\n    pass");
        assert_eq!(pf.functions[0].name, "bar");
        // "self" is excluded
        assert_eq!(
            pf.functions[0].parameters,
            vec!["x".to_string(), "y".to_string()]
        );
    }

    #[test]
    fn excludes_cls_parameter() {
        let pf = parse_py("def create(cls, name):\n    pass");
        assert_eq!(pf.functions[0].name, "create");
        // "cls" is excluded
        assert_eq!(pf.functions[0].parameters, vec!["name".to_string()]);
    }

    #[test]
    fn no_parameters_for_nullary_function() {
        let pf = parse_py("def init():\n    pass");
        assert!(pf.functions[0].parameters.is_empty());
    }

    #[test]
    fn extracts_async_function_parameters() {
        let pf = parse_py("async def fetch(url, timeout):\n    pass");
        assert!(pf.functions[0].is_async);
        assert_eq!(
            pf.functions[0].parameters,
            vec!["url".to_string(), "timeout".to_string()]
        );
    }

    #[test]
    fn extracts_function_docstring() {
        let source = r#"
def get_user(user_id):
    """Return the user with the given ID."""
    return None
"#;
        let pf = parse_py(source);
        assert_eq!(pf.functions.len(), 1);
        let doc = pf.functions[0].doc_comment.as_deref().unwrap_or("");
        assert!(
            doc.contains("Return the user with the given ID."),
            "got: {doc}"
        );
    }

    #[test]
    fn extracts_multiline_docstring() {
        let source = "def process():\n    \"\"\"\n    Process items.\n    Returns count.\n    \"\"\"\n    pass";
        let pf = parse_py(source);
        let doc = pf.functions[0].doc_comment.as_deref().unwrap_or("");
        assert!(doc.contains("Process items."), "got: {doc}");
    }

    #[test]
    fn function_without_docstring_is_none() {
        let pf = parse_py("def no_doc():\n    pass");
        assert!(pf.functions[0].doc_comment.is_none());
    }

    #[test]
    fn extracts_module_docstring_as_file_doc() {
        let source = r#""""Module for user management."""

def get_user():
    pass
"#;
        let pf = parse_py(source);
        let file_doc = pf.file_doc.as_deref().unwrap_or("");
        assert!(
            file_doc.contains("Module for user management."),
            "got: {file_doc}"
        );
    }

    #[test]
    fn file_without_module_docstring_has_no_file_doc() {
        let pf = parse_py("def foo():\n    pass");
        assert!(pf.file_doc.is_none());
    }

    #[test]
    fn extracts_external_python_dependencies() {
        let source = r#"
import os
import sys
import requests
from fastapi import FastAPI
from . import local_module
from typing import Optional
"#;
        let pf = parse_py(source);
        let packages: Vec<&str> = pf
            .dependencies_used
            .iter()
            .map(|d| d.package.as_str())
            .collect();
        assert!(
            packages.contains(&"requests"),
            "requests missing: {packages:?}"
        );
        assert!(
            packages.contains(&"fastapi"),
            "fastapi missing: {packages:?}"
        );
        // stdlib and relative must be excluded.
        assert!(
            !packages.contains(&"os"),
            "os must be excluded: {packages:?}"
        );
        assert!(
            !packages.contains(&"sys"),
            "sys must be excluded: {packages:?}"
        );
        assert!(
            !packages.contains(&"typing"),
            "typing must be excluded: {packages:?}"
        );
    }

    #[test]
    fn stdlib_only_python_file_has_no_dependencies() {
        let source = "import os\nimport sys\nfrom typing import List";
        let pf = parse_py(source);
        assert!(
            pf.dependencies_used.is_empty(),
            "stdlib-only file must have no external deps: {:?}",
            pf.dependencies_used
        );
    }
}
