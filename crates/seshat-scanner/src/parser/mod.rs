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

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use seshat_core::{FunctionCall, Language, ProjectFile};
use sha2::{Digest, Sha256};
use tree_sitter::Node;

use crate::ScanError;
use javascript_parser::JavaScriptParser;
use python_parser::PythonParser;
use rust_parser::RustParser;
use seshat_core::ir::DependencyUsage;
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
    (0..node.child_count())
        .filter_map(|i| node.child(i as u32))
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

// ---------------------------------------------------------------------------
// Call-site shared helpers (used by all language parsers)
// ---------------------------------------------------------------------------

/// Maximum number of function call entries to collect per file.
pub(crate) const MAX_FUNCTION_CALLS_PER_FILE: usize = 500;

/// Number of context lines to include **before** the opening line of the call.
pub(crate) const CALL_SNIPPET_LINES_BEFORE: usize = 2;

/// Number of context lines to include **after** the closing line of the call.
pub(crate) const CALL_SNIPPET_LINES_AFTER: usize = 4;

/// Maximum total lines in a call-site snippet.
pub(crate) const CALL_SNIPPET_MAX_LINES: usize = 30;

/// Build a context snippet around a call-site from a pre-split line slice.
///
/// Layout:
/// ```text
/// [CALL_SNIPPET_LINES_BEFORE lines before `line`]
/// [all lines of the call expression: `line` ..= `end_line`]
/// [CALL_SNIPPET_LINES_AFTER lines after `end_line`]
/// ```
///
/// The total is capped at [`CALL_SNIPPET_MAX_LINES`].
/// Lines are taken verbatim from `source_lines` (original indentation preserved).
pub fn build_call_snippet_from_lines(
    source_lines: &[&str],
    line: usize,
    end_line: usize,
) -> String {
    let total = source_lines.len();
    if total == 0 || line == 0 || end_line == 0 {
        return String::new();
    }

    // Convert to 0-indexed, clamp to file bounds.
    let call_start_0 = (line - 1).min(total - 1);
    let call_end_0 = (end_line - 1).min(total - 1);
    // Guard against inverted spans (tree-sitter error-recovery nodes).
    let call_end_0 = call_end_0.max(call_start_0);

    let snippet_start = call_start_0.saturating_sub(CALL_SNIPPET_LINES_BEFORE);
    let snippet_end_uncapped = (call_end_0 + CALL_SNIPPET_LINES_AFTER + 1).min(total);

    // Hard cap: never exceed CALL_SNIPPET_MAX_LINES total.
    let snippet_end = snippet_end_uncapped.min(snippet_start + CALL_SNIPPET_MAX_LINES);

    source_lines[snippet_start..snippet_end].join("\n")
}

/// Convenience wrapper: splits `source` into lines and delegates to
/// [`build_call_snippet_from_lines`].  Use the `_from_lines` variant directly
/// when building many snippets from the same file to avoid repeated allocation.
pub fn build_call_snippet(source: &str, line: usize, end_line: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    build_call_snippet_from_lines(&lines, line, end_line)
}

/// Walk the entire syntax tree (BFS) collecting function call nodes.
///
/// `call_kind`: tree-sitter node kind to match.
///   - `"call_expression"` for Rust, TypeScript, JavaScript
///   - `"call"` for Python
///
/// `skip_kinds`: node kinds to prune entirely (no descent into their children).
///   Pass `&["token_tree"]` for Rust; pass `&[]` for other languages.
///
/// `extract_fn`: language-specific closure that extracts a [`FunctionCall`] from a
/// matched node.  Receives `(node, source, source_lines)` — `source_lines` is the
/// pre-split line slice so snippet builders don't re-allocate it per call.
/// Returns `None` for nodes that should be skipped.
///
/// Deduplicates by callee name via a `HashSet` (first occurrence wins, O(1) lookup).
/// Stops enqueuing new children as soon as [`MAX_FUNCTION_CALLS_PER_FILE`] is reached.
pub fn collect_calls_bfs<F>(
    root: &tree_sitter::Node,
    source: &str,
    call_kind: &str,
    skip_kinds: &[&str],
    extract_fn: F,
    out: &mut Vec<FunctionCall>,
) where
    F: Fn(&tree_sitter::Node, &str, &[&str]) -> Option<FunctionCall>,
{
    // Split lines once for the entire file; passed to every extract_fn call.
    let source_lines: Vec<&str> = source.lines().collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(tree_sitter::Node, usize)> = VecDeque::new();
    for i in 0..root.child_count() {
        if let Some(child) = root.child(i as u32) {
            queue.push_back((child, 0));
        }
    }

    const MAX_DEPTH: usize = 60;

    while let Some((node, depth)) = queue.pop_front() {
        // Hard stop: don't enqueue more children once the cap is reached.
        if out.len() >= MAX_FUNCTION_CALLS_PER_FILE {
            break;
        }
        if depth > MAX_DEPTH {
            continue;
        }

        // Language-specific subtrees to skip entirely (no descent).
        if skip_kinds.contains(&node.kind()) {
            continue;
        }

        if node.kind() == call_kind {
            if let Some(call) = extract_fn(&node, source, &source_lines) {
                // O(1) dedup via HashSet.
                if seen.insert(call.callee.clone()) {
                    out.push(call);
                }
            }
            // Still recurse into call children (nested calls).
        }

        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                queue.push_back((child, depth + 1));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Doc-comment extraction helpers (shared across parsers)
// ---------------------------------------------------------------------------

/// Collect consecutive leading `///` doc-comment lines immediately preceding
/// a Rust AST node.
///
/// Walks **backwards** from the node's previous named sibling, collecting
/// adjacent `line_comment` nodes whose text starts with `///`. Returns them
/// joined as a single string (with `///` prefix stripped), or `None` if no
/// doc comments were found.
pub(super) fn collect_rust_doc_comment(node: &Node, source: &[u8]) -> Option<String> {
    let mut comments: Vec<String> = Vec::new();
    let mut current = node.prev_sibling();
    while let Some(prev) = current {
        match prev.kind() {
            "line_comment" => {
                let text = node_text(&prev, source);
                if let Some(doc) = text.strip_prefix("///") {
                    comments.push(doc.trim().to_owned());
                    current = prev.prev_sibling();
                    continue;
                }
                break;
            }
            // #[derive(...)], #[cfg(test)], etc. sit between `///` lines and the
            // item definition in Rust syntax — skip them and keep walking back.
            "attribute_item" => {
                current = prev.prev_sibling();
            }
            _ => break,
        }
    }
    if comments.is_empty() {
        return None;
    }
    comments.reverse();
    Some(comments.join("\n"))
}

/// Extract a file-level doc comment from the first node in a JS/TS parse tree.
///
/// Scans root children in order; returns the cleaned text of the first
/// `comment` node encountered, skipping only `hash_bang_line` nodes.
/// Stops immediately on any non-comment, non-shebang node.
pub(super) fn extract_js_ts_file_doc(root: &Node, source: &[u8]) -> Option<String> {
    for i in 0..(root.child_count()) {
        let Some(child) = root.child(i as u32) else {
            break;
        };
        if child.kind() == "comment" {
            let raw = node_text(&child, source);
            let cleaned = clean_js_comment(raw);
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

/// Collect a leading JSDoc or block comment immediately preceding a TS/JS node.
///
/// Uses `prev_named_sibling()` to find the nearest preceding `comment` node.
/// Returns the cleaned text (strips `/** */` and `//` markers) or `None`.
pub(super) fn collect_js_doc_comment(node: &Node, source: &[u8]) -> Option<String> {
    let prev = node.prev_named_sibling()?;
    if prev.kind() != "comment" {
        return None;
    }
    let raw = node_text(&prev, source);
    let cleaned = clean_js_comment(raw);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Strip JSDoc (`/** ... */`), block-comment (`/* ... */`), or line-comment
/// (`//`) markers from a raw comment string, returning trimmed human-readable
/// text.
///
/// Both `/** */` and `/* */` are handled with shared logic — only the prefix
/// length differs.
pub(super) fn clean_js_comment(raw: &str) -> String {
    let s = raw.trim();

    // Block comment: /** ... */ or /* ... */
    if s.starts_with("/*") && s.ends_with("*/") {
        // Skip either 3 bytes (/**) or 2 bytes (/*).
        let prefix_len = if s.starts_with("/**") { 3 } else { 2 };
        let inner = &s[prefix_len..s.len() - 2];
        return inner
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
    }

    // Line comment: // ...
    if let Some(rest) = s.strip_prefix("//") {
        return rest.trim().to_owned();
    }

    s.to_owned()
}

/// Extract a Python docstring from the first statement of a `block` node.
///
/// Returns the stripped content of the first triple-quoted or single-quoted
/// string literal in the block, or `None` if no docstring is present.
pub(super) fn extract_python_docstring(block: &Node, source: &[u8]) -> Option<String> {
    // The first named child of a `block` that is an `expression_statement`
    // containing a bare `string` literal is the docstring.
    let first = block.named_child(0)?;
    if first.kind() != "expression_statement" {
        return None;
    }
    // The expression_statement should have exactly one named child: a string.
    let expr = first.named_child(0)?;
    if expr.kind() == "string" {
        let raw = node_text(&expr, source);
        return Some(clean_python_docstring(raw));
    }
    None
}

/// Strip surrounding triple/single/double quotes from a Python string literal.
///
/// Uses byte-length arithmetic which is safe here because all quote delimiters
/// (`"""`, `'''`, `"`, `'`) are ASCII (1 byte each).
fn clean_python_docstring(raw: &str) -> String {
    let s = raw.trim();

    // Strip triple quotes first (""" or ''')
    for delim in &[r#"""""#, "'''"] {
        let dlen = delim.len(); // 3 bytes, always ASCII
        if s.starts_with(delim) && s.ends_with(delim) && s.len() >= dlen * 2 {
            let inner = &s[dlen..s.len() - dlen];
            return inner.trim().to_owned();
        }
    }

    // Strip single double/single quote (" or ')
    // Both are ASCII (1 byte), so byte-indexing is safe.
    for delim in &[r#"""#, "'"] {
        if s.starts_with(delim) && s.ends_with(delim) && s.len() >= 2 {
            let inner = &s[1..s.len() - 1];
            return inner.trim().to_owned();
        }
    }

    s.to_owned()
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

    for i in 0..(clause.child_count()) {
        let Some(child) = clause.child(i as u32) else {
            continue;
        };
        match child.kind() {
            "identifier" => {
                // Default import: `import Foo from ...`
                names.push(node_text(&child, source).to_string());
            }
            "named_imports" => {
                // Named imports: `import { Foo, Bar } from ...`
                for j in 0..(child.child_count()) {
                    if let Some(spec) = child.child(j as u32) {
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
///
/// `line` and `end_line` are the start/end of the surrounding `export_statement`
/// node (passed by the caller) so every emitted [`Export`] carries the full
/// source range of the declaration — which the hunk-intersection logic in
/// `map_diff_impact` uses to decide whether a changed hunk touches the symbol.
#[allow(clippy::too_many_arguments)]
pub(super) fn extract_exported_lexical(
    node: &Node,
    source: &[u8],
    exports: &mut Vec<seshat_core::Export>,
    functions: &mut Vec<seshat_core::Function>,
    is_default: bool,
    line: usize,
    end_line: usize,
) {
    for i in 0..(node.child_count()) {
        let Some(child) = node.child(i as u32) else {
            continue;
        };
        if child.kind() == "variable_declarator" {
            let name = find_child_text(&child, "identifier", source).unwrap_or_default();

            // Check if the value is an arrow function or function expression
            let func_node = find_arrow_or_function_expr(&child);
            let is_func = func_node.is_some();

            if is_func {
                let is_async = child_has_async_value(&child, source);
                let parameters = func_node
                    .map(|n| extract_js_ts_parameters(&n, source))
                    .unwrap_or_default();
                functions.push(seshat_core::Function {
                    name: name.clone(),
                    is_public: true,
                    is_async,
                    line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                    parameters,
                    // doc_comment for lexical arrow-functions is not yet extracted
                    // (no prev_named_sibling hook available here without refactoring).
                    doc_comment: None,
                });
            }

            if !name.is_empty() {
                exports.push(seshat_core::Export {
                    name,
                    is_default,
                    is_type_only: false,
                    line,
                    end_line,
                });
            }
        }
    }
}

/// Extract a `function_declaration` node into a [`seshat_core::Function`].
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn extract_function_declaration(node: &Node, source: &[u8]) -> seshat_core::Function {
    let name = find_child_text(node, "identifier", source).unwrap_or_default();
    let is_async = has_child_kind(node, "async");
    let parameters = extract_js_ts_parameters(node, source);

    seshat_core::Function {
        name,
        is_public: false, // will be set to true by export handling
        is_async,
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        parameters,
        // doc_comment is set by the caller (parser main loop) via collect_js_doc_comment.
        doc_comment: None,
    }
}

/// Check if a `variable_declarator` value child (arrow_function or
/// function_expression) is async.
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn child_has_async_value(declarator: &Node, source: &[u8]) -> bool {
    for i in 0..(declarator.child_count()) {
        if let Some(child) = declarator.child(i as u32) {
            if child.kind() == "arrow_function" || child.kind() == "function_expression" {
                return has_child_kind(&child, "async");
            }
        }
    }
    // Fallback: check the whole declarator text
    node_text(declarator, source).contains("async")
}

/// Find the first `arrow_function` or `function_expression` child of a
/// `variable_declarator` node.
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn find_arrow_or_function_expr<'a>(declarator: &'a Node) -> Option<Node<'a>> {
    for i in 0..(declarator.child_count()) {
        if let Some(child) = declarator.child(i as u32) {
            match child.kind() {
                "arrow_function" | "function_expression" => return Some(child),
                _ => {}
            }
        }
    }
    None
}

/// Extract parameter names from a JS/TS function node.
///
/// Works for `function_declaration`, `arrow_function`, `function_expression`,
/// and `method_definition` nodes. Looks for a `formal_parameters` child and
/// extracts identifier names from each parameter.
///
/// Shared between the TypeScript and JavaScript parsers.
pub(super) fn extract_js_ts_parameters(func_node: &Node, source: &[u8]) -> Vec<String> {
    let Some(params) = find_child_node(func_node, "formal_parameters") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for i in 0..(params.child_count()) {
        let Some(child) = params.child(i as u32) else {
            continue;
        };
        match child.kind() {
            // Simple identifier parameter: `function f(x) {}`
            "identifier" => {
                let name = node_text(&child, source).to_string();
                if !name.is_empty() {
                    names.push(name);
                }
            }
            // TS required parameter: `function f(x: number) {}`
            // TS optional parameter: `function f(x?: number) {}`
            "required_parameter" | "optional_parameter" => {
                // The first identifier child is the parameter name
                if let Some(name) = find_child_text(&child, "identifier", source) {
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
            // Default parameter: `function f(x = 5) {}`
            "assignment_pattern" => {
                // Left side of the assignment is the parameter name
                if let Some(first) = child.child(0) {
                    if first.kind() == "identifier" {
                        let name = node_text(&first, source).to_string();
                        if !name.is_empty() {
                            names.push(name);
                        }
                    }
                }
            }
            // Rest parameter: `function f(...args) {}`
            "rest_pattern" => {
                if let Some(name) = find_child_text(&child, "identifier", source) {
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// Compute the SHA-256 hex digest of the given source content.
pub fn content_hash(source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let hash = hasher.finalize();
    let mut hex = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Read `abs_path` from disk, [`parse_file`] it under `stored_path`, then
/// strip `local_packages` from `dependencies_used`. Returns the parsed
/// `ProjectFile` alongside the original source so callers can populate a
/// `source_map` for the detection pipeline.
///
/// `abs_path` is the on-disk path used for I/O; `stored_path` is what
/// the resulting `ProjectFile.path` carries (and ultimately becomes the
/// `files_ir.file_path` key on upsert). Splitting the two lets callers
/// store paths relative to the project root — so cross-worktree scans of
/// the same git tree share a single `(branch_id, file_path)` IR row
/// instead of one row per worktree-prefix variant (Bug #3).
///
/// Single source of truth for the read+parse+strip-local-packages pattern
/// shared by the full scan orchestrator, the hot-tier watcher, and the
/// incremental freshness sync. Keeping every path through one helper means
/// detector evidence (snippets) is built consistently regardless of which
/// trigger drove the IR upsert.
pub fn read_and_parse_file(
    abs_path: &Path,
    stored_path: &Path,
    language: Language,
    local_packages: &[String],
) -> std::io::Result<(ProjectFile, String)> {
    let source = std::fs::read_to_string(abs_path)?;
    let mut project_file = parse_file(stored_path, &source, language);
    if !local_packages.is_empty() {
        project_file
            .dependencies_used
            .retain(|dep| !local_packages.contains(&dep.package));
    }
    Ok((project_file, source))
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
        file_doc: None,
    }
}

// ---------------------------------------------------------------------------
// Dependency classification helpers (shared by all language parsers)
// ---------------------------------------------------------------------------

/// Returns `true` if a Rust `use` path refers to a built-in / first-party
/// module that should not be counted as an external dependency.
pub(super) fn is_rust_builtin(module: &str) -> bool {
    let first = module.split("::").next().unwrap_or(module);
    matches!(
        first,
        "std" | "core" | "alloc" | "proc_macro" | "test" | "self" | "super" | "crate"
    )
}

/// Returns `true` if a Python import path is part of the standard library
/// or is a relative import (`.foo`, `..bar`).
pub(super) fn is_python_stdlib_or_relative(module: &str) -> bool {
    if module.starts_with('.') {
        return true;
    }
    let root = module.split('.').next().unwrap_or(module);
    matches!(
        root,
        "os" | "sys"
            | "re"
            | "json"
            | "math"
            | "io"
            | "abc"
            | "ast"
            | "copy"
            | "datetime"
            | "enum"
            | "functools"
            | "itertools"
            | "logging"
            | "pathlib"
            | "typing"
            | "collections"
            | "dataclasses"
            | "contextlib"
            | "subprocess"
            | "threading"
            | "asyncio"
            | "time"
            | "hashlib"
            | "hmac"
            | "base64"
            | "urllib"
            | "http"
            | "email"
            | "csv"
            | "sqlite3"
            | "unittest"
            | "tempfile"
            | "shutil"
            | "glob"
            | "inspect"
            | "traceback"
            | "warnings"
            | "weakref"
            | "gc"
            | "struct"
            | "socket"
            | "ssl"
            | "uuid"
            | "string"
            | "textwrap"
            | "random"
            | "secrets"
            | "decimal"
            | "fractions"
            | "statistics"
            | "pprint"
            | "builtins"
            | "__future__"
            | "typing_extensions"
            | "types"
            | "operator"
            // Additional stdlib modules that were missing:
            | "argparse"
            | "configparser"
            | "xml"
            | "zipfile"
            | "tarfile"
            | "pickle"
            | "shelve"
            | "queue"
            | "shlex"
            | "platform"
            | "multiprocessing"
            | "concurrent"
            | "signal"
            | "fnmatch"
            | "difflib"
            | "dis"
            | "compileall"
            | "runpy"
            | "importlib"
            | "pkgutil"
            | "ctypes"
            | "array"
            | "bisect"
            | "heapq"
            | "pdb"
            | "profile"
            | "cProfile"
            | "timeit"
            | "doctest"
            | "getopt"
            | "getpass"
            | "curses"
            | "readline"
            | "rlcompleter"
            | "zipimport"
            | "zlib"
            | "gzip"
            | "bz2"
            | "lzma"
    )
}

/// Returns `true` if a TypeScript / JavaScript import path refers to a
/// local module (relative path, path alias) or Node built-in.
pub(super) fn is_ts_js_builtin(module: &str) -> bool {
    module.starts_with("./")
        || module.starts_with("../")
        || module.starts_with("@/")   // common path alias
        || module.starts_with("~/")   // common path alias
        || module.starts_with("node:") // explicit Node built-in protocol
        || module.starts_with('#') // Node subpath imports
}

/// Extract the NPM package name from a TypeScript / JavaScript import specifier.
///
/// For scoped packages (`@angular/core/testing`) the scope + first segment is
/// returned (`@angular/core`).  For unscoped packages (`react/hooks`) only the
/// top-level package name is returned (`react`).
pub(super) fn ts_package_name(module: &str) -> String {
    if let Some(rest) = module.strip_prefix('@') {
        // Scoped package: @scope/name[/deep]
        let segments: Vec<&str> = rest.splitn(3, '/').collect();
        if segments.len() >= 2 {
            return format!("@{}/{}", segments[0], segments[1]);
        }
        return format!("@{}", rest);
    }
    module.split('/').next().unwrap_or(module).to_owned()
}

/// Build a [`DependencyUsage`] from a Rust import path if it is an external
/// dependency (i.e. not a stdlib / crate-internal path).
pub(super) fn rust_dep_from_import(module: &str, line: usize) -> Option<DependencyUsage> {
    if is_rust_builtin(module) {
        return None;
    }
    let package = module.split("::").next().unwrap_or(module).to_owned();
    Some(DependencyUsage {
        package,
        import_path: module.to_owned(),
        line,
    })
}

/// Build a [`DependencyUsage`] from a Python import path if it is an external
/// dependency (i.e. not stdlib or relative).
pub(super) fn python_dep_from_import(module: &str, line: usize) -> Option<DependencyUsage> {
    if is_python_stdlib_or_relative(module) {
        return None;
    }
    let package = module.split('.').next().unwrap_or(module).to_owned();
    Some(DependencyUsage {
        package,
        import_path: module.to_owned(),
        line,
    })
}

/// Build a [`DependencyUsage`] from a TypeScript / JavaScript import specifier
/// if it is an external package.
pub(super) fn ts_dep_from_import(module: &str, line: usize) -> Option<DependencyUsage> {
    if is_ts_js_builtin(module) {
        return None;
    }
    let package = ts_package_name(module);
    Some(DependencyUsage {
        package,
        import_path: module.to_owned(),
        line,
    })
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

    // -----------------------------------------------------------------------
    // Dependency extraction helpers
    // -----------------------------------------------------------------------

    #[test]
    fn rust_builtin_filter() {
        assert!(is_rust_builtin("std"));
        assert!(is_rust_builtin("std::io"));
        assert!(is_rust_builtin("core::fmt"));
        assert!(is_rust_builtin("alloc::vec"));
        assert!(is_rust_builtin("crate::foo"));
        assert!(is_rust_builtin("super::bar"));
        assert!(is_rust_builtin("self::baz"));
        assert!(!is_rust_builtin("reqwest"));
        assert!(!is_rust_builtin("serde::Serialize"));
        assert!(!is_rust_builtin("tokio::runtime"));
    }

    #[test]
    fn python_builtin_filter() {
        assert!(is_python_stdlib_or_relative("os"));
        assert!(is_python_stdlib_or_relative("sys"));
        assert!(is_python_stdlib_or_relative("typing"));
        assert!(is_python_stdlib_or_relative(".relative"));
        assert!(is_python_stdlib_or_relative("..parent"));
        assert!(!is_python_stdlib_or_relative("requests"));
        assert!(!is_python_stdlib_or_relative("fastapi"));
        assert!(!is_python_stdlib_or_relative("pydantic"));
    }

    #[test]
    fn ts_package_name_extraction() {
        assert_eq!(ts_package_name("react"), "react");
        assert_eq!(ts_package_name("react/hooks"), "react");
        assert_eq!(ts_package_name("@angular/core"), "@angular/core");
        assert_eq!(ts_package_name("@angular/core/testing"), "@angular/core");
    }

    #[test]
    fn ts_builtin_filter() {
        assert!(is_ts_js_builtin("./local"));
        assert!(is_ts_js_builtin("../parent"));
        assert!(is_ts_js_builtin("@/alias"));
        assert!(is_ts_js_builtin("~/home"));
        assert!(is_ts_js_builtin("node:fs"));
        assert!(is_ts_js_builtin("#internal"));
        assert!(!is_ts_js_builtin("react"));
        assert!(!is_ts_js_builtin("@angular/core"));
        assert!(!is_ts_js_builtin("axios"));
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
