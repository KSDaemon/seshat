//! Tree-sitter–based Rust parser.
//!
//! Extracts imports (`use`), functions, types (structs, enums, traits),
//! exports (pub items), mod declarations, derive macros, trait implementations,
//! and error types from Rust source files.

use std::collections::VecDeque;
use std::path::Path;

use seshat_core::{
    DeriveUsage, Export, Function, FunctionCall, Import, Language, LanguageIR, MacroCall,
    ModDeclaration, ProjectFile, RustIR, TraitImpl, TypeDef, TypeDefKind,
};
use tree_sitter::{Node, Parser as TsParser};

use super::{
    Parser, collect_rust_doc_comment, find_child_node, find_child_text, node_text,
    rust_dep_from_import,
};
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
        let mut mod_declarations: Vec<ModDeclaration> = Vec::new();
        let mut derive_macros: Vec<DeriveUsage> = Vec::new();
        let mut trait_implementations = Vec::new();
        let mut error_types = Vec::new();
        let mut macro_calls: Vec<MacroCall> = Vec::new();
        let mut function_calls: Vec<FunctionCall> = Vec::new();

        // Track pending derive attributes for the next item
        let mut pending_derives: Vec<(Vec<String>, usize)> = Vec::new();

        let source_bytes = source.as_bytes();

        // Collect file-level //! inner doc comments from the top of the file.
        let file_doc = extract_rust_file_doc(&root, source_bytes);

        for i in 0..root.child_count() {
            let Some(child) = root.child(i as u32) else {
                continue;
            };
            match child.kind() {
                "use_declaration" => {
                    // A single `use_declaration` can expand into multiple
                    // [`Import`]s when the brace list contains nested groups
                    // (e.g. `use std::{io::{self, Read}, fmt};`). Each leaf
                    // group becomes its own import so downstream consumers
                    // see one (module, names) pair per import target.
                    imports.extend(extract_use_declaration(&child, source_bytes));
                }
                "function_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let mut func = extract_function(&child, source_bytes, is_pub);
                    func.doc_comment = collect_rust_doc_comment(&child, source_bytes);
                    if is_pub {
                        exports.push(Export {
                            name: func.name.clone(),
                            is_default: false,
                            is_type_only: false,
                            line: func.line,
                            end_line: func.end_line,
                        });
                    }
                    // Apply any pending derives (shouldn't happen for functions, but drain anyway)
                    pending_derives.clear();
                    functions.push(func);
                }
                "struct_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let mut td =
                        extract_type_def(&child, source_bytes, TypeDefKind::Struct, is_pub);
                    td.doc_comment = collect_rust_doc_comment(&child, source_bytes);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                            end_line: td.end_line,
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
                    let mut td = extract_type_def(&child, source_bytes, TypeDefKind::Enum, is_pub);
                    td.doc_comment = collect_rust_doc_comment(&child, source_bytes);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                            end_line: td.end_line,
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
                    let mut td = extract_type_def(&child, source_bytes, TypeDefKind::Trait, is_pub);
                    td.doc_comment = collect_rust_doc_comment(&child, source_bytes);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                            end_line: td.end_line,
                        });
                    }
                    pending_derives.clear();
                    types.push(td);
                }
                "type_item" => {
                    let is_pub = has_visibility_modifier(&child);
                    let mut td =
                        extract_type_def(&child, source_bytes, TypeDefKind::TypeAlias, is_pub);
                    td.doc_comment = collect_rust_doc_comment(&child, source_bytes);
                    if is_pub {
                        exports.push(Export {
                            name: td.name.clone(),
                            is_default: false,
                            is_type_only: true,
                            line: td.line,
                            end_line: td.end_line,
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
                    if let Some(decl) = extract_mod_declaration(&child, source_bytes) {
                        mod_declarations.push(decl);
                    }
                    pending_derives.clear();
                }
                // NOTE: `macro_invocation` is intentionally NOT handled here.
                // collect_macro_calls_recursive (called after this loop) already
                // walks the entire tree from root — including top-level macro
                // invocations.  Handling them here AND there would double-count
                // every module-scope macro call (e.g. tracing::info! at the top
                // of a file), inflating adoption scores in logging detectors.
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

        // Walk the entire tree to collect ALL macro_invocation nodes —
        // top-level, inside function bodies, impl blocks, etc.
        // This is the single authoritative collection point; the main loop
        // above deliberately does NOT handle "macro_invocation" to avoid
        // double-counting.
        collect_macro_calls_recursive(&root, source_bytes, &mut macro_calls);

        // Collect deduplicated function call-sites for query_code_pattern enrichment.
        // One example per unique callee name, up to MAX_FUNCTION_CALLS_PER_FILE.
        super::collect_calls_bfs(
            &root,
            source,
            "call_expression",
            &["token_tree"],
            extract_function_call,
            &mut function_calls,
        );

        // Build a same-file local-mod set so `pub use <mod>::Item;` (or
        // `use <mod>::Item;`) referring to a sibling submodule declared with
        // `mod <name>;` / `pub mod <name>;` in this file is NOT classified
        // as an external crate. Without this filter, every `pub use` re-export
        // of a child module in a `lib.rs` (e.g. `pub use code_pattern::Foo;`
        // next to `pub mod code_pattern;`) leaks into `external_dependencies`.
        let local_mod_names: std::collections::HashSet<&str> =
            mod_declarations.iter().map(|m| m.name.as_str()).collect();

        // Deduplicate by package name: multiple `use serde::Serialize; use
        // serde::Deserialize;` statements map to the same external package.
        // Keep only the first occurrence (lowest line number) per package.
        let mut seen_packages = std::collections::HashSet::new();
        let dependencies_used: Vec<_> = imports
            .iter()
            .filter_map(|imp| rust_dep_from_import(&imp.module, imp.line))
            .filter(|dep| !local_mod_names.contains(dep.package.as_str()))
            .filter(|dep| seen_packages.insert(dep.package.clone()))
            .collect();

        Ok(ProjectFile {
            path: path.to_path_buf(),
            language: Language::Rust,
            content_hash: String::new(), // filled by parse_file
            imports,
            exports,
            functions,
            types,
            dependencies_used,
            language_ir: LanguageIR::Rust(RustIR {
                mod_declarations,
                derive_macros,
                trait_implementations,
                error_types,
                macro_calls,
                function_calls,
            }),
            file_doc,
        })
    }
}

/// Extract file-level `//!` inner doc comments from the top of a Rust file.
///
/// Collects consecutive `line_comment` nodes at the root level whose text
/// starts with `//!`, joining them into a single string.
fn extract_rust_file_doc(root: &Node, source: &[u8]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for i in 0..(root.child_count()) {
        let Some(child) = root.child(i as u32) else {
            break;
        };
        match child.kind() {
            "line_comment" => {
                let text = node_text(&child, source);
                if let Some(rest) = text.strip_prefix("//!") {
                    lines.push(rest.trim().to_owned());
                } else {
                    // Non-doc comment — stop collecting
                    break;
                }
            }
            "inner_attribute_item" => {
                // Skip #![...] attributes (e.g. #![allow(...)]) but continue
                // looking for //! comments after them.
            }
            _ => break,
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Check if a node has a `visibility_modifier` child (i.e., `pub`).
fn has_visibility_modifier(node: &Node) -> bool {
    for i in 0..(node.child_count()) {
        if let Some(c) = node.child(i as u32) {
            if c.kind() == "visibility_modifier" {
                return true;
            }
        }
    }
    false
}

/// Extract a `use_declaration` into one or more [`Import`]s.
///
/// Returns a `Vec<Import>` because brace-grouped forms with nested
/// `scoped_use_list` children produce one import per leaf group:
/// - `use std::io;`                 → [("std::io", ["io"])]
/// - `use std::io::Read;`           → [("std::io", ["Read"])]
/// - `use std::io::{Read, Write};`  → [("std::io", ["Read", "Write"])]
/// - `use std::io::*;`              → [("std::io", ["*"])]
/// - `use crate::{A, B};`           → [("crate", ["A", "B"])]
/// - `use crate::{foo::A, bar::B};` → [("crate::foo", ["A"]), ("crate::bar", ["B"])]
/// - `use std::{io::{self, Read}, fmt};`
///   → [("std::io", ["self", "Read"]), ("std", ["fmt"])]
fn extract_use_declaration(node: &Node, source: &[u8]) -> Vec<Import> {
    let line = node.start_position().row + 1;

    let Some(arg) = find_use_argument(node) else {
        return Vec::new();
    };

    parse_use_path(&arg, source, "")
        .into_iter()
        .map(|(module, names)| Import {
            module,
            names,
            is_type_only: false, // Rust doesn't have type-only imports
            line,
        })
        .collect()
}

/// Find the main argument node inside a `use_declaration`.
fn find_use_argument<'a>(node: &'a Node<'a>) -> Option<Node<'a>> {
    for i in 0..(node.child_count()) {
        let child = node.child(i as u32)?;
        match child.kind() {
            "scoped_identifier" | "scoped_use_list" | "use_wildcard" | "identifier"
            | "use_as_clause" => return Some(child),
            _ => {}
        }
    }
    None
}

/// Parse a use-path subtree into one or more `(module, names)` pairs.
///
/// `prefix` carries the accumulated path from any enclosing `scoped_use_list`
/// so nested groups (`use a::{b::{c, d}, e}`) resolve to fully qualified
/// modules. When non-empty it is joined to the current node's path segment
/// via `::`.
fn parse_use_path(node: &Node, source: &[u8], prefix: &str) -> Vec<(String, Vec<String>)> {
    match node.kind() {
        "scoped_identifier" => {
            // e.g., std::io::Read -> module: "std::io", name: "Read"
            let full = join_prefix(prefix, node_text(node, source));
            if let Some(pos) = full.rfind("::") {
                let module = full[..pos].to_string();
                let name = full[pos + 2..].to_string();
                vec![(module, vec![name])]
            } else {
                vec![(full.clone(), vec![full])]
            }
        }
        "scoped_use_list" => {
            // The path child can be `scoped_identifier`, `identifier`, OR a
            // bare `crate`/`self`/`super` keyword node — tree-sitter-rust
            // emits the keyword variants as anonymous nodes with those names,
            // so we cannot rely on `scoped_identifier`/`identifier` alone.
            let mut local_path = String::new();
            let mut use_list_node = None;
            for i in 0..(node.child_count()) {
                if let Some(child) = node.child(i as u32) {
                    match child.kind() {
                        "scoped_identifier" | "identifier" | "crate" | "self" | "super"
                            if use_list_node.is_none() && local_path.is_empty() =>
                        {
                            local_path = node_text(&child, source).to_string();
                        }
                        "use_list" => {
                            use_list_node = Some(child);
                        }
                        _ => {}
                    }
                }
            }

            let module_prefix = join_prefix(prefix, &local_path);
            let Some(use_list) = use_list_node else {
                // Defensive: malformed scoped_use_list with no brace body.
                return vec![(module_prefix.clone(), vec![module_prefix])];
            };

            expand_use_list(&use_list, source, &module_prefix)
        }
        "use_wildcard" => {
            // e.g., std::io::*
            let full = join_prefix(prefix, node_text(node, source));
            if let Some(pos) = full.rfind("::") {
                vec![(full[..pos].to_string(), vec!["*".to_string()])]
            } else {
                vec![(full, vec!["*".to_string()])]
            }
        }
        "identifier" => {
            let raw = node_text(node, source).to_string();
            let full = join_prefix(prefix, &raw);
            vec![(full, vec![raw])]
        }
        "use_as_clause" => {
            // e.g., `use foo::Bar as Baz;` or `use foo as bar;`.
            //
            // The symbol-index stores the defining (rightmost) name, not the
            // local alias — so strip the trailing ` as <alias>` from whatever
            // the path resolves to.
            let full = node_text(node, source).to_string();
            let defining = strip_as_alias(&full).to_owned();
            let qualified = join_prefix(prefix, &defining);
            if let Some(pos) = qualified.rfind("::") {
                let module = qualified[..pos].to_string();
                let rest = qualified[pos + 2..].to_string();
                vec![(module, vec![rest])]
            } else {
                vec![(qualified.clone(), vec![qualified])]
            }
        }
        _ => {
            let text = node_text(node, source).to_string();
            let full = join_prefix(prefix, &text);
            vec![(full.clone(), vec![text])]
        }
    }
}

/// Join an accumulated path `prefix` with a new segment `s` using `::`.
///
/// Returns `s` when `prefix` is empty so leading separators are never
/// produced. Both inputs are already normalised — they never start or
/// end with `::`.
fn join_prefix(prefix: &str, s: &str) -> String {
    if prefix.is_empty() {
        s.to_owned()
    } else {
        format!("{prefix}::{s}")
    }
}

/// Expand a `use_list` brace body into `(module, names)` pairs under `prefix`.
///
/// Each child of the brace list is treated as a leaf entry under `prefix`:
/// - bare `identifier`/`self` collapse into a single `(prefix, [names…])` pair
///   so flat lists like `use crate::{A, B};` stay a single import;
/// - `scoped_identifier` / `scoped_use_list` / `use_as_clause` / `use_wildcard`
///   recurse via [`parse_use_path`] with `prefix` carried in, so nested
///   groups (`use std::{io::{self, Read}, fmt};`) flatten to one import per
///   leaf group with fully-qualified module paths.
fn expand_use_list(node: &Node, source: &[u8], prefix: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut flat_names: Vec<String> = Vec::new();

    for i in 0..(node.child_count()) {
        let Some(child) = node.child(i as u32) else {
            continue;
        };
        match child.kind() {
            "identifier" => {
                flat_names.push(node_text(&child, source).to_string());
            }
            "self" => {
                flat_names.push("self".to_string());
            }
            "use_as_clause" => {
                // Record the defining (left-hand) name; alias is discarded.
                if let Some(first) = child.child(0) {
                    flat_names.push(node_text(&first, source).to_string());
                }
            }
            "scoped_identifier" | "scoped_use_list" | "use_wildcard" => {
                out.extend(parse_use_path(&child, source, prefix));
            }
            _ => {}
        }
    }

    if !flat_names.is_empty() {
        out.insert(0, (prefix.to_string(), flat_names));
    }
    out
}

/// Strip a trailing ` as <alias>` suffix from a Rust use-path string.
///
/// `use_as_clause` nodes serialise as e.g. `foo::Bar as Baz` via `node_text`;
/// the symbol-index stores the defining (rightmost) name, never the alias.
/// Scans from the right because the `as` clause is always the suffix —
/// defensive against any future case where `node_text` returns more than the
/// bare use-path.
fn strip_as_alias(s: &str) -> &str {
    if let Some(idx) = s.rfind(" as ") {
        s[..idx].trim_end()
    } else {
        s
    }
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

    let parameters = extract_rust_parameters(node, source);

    Function {
        name,
        is_public: is_pub,
        is_async,
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        parameters,
        // doc_comment is set by the caller via collect_rust_doc_comment.
        doc_comment: None,
    }
}

/// Extract parameter names from a Rust `function_item` or method node.
///
/// Walks the `parameters` child and extracts identifier names from each
/// `parameter` node. Skips `self_parameter` (self/&self/&mut self).
fn extract_rust_parameters(func_node: &Node, source: &[u8]) -> Vec<String> {
    let Some(params) = find_child_node(func_node, "parameters") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for i in 0..(params.child_count()) {
        let Some(child) = params.child(i as u32) else {
            continue;
        };
        if child.kind() == "parameter" {
            // A parameter has a pattern (typically identifier) and a type.
            // Try field name "pattern" first, then look for an identifier child.
            if let Some(pat) = child.child_by_field_name("pattern".as_bytes()) {
                // Pattern can be an identifier, a `_` wildcard, or a destructuring.
                // We only extract simple identifiers.
                if pat.kind() == "identifier" {
                    let name = node_text(&pat, source).to_string();
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            } else if let Some(name) = find_child_text(&child, "identifier", source) {
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
        // Skip self_parameter, commas, etc.
    }
    names
}

/// Extract a type definition (struct, enum, trait, type alias).
fn extract_type_def(node: &Node, source: &[u8], kind: TypeDefKind, is_pub: bool) -> TypeDef {
    let name = find_child_text(node, "type_identifier", source).unwrap_or_default();

    TypeDef {
        name,
        kind,
        is_public: is_pub,
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        // doc_comment is set by the caller via collect_rust_doc_comment.
        doc_comment: None,
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

    for i in 0..(node.child_count()) {
        if let Some(child) = node.child(i as u32) {
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
    for i in 0..(node.child_count()) {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "declaration_list" {
                for j in 0..(child.child_count()) {
                    if let Some(item) = child.child(j as u32) {
                        if item.kind() == "function_item" {
                            let is_pub = has_visibility_modifier(&item);
                            let func = extract_function(&item, source, is_pub);
                            if is_pub {
                                exports.push(Export {
                                    name: func.name.clone(),
                                    is_default: false,
                                    is_type_only: false,
                                    line: func.line,
                                    end_line: func.end_line,
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

/// Extract a `mod` declaration, capturing its name and 1-indexed source line.
fn extract_mod_declaration(node: &Node, source: &[u8]) -> Option<ModDeclaration> {
    let name = find_child_text(node, "identifier", source)?;
    Some(ModDeclaration {
        name,
        line: node.start_position().row + 1,
    })
}

/// Extract a macro call site, capturing the full macro path and line.
///
/// Handles both simple names (`vec`, `println`) and path-qualified names
/// (`tracing::info`, `std::mem::drop`).
fn extract_macro_call(node: &Node, source: &[u8]) -> Option<MacroCall> {
    // macro_invocation grammar: <macro> <token_tree>
    // <macro> can be an identifier or a scoped_identifier (path::name)
    let name = {
        // Try scoped_identifier first (e.g. `tracing::info`)
        if let Some(scoped) = find_child_node(node, "scoped_identifier") {
            node_text(&scoped, source).to_string()
        } else if let Some(ident) = find_child_node(node, "identifier") {
            node_text(&ident, source).to_string()
        } else {
            return None;
        }
    };
    if name.is_empty() {
        return None;
    }
    Some(MacroCall {
        name,
        line: node.start_position().row + 1,
    })
}

/// Walk the entire syntax tree and collect all `macro_invocation` nodes.
///
/// Uses a FIFO queue (BFS order) so macro calls are yielded in source order
/// (top-to-bottom, left-to-right).  A depth limit prevents runaway traversal
/// on pathologically large generated files.
///
/// `token_tree` nodes (macro argument bodies) are skipped entirely — they
/// contain raw token sequences, not structured Rust AST.  Nested macro calls
/// that appear as arguments to other macros are therefore not collected; only
/// the outermost invocation at each call site is recorded.
fn collect_macro_calls_recursive(root: &Node, source: &[u8], out: &mut Vec<MacroCall>) {
    // Queue entries: (node, depth).  BFS order preserves source ordering.
    let mut queue: VecDeque<(tree_sitter::Node, usize)> = VecDeque::new();
    for i in 0..root.child_count() {
        if let Some(child) = root.child(i as u32) {
            queue.push_back((child, 0));
        }
    }

    const MAX_DEPTH: usize = 60;

    while let Some((node, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            continue;
        }
        if node.kind() == "macro_invocation" {
            if let Some(call) = extract_macro_call(&node, source) {
                out.push(call);
            }
            // Don't recurse into macro token_tree bodies — they are not Rust AST.
            continue;
        }
        // Skip token_tree nodes — they contain macro argument tokens, not Rust AST.
        if node.kind() == "token_tree" {
            continue;
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                queue.push_back((child, depth + 1));
            }
        }
    }
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

// ── Function call-site collection ────────────────────────────────────────────

/// Extract a [`FunctionCall`] from a `call_expression` node.
///
/// The callee is resolved from the `function` child of the node:
/// - `scoped_identifier` → `"Arc::new"`, `"HashMap::with_capacity"`, …
/// - `field_expression`  → `"db.execute"`, `"self.run"`, …
/// - `identifier`        → `"scan_project"`, `"unwrap"`, …
///
/// Returns `None` when the callee cannot be determined (anonymous closures,
/// complex expressions) or when the name is empty.
fn extract_function_call(node: &Node, source: &str, source_lines: &[&str]) -> Option<FunctionCall> {
    let source_bytes = source.as_bytes();

    // tree-sitter Rust grammar:  call_expression { function: …, arguments: … }
    let function_child = node.child_by_field_name("function")?;

    let callee = match function_child.kind() {
        "scoped_identifier" | "identifier" => node_text(&function_child, source_bytes).to_owned(),
        "field_expression" => {
            // `receiver.method` — get the field (method name) child
            // tree-sitter field_expression: { value: …, field: identifier }
            if let Some(field) = function_child.child_by_field_name("field") {
                let value_text = if let Some(val) = function_child.child_by_field_name("value") {
                    node_text(&val, source_bytes).to_owned()
                } else {
                    String::new()
                };
                let field_text = node_text(&field, source_bytes);
                if value_text.is_empty() {
                    field_text.to_owned()
                } else {
                    format!("{value_text}.{field_text}")
                }
            } else {
                node_text(&function_child, source_bytes).to_owned()
            }
        }
        // Generic calls: `foo::<T>(...)` — scoped with type args
        "generic_function" => {
            if let Some(inner) = function_child.child_by_field_name("function") {
                node_text(&inner, source_bytes).to_owned()
            } else {
                node_text(&function_child, source_bytes).to_owned()
            }
        }
        _ => return None,
    };

    if callee.is_empty() {
        return None;
    }

    let line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;
    let snippet = super::build_call_snippet_from_lines(source_lines, line, end_line);

    Some(FunctionCall {
        callee,
        line,
        end_line,
        snippet,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{CALL_SNIPPET_MAX_LINES, MAX_FUNCTION_CALLS_PER_FILE, build_call_snippet};
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
        assert_eq!(ir.mod_declarations.len(), 1);
        assert_eq!(ir.mod_declarations[0].name, "utils");
        assert_eq!(
            ir.mod_declarations[0].line, 1,
            "mod decl must record line number"
        );
    }

    #[test]
    fn extracts_mod_declaration_with_correct_line() {
        // Line numbers are 1-indexed; "mod config;" is on line 3 here.
        let source = "use std::io;\n\nmod config;\n";
        let pf = parse_rust(source);
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert_eq!(ir.mod_declarations.len(), 1);
        assert_eq!(ir.mod_declarations[0].name, "config");
        assert_eq!(
            ir.mod_declarations[0].line, 3,
            "mod decl line must be 1-indexed"
        );
    }

    #[test]
    fn extracts_macro_calls() {
        let source = "fn main() {\n    tracing::info!(\"hello {}\", name);\n    vec![1, 2, 3];\n}";
        let pf = parse_rust(source);
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        // tracing::info! should be captured as a macro call
        let tracing_calls: Vec<_> = ir
            .macro_calls
            .iter()
            .filter(|m| m.name == "tracing::info")
            .collect();
        assert_eq!(
            tracing_calls.len(),
            1,
            "tracing::info! must be captured exactly once (not duplicated), got: {:?}",
            ir.macro_calls
        );
        assert_eq!(
            tracing_calls[0].line, 2,
            "macro call line must be 1-indexed"
        );
    }

    #[test]
    fn module_scope_macro_not_duplicated() {
        // Regression test for P-1: a macro call at module scope (outside any
        // function) must appear exactly once in ir.macro_calls, not twice.
        let source = "tracing::info!(\"startup\");\n\nfn foo() {}\n";
        let pf = parse_rust(source);
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        let count = ir
            .macro_calls
            .iter()
            .filter(|m| m.name == "tracing::info")
            .count();
        assert_eq!(
            count, 1,
            "module-scope macro_invocation must not be double-collected, got count={count}, calls={:?}",
            ir.macro_calls
        );
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
        assert!(
            ir.mod_declarations.iter().any(|m| m.name == "tests"),
            "mod tests must be in mod_declarations"
        );
    }

    // -----------------------------------------------------------------------
    // Parameter extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn extracts_function_parameters() {
        let pf = parse_rust("fn process(input: &str, count: usize) -> bool { true }");
        assert_eq!(pf.functions.len(), 1);
        assert_eq!(pf.functions[0].name, "process");
        assert_eq!(
            pf.functions[0].parameters,
            vec!["input".to_string(), "count".to_string()]
        );
    }

    #[test]
    fn extracts_no_parameters_for_unit_function() {
        let pf = parse_rust("fn main() {}");
        assert_eq!(pf.functions.len(), 1);
        assert!(pf.functions[0].parameters.is_empty());
    }

    #[test]
    fn skips_self_parameter_in_method() {
        let source = r#"
struct Foo;
impl Foo {
    pub fn bar(&self, x: i32) -> i32 { x }
}
"#;
        let pf = parse_rust(source);
        let method = pf.functions.iter().find(|f| f.name == "bar").unwrap();
        // Only "x", not "&self"
        assert_eq!(method.parameters, vec!["x".to_string()]);
    }

    #[test]
    fn extracts_multiple_typed_parameters() {
        let source = r#"
pub fn create(name: String, age: u32, active: bool) -> User {
    todo!()
}
"#;
        let pf = parse_rust(source);
        assert_eq!(pf.functions[0].parameters.len(), 3);
        assert_eq!(
            pf.functions[0].parameters,
            vec!["name".to_string(), "age".to_string(), "active".to_string()]
        );
    }

    #[test]
    fn extracts_async_function_parameters() {
        let pf = parse_rust("async fn fetch(url: &str, timeout: u64) {}");
        assert_eq!(pf.functions[0].name, "fetch");
        assert!(pf.functions[0].is_async);
        assert_eq!(
            pf.functions[0].parameters,
            vec!["url".to_string(), "timeout".to_string()]
        );
    }

    #[test]
    fn extracts_function_doc_comment() {
        let source = r#"
/// Handles an incoming HTTP request.
/// Returns a response.
pub fn handle(req: &str) -> String {
    req.to_owned()
}
"#;
        let pf = parse_rust(source);
        assert_eq!(pf.functions.len(), 1);
        let doc = pf.functions[0].doc_comment.as_deref().unwrap_or("");
        assert!(
            doc.contains("Handles an incoming HTTP request."),
            "got: {doc}"
        );
        assert!(doc.contains("Returns a response."), "got: {doc}");
    }

    #[test]
    fn function_without_doc_comment_is_none() {
        let pf = parse_rust("pub fn no_doc() {}");
        assert!(pf.functions[0].doc_comment.is_none());
    }

    #[test]
    fn extracts_struct_doc_comment() {
        let source = r#"
/// A user account.
pub struct User {
    pub name: String,
}
"#;
        let pf = parse_rust(source);
        assert_eq!(pf.types.len(), 1);
        let doc = pf.types[0].doc_comment.as_deref().unwrap_or("");
        assert!(doc.contains("A user account."), "got: {doc}");
    }

    #[test]
    fn extracts_file_doc_from_inner_comments() {
        let source = r#"//! This module handles authentication.
//! It provides JWT-based login.

pub fn login() {}
"#;
        let pf = parse_rust(source);
        let file_doc = pf.file_doc.as_deref().unwrap_or("");
        assert!(
            file_doc.contains("This module handles authentication."),
            "got: {file_doc}"
        );
        assert!(
            file_doc.contains("It provides JWT-based login."),
            "got: {file_doc}"
        );
    }

    #[test]
    fn file_without_inner_doc_has_no_file_doc() {
        let pf = parse_rust("pub fn foo() {}");
        assert!(pf.file_doc.is_none());
    }

    #[test]
    fn extracts_external_dependencies() {
        let source = r#"
use std::io::Read;
use serde::Serialize;
use reqwest::Client;
use crate::utils::foo;
use super::bar;
"#;
        let pf = parse_rust(source);
        let packages: Vec<&str> = pf
            .dependencies_used
            .iter()
            .map(|d| d.package.as_str())
            .collect();
        // External crates should be detected.
        assert!(packages.contains(&"serde"), "serde missing: {packages:?}");
        assert!(
            packages.contains(&"reqwest"),
            "reqwest missing: {packages:?}"
        );
        // stdlib and crate-internal must be excluded.
        assert!(
            !packages.contains(&"std"),
            "std must be excluded: {packages:?}"
        );
        assert!(
            !packages.contains(&"crate"),
            "crate must be excluded: {packages:?}"
        );
        assert!(
            !packages.contains(&"super"),
            "super must be excluded: {packages:?}"
        );
    }

    #[test]
    fn stdlib_only_file_has_no_dependencies() {
        let source = "use std::collections::HashMap;\nuse std::io::Read;";
        let pf = parse_rust(source);
        assert!(
            pf.dependencies_used.is_empty(),
            "stdlib-only file must have no external deps: {:?}",
            pf.dependencies_used
        );
    }

    // -----------------------------------------------------------------------
    // FunctionCall / call-site extraction tests
    // -----------------------------------------------------------------------

    fn rust_ir(pf: &ProjectFile) -> &RustIR {
        match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        }
    }

    #[test]
    fn extracts_simple_function_call() {
        let source = "fn main() { scan_project(root, config); }";
        let pf = parse_rust(source);
        let ir = rust_ir(&pf);
        let call = ir
            .function_calls
            .iter()
            .find(|c| c.callee == "scan_project");
        assert!(
            call.is_some(),
            "scan_project call must be captured; calls={:?}",
            ir.function_calls
        );
        assert_eq!(call.unwrap().line, 1);
    }

    #[test]
    fn extracts_scoped_call() {
        // Arc::new is a scoped_identifier call
        let source = "fn main() { let x = Arc::new(42); }";
        let pf = parse_rust(source);
        let ir = rust_ir(&pf);
        let call = ir.function_calls.iter().find(|c| c.callee == "Arc::new");
        assert!(
            call.is_some(),
            "Arc::new must be captured; calls={:?}",
            ir.function_calls
        );
    }

    #[test]
    fn deduplicates_same_callee() {
        // Calling scan_project five times — only ONE entry should be stored.
        let source = r#"
fn main() {
    scan_project(a, b);
    scan_project(c, d);
    scan_project(e, f);
    scan_project(g, h);
    scan_project(i, j);
}
"#;
        let pf = parse_rust(source);
        let ir = rust_ir(&pf);
        let count = ir
            .function_calls
            .iter()
            .filter(|c| c.callee == "scan_project")
            .count();
        assert_eq!(
            count, 1,
            "deduplicated: scan_project must appear exactly once; calls={:?}",
            ir.function_calls
        );
    }

    #[test]
    fn respects_500_limit() {
        // Generate a file with 600 unique function calls.
        let calls: String = (0..600).map(|i| format!("    f{i}();\n")).collect();
        let source = format!("fn main() {{\n{calls}}}");
        let pf = parse_rust(&source);
        let ir = rust_ir(&pf);
        assert!(
            ir.function_calls.len() <= MAX_FUNCTION_CALLS_PER_FILE,
            "must not exceed 500 unique callees; got {}",
            ir.function_calls.len()
        );
    }

    #[test]
    fn multiline_call_captured_fully() {
        // Five-argument call spanning 7 lines — snippet must include all of them.
        let source = r#"fn main() {
    let r = scan_project(
        root,
        config,
        db,
        opts,
        extra,
    );
    do_something(r);
}"#;
        let pf = parse_rust(source);
        let ir = rust_ir(&pf);
        let call = ir
            .function_calls
            .iter()
            .find(|c| c.callee == "scan_project")
            .expect("scan_project must be captured");
        // end_line must be beyond line (closing paren is on line 8).
        assert!(
            call.end_line > call.line,
            "multiline call: end_line ({}) must be > line ({})",
            call.end_line,
            call.line
        );
        // snippet must contain all argument names.
        assert!(call.snippet.contains("root"), "snippet must contain 'root'");
        assert!(
            call.snippet.contains("extra"),
            "snippet must contain 'extra'"
        );
        // snippet must also contain post-call context.
        assert!(
            call.snippet.contains("do_something"),
            "snippet must include post-call context"
        );
    }

    #[test]
    fn snippet_bof_guard() {
        // Call on the very first line — no context before it.
        let source = "scan_project(root);\nfn foo() {}\n";
        let pf = parse_rust(source);
        let ir = rust_ir(&pf);
        // Should not panic; snippet must be non-empty.
        let call = ir
            .function_calls
            .iter()
            .find(|c| c.callee == "scan_project");
        if let Some(c) = call {
            assert!(!c.snippet.is_empty(), "BOF call must still have a snippet");
        }
    }

    #[test]
    fn snippet_eof_guard() {
        // Call on the very last line — no context after it.
        let source = "fn main() {\n    scan_project(root);\n}";
        let pf = parse_rust(source);
        let ir = rust_ir(&pf);
        let call = ir
            .function_calls
            .iter()
            .find(|c| c.callee == "scan_project");
        if let Some(c) = call {
            assert!(!c.snippet.is_empty(), "EOF call must still have a snippet");
        }
    }

    #[test]
    fn snippet_capped_at_max_lines() {
        // Generate a call with 25 arguments (each on its own line) — snippet must be <= 30 lines.
        let args: String = (0..25).map(|i| format!("    arg{i},\n")).collect();
        let source = format!("fn main() {{\n    huge_call(\n{args}    );\n    after();\n}}");
        let pf = parse_rust(&source);
        let ir = rust_ir(&pf);
        let call = ir.function_calls.iter().find(|c| c.callee == "huge_call");
        if let Some(c) = call {
            let line_count = c.snippet.lines().count();
            assert!(
                line_count <= CALL_SNIPPET_MAX_LINES,
                "snippet must be capped at {CALL_SNIPPET_MAX_LINES} lines; got {line_count}"
            );
        }
    }

    #[test]
    fn build_call_snippet_basic() {
        // Unit-test the snippet builder directly.
        let source = "line1\nline2\nFN_CALL\nline4\nline5\nline6\nline7\nline8\n";
        // call at line 3, single line
        let snippet = build_call_snippet(source, 3, 3);
        assert!(snippet.contains("line1"), "2 lines before: {snippet}");
        assert!(snippet.contains("FN_CALL"), "call line itself: {snippet}");
        assert!(snippet.contains("line4"), "1 line after: {snippet}");
    }

    // -----------------------------------------------------------------------
    // Grouped `use` and local-mod regression tests
    // -----------------------------------------------------------------------
    //
    // Bug A (pre-fix): `pub use child_mod::Foo;` siblings of `pub mod child_mod;`
    // in the same file leaked into `dependencies_used` because the parser did
    // not know `child_mod` was a same-file submodule, so `rust_dep_from_import`
    // classified it as an external crate.
    //
    // Bug B (pre-fix): `use crate::{A, B};` produced an Import with empty
    // module — `parse_use_path` only accepted `scoped_identifier`/`identifier`
    // as the path child of a `scoped_use_list`, but tree-sitter-rust emits
    // bare `crate` / `self` / `super` as anonymous keyword nodes. The empty
    // module then leaked into `dependencies_used` as a `("", "", line)` ghost.

    #[test]
    fn use_crate_grouped_preserves_module() {
        // Bug B core fixture: brace-grouped `use crate::{A, B};` must keep
        // `module = "crate"` and emit both names.
        let pf = parse_rust("use crate::{SQL_NOT_REMOVED, query_convention};");
        assert_eq!(pf.imports.len(), 1);
        let imp = &pf.imports[0];
        assert_eq!(imp.module, "crate", "got: {imp:?}");
        assert_eq!(imp.names.len(), 2);
        assert!(imp.names.contains(&"SQL_NOT_REMOVED".to_string()));
        assert!(imp.names.contains(&"query_convention".to_string()));
        // dependencies_used must NOT carry an empty-string ghost entry.
        assert!(
            pf.dependencies_used.is_empty(),
            "crate-internal grouped use must yield no external deps; got {:?}",
            pf.dependencies_used
        );
    }

    #[test]
    fn use_self_grouped_preserves_module() {
        let pf = parse_rust("use self::{X, Y};");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "self");
        assert!(pf.imports[0].names.contains(&"X".to_string()));
        assert!(pf.imports[0].names.contains(&"Y".to_string()));
        assert!(pf.dependencies_used.is_empty());
    }

    #[test]
    fn use_super_grouped_preserves_module() {
        let pf = parse_rust("use super::{P, Q};");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "super");
        assert!(pf.imports[0].names.contains(&"P".to_string()));
        assert!(pf.imports[0].names.contains(&"Q".to_string()));
        assert!(pf.dependencies_used.is_empty());
    }

    #[test]
    fn use_crate_grouped_with_submodule_path() {
        let pf = parse_rust("use crate::foo::{A, B};");
        assert_eq!(pf.imports.len(), 1);
        assert_eq!(pf.imports[0].module, "crate::foo");
        assert!(pf.imports[0].names.contains(&"A".to_string()));
        assert!(pf.imports[0].names.contains(&"B".to_string()));
    }

    #[test]
    fn use_crate_grouped_with_nested_scoped_entries() {
        // `use crate::{foo::A, bar::B};` — each leaf has its own path,
        // so the parser emits two imports, one per leaf group.
        let pf = parse_rust("use crate::{foo::A, bar::B};");
        assert_eq!(pf.imports.len(), 2, "got: {:?}", pf.imports);
        let modules: std::collections::HashSet<&str> =
            pf.imports.iter().map(|i| i.module.as_str()).collect();
        assert!(modules.contains("crate::foo"), "modules: {modules:?}");
        assert!(modules.contains("crate::bar"), "modules: {modules:?}");
        let foo = pf
            .imports
            .iter()
            .find(|i| i.module == "crate::foo")
            .unwrap();
        assert_eq!(foo.names, vec!["A".to_string()]);
        let bar = pf
            .imports
            .iter()
            .find(|i| i.module == "crate::bar")
            .unwrap();
        assert_eq!(bar.names, vec!["B".to_string()]);
    }

    #[test]
    fn use_std_with_nested_brace_groups() {
        // `use std::{io::{self, Read}, fmt};` — bonus: nested
        // `scoped_use_list` inside a `use_list` must flatten cleanly.
        let pf = parse_rust("use std::{io::{self, Read}, fmt};");
        assert_eq!(pf.imports.len(), 2, "got: {:?}", pf.imports);

        // The flat name `fmt` collapses into a single ("std", ["fmt"]) entry.
        let std_fmt = pf
            .imports
            .iter()
            .find(|i| i.module == "std")
            .expect("std::fmt entry missing");
        assert_eq!(std_fmt.names, vec!["fmt".to_string()]);

        // The nested group becomes ("std::io", ["self", "Read"]).
        let std_io = pf
            .imports
            .iter()
            .find(|i| i.module == "std::io")
            .expect("std::io entry missing");
        assert!(std_io.names.contains(&"self".to_string()));
        assert!(std_io.names.contains(&"Read".to_string()));
        // Pure stdlib — never external.
        assert!(pf.dependencies_used.is_empty());
    }

    #[test]
    fn pub_mod_not_in_imports() {
        // Bug A: `pub mod foo;` is a module declaration, not an import.
        // It must land in `language_ir.mod_declarations`, never in `imports`,
        // and never as an external dependency.
        let pf = parse_rust("pub mod my_child;");
        assert!(
            pf.imports.is_empty(),
            "pub mod must not appear in imports; got {:?}",
            pf.imports
        );
        assert!(
            pf.dependencies_used.is_empty(),
            "pub mod must not yield external deps; got {:?}",
            pf.dependencies_used
        );
        let ir = match &pf.language_ir {
            LanguageIR::Rust(ir) => ir,
            _ => panic!("expected RustIR"),
        };
        assert_eq!(ir.mod_declarations.len(), 1);
        assert_eq!(ir.mod_declarations[0].name, "my_child");
    }

    #[test]
    fn pub_use_of_sibling_local_mod_is_not_external() {
        // Bug A real-world fixture: `lib.rs`-style file with `pub mod foo;`
        // and `pub use foo::Bar;` — `foo` is a local submodule, NOT an
        // external crate. It used to leak into `external_dependencies`
        // because `rust_dep_from_import("foo", _)` classified anything
        // not in the Rust builtin set as external.
        let source = r#"
pub mod code_pattern;
pub mod conventions;

use rusqlite::Connection;

pub use code_pattern::{Foo, Bar};
pub use conventions::ConventionData;
"#;
        let pf = parse_rust(source);

        // `code_pattern` and `conventions` still show up in `imports` —
        // resolve_import uses them to map to sibling files at query time.
        // The contract we enforce here is purely about external classification.
        let dep_packages: std::collections::HashSet<&str> = pf
            .dependencies_used
            .iter()
            .map(|d| d.package.as_str())
            .collect();
        assert!(
            !dep_packages.contains("code_pattern"),
            "local mod 'code_pattern' must not be classified as external; got deps: {dep_packages:?}"
        );
        assert!(
            !dep_packages.contains("conventions"),
            "local mod 'conventions' must not be classified as external; got deps: {dep_packages:?}"
        );
        // True external still survives.
        assert!(
            dep_packages.contains("rusqlite"),
            "real external crate must still be classified as external; got deps: {dep_packages:?}"
        );
    }

    #[test]
    fn use_with_aliased_grouped_names() {
        // `use foo::{Bar as Baz, Qux};` — `Bar` (not the alias `Baz`) is
        // recorded, alongside `Qux`. Confirms `use_as_clause` handling
        // inside a brace list still works after the refactor.
        let pf = parse_rust("use foo::{Bar as Baz, Qux};");
        assert_eq!(pf.imports.len(), 1);
        let imp = &pf.imports[0];
        assert_eq!(imp.module, "foo");
        assert!(
            imp.names.contains(&"Bar".to_string()),
            "got: {:?}",
            imp.names
        );
        assert!(
            imp.names.contains(&"Qux".to_string()),
            "got: {:?}",
            imp.names
        );
        assert!(
            !imp.names.contains(&"Baz".to_string()),
            "alias must not be recorded; got: {:?}",
            imp.names
        );
    }
}
