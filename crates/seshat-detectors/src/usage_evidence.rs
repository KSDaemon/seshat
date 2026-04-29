//! Cross-reference import symbols to function call sites for evidence snippets.
//!
//! Replaces import-line evidence with actual call-site evidence by matching
//! [`FunctionCall`] callees against [`Import`] names from the same top-level
//! package or module.

use std::collections::HashSet;
use std::path::Path;

use seshat_core::{CodeEvidence, FunctionCall, Import, LanguageIR, ProjectFile};

/// Maximum evidence entries returned by [`find_usage_evidence`].
const DEFAULT_MAX: usize = 5;

/// Match function call sites to their corresponding imports, returning
/// call-site evidence instead of import-line evidence.
///
/// For each [`FunctionCall`], the callee is matched against [`Import`].names
/// using these strategies:
///
/// 1. **Namespaced calls** (`tracing::info`): split on `::` — the left side
///    must match the import's top-level module, and the right side must be in
///    the import's names. Additionally, if the left side itself appears in any
///    import's names (e.g. `Client::new` where `Client` was imported), it
///    matches.
/// 2. **Method calls** (`logger.info`): split on `.` — the receiver must
///    appear in some import's names.
/// 3. **Standalone names** (`info`): must appear in some import's names.
///
/// Results are deduplicated by callee name (one [`CodeEvidence`] per unique
/// callee) and limited to `max` entries.
pub fn find_usage_evidence(
    imports: &[Import],
    function_calls: &[FunctionCall],
    file_path: &Path,
    max: usize,
) -> Vec<CodeEvidence> {
    let limit = if max == 0 { DEFAULT_MAX } else { max };

    if imports.is_empty() || function_calls.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::with_capacity(function_calls.len().min(limit));
    let mut seen_callees = HashSet::new();

    for call in function_calls {
        if result.len() >= limit {
            break;
        }

        if seen_callees.contains(&call.callee) {
            continue;
        }

        if matches_import(call, imports) {
            seen_callees.insert(call.callee.clone());
            result.push(CodeEvidence {
                file: file_path.to_path_buf(),
                line: call.line,
                end_line: call.end_line,
                snippet: call.snippet.clone(),
            });
        }
    }

    result
}

/// Check whether a function call's callee can be resolved to an import.
fn matches_import(call: &FunctionCall, imports: &[Import]) -> bool {
    // Case 1: Rust-style namespaced call (e.g. "tracing::info", "Client::new")
    if let Some((left, right)) = split_first(call.callee.as_str(), "::") {
        // Strategy A: left matches import's top-level module, right is in names
        for imp in imports {
            let imp_top = imp
                .module
                .chars()
                .position(|c| [' ', ':', '.'].contains(&c))
                .map(|p| &imp.module[..p])
                .unwrap_or(&imp.module);
            if imp_top == left && imp.names.iter().any(|n| *n == right) {
                return true;
            }
        }
        // Strategy B: left (the type name) is itself in an import's names
        for imp in imports {
            if imp.names.iter().any(|n| *n == left) {
                return true;
            }
        }
    }

    // Case 2: Method call (e.g. "logger.info", "db.execute")
    if let Some((receiver, _method)) = split_first(call.callee.as_str(), ".") {
        for imp in imports {
            if imp.names.iter().any(|n| *n == receiver) {
                return true;
            }
        }
    }

    // Case 3: Standalone name (e.g. "info", "scan_project")
    for imp in imports {
        if imp.names.contains(&call.callee) {
            return true;
        }
    }

    false
}

/// Split a string on the first occurrence of a separator, returning both parts.
fn split_first<'a>(s: &'a str, sep: &str) -> Option<(&'a str, &'a str)> {
    let pos = s.find(sep)?;
    Some((&s[..pos], &s[pos + sep.len()..]))
}

/// Language-agnostic wrapper: extract call-site evidence from a [`ProjectFile`].
///
/// Dispatches to the language-specific IR to extract both `function_calls` and
/// (for Rust) `macro_calls`. Rust macro calls are converted into synthetic
/// [`FunctionCall`] entries so they flow through the same matching logic.
///
/// Returns up to `max` evidence entries (defaults to [`DEFAULT_MAX`] if `max == 0`).
pub fn find_usage_evidence_for_file(file: &ProjectFile, max: usize) -> Vec<CodeEvidence> {
    let limit = if max == 0 { DEFAULT_MAX } else { max };

    let mut all_calls: Vec<FunctionCall> = match &file.language_ir {
        LanguageIR::Rust(ir) => {
            let mut calls = ir.function_calls.clone();
            for mc in &ir.macro_calls {
                calls.push(FunctionCall {
                    callee: mc.name.clone(),
                    line: mc.line,
                    end_line: mc.line,
                    snippet: String::new(),
                });
            }
            calls
        }
        LanguageIR::TypeScript(ir) => ir.function_calls.clone(),
        LanguageIR::JavaScript(ir) => ir.function_calls.clone(),
        LanguageIR::Python(ir) => ir.function_calls.clone(),
    };

    if all_calls.is_empty() || file.imports.is_empty() {
        return Vec::new();
    }

    // Sort by line so the first occurrences are kept during dedup.
    all_calls.sort_by_key(|c| c.line);
    find_usage_evidence(&file.imports, &all_calls, &file.path, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::ir::LanguageIR;
    use seshat_core::{JavaScriptIR, MacroCall, PythonIR, RustIR, TypeScriptIR};
    use std::path::PathBuf;

    fn file_path() -> PathBuf {
        PathBuf::from("src/lib.rs")
    }

    fn make_import(module: &str, names: &[&str]) -> Import {
        Import {
            module: module.to_owned(),
            names: names.iter().map(|s| s.to_string()).collect(),
            is_type_only: false,
            line: 1,
        }
    }

    fn make_call(callee: &str, line: usize) -> FunctionCall {
        FunctionCall {
            callee: callee.to_owned(),
            line,
            end_line: line,
            snippet: format!("{callee}()"),
        }
    }

    fn make_macro_call(name: &str, line: usize) -> MacroCall {
        MacroCall {
            name: name.to_owned(),
            line,
        }
    }

    fn make_rust_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        }
    }

    fn make_ts_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::TypeScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR::default()),
            file_doc: None,
        }
    }

    fn make_js_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::JavaScript,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR::default()),
            file_doc: None,
        }
    }

    fn make_python_file(path: &str) -> ProjectFile {
        ProjectFile {
            path: PathBuf::from(path),
            language: seshat_core::Language::Python,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR::default()),
            file_doc: None,
        }
    }

    // -- Basic matching --

    #[test]
    fn basic_import_to_call_match() {
        let imports = vec![make_import("tracing", &["info", "warn", "error"])];
        let calls = vec![make_call("info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 10);
    }

    #[test]
    fn no_match_when_callee_not_in_import_names() {
        let imports = vec![make_import("tracing", &["info", "warn"])];
        let calls = vec![make_call("debug", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert!(result.is_empty());
    }

    // -- Deduplication --

    #[test]
    fn dedup_by_callee_two_identical_calls() {
        let imports = vec![make_import("tracing", &["info"])];
        let calls = vec![make_call("info", 10), make_call("info", 20)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 10); // first occurrence kept
    }

    #[test]
    fn diverse_callees_preserved() {
        let imports = vec![make_import("tracing", &["info", "warn", "error"])];
        let calls = vec![
            make_call("info", 10),
            make_call("warn", 20),
            make_call("error", 30),
        ];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].line, 10);
        assert_eq!(result[1].line, 20);
        assert_eq!(result[2].line, 30);
    }

    // -- Max limit --

    #[test]
    fn max_limit_respected() {
        let imports = vec![make_import(
            "tracing",
            &["info", "warn", "error", "debug", "trace", "fatal"],
        )];
        let calls = vec![
            make_call("info", 1),
            make_call("warn", 2),
            make_call("error", 3),
            make_call("debug", 4),
            make_call("trace", 5),
            make_call("fatal", 6),
        ];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 3);
        assert_eq!(result.len(), 3);
    }

    // -- Empty inputs --

    #[test]
    fn empty_imports_returns_empty() {
        let calls = vec![make_call("info", 10)];
        let result = find_usage_evidence(&[], &calls, &file_path(), 5);
        assert!(result.is_empty());
    }

    #[test]
    fn empty_calls_returns_empty() {
        let imports = vec![make_import("tracing", &["info"])];
        let result = find_usage_evidence(&imports, &[], &file_path(), 5);
        assert!(result.is_empty());
    }

    // -- Multiple imports from same module --

    #[test]
    fn multiple_imports_from_same_module_all_names_considered() {
        let imports = vec![
            make_import("tracing", &["info", "warn"]),
            make_import("tracing::subscriber", &["Layer"]),
        ];
        let calls = vec![make_call("info", 10), make_call("Layer", 20)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 2);
    }

    // -- Cross-module mismatch --

    #[test]
    fn cross_module_mismatch_no_false_positive() {
        let imports = vec![make_import("module_a", &["info"])];
        let calls = vec![make_call("module_b::info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert!(result.is_empty());
    }

    // -- Namespaced calls (Strategy A: module match) --

    #[test]
    fn namespaced_call_module_match() {
        let imports = vec![make_import("tracing", &["info", "warn"])];
        let calls = vec![make_call("tracing::info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
    }

    // -- Namespaced calls (Strategy B: type name match) --

    #[test]
    fn namespaced_call_type_in_import_names() {
        let imports = vec![make_import("reqwest", &["Client"])];
        let calls = vec![make_call("Client::new", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
    }

    // -- Method calls --

    #[test]
    fn method_call_receiver_in_import_names() {
        let imports = vec![make_import("winston", &["logger"])];
        let calls = vec![make_call("logger.info", 10)];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
    }

    // -- Snippet preserved --

    #[test]
    fn snippet_from_call_preserved() {
        let imports = vec![make_import("tracing", &["info"])];
        let calls = vec![FunctionCall {
            callee: "info".to_owned(),
            line: 10,
            end_line: 12,
            snippet: "info!(\"starting server\", port = 3000)".to_string(),
        }];
        let result = find_usage_evidence(&imports, &calls, &file_path(), 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].snippet, "info!(\"starting server\", port = 3000)");
        assert_eq!(result[0].end_line, 12);
    }

    // -----------------------------------------------------------------------
    // find_usage_evidence_for_file — per-language tests
    // -----------------------------------------------------------------------

    // -- Rust: macro_calls converted to synthetic FunctionCalls --

    #[test]
    fn rust_macro_calls_matched() {
        let mut file = make_rust_file("src/lib.rs");
        file.imports = vec![make_import("tracing", &["info", "warn", "error"])];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![
                make_macro_call("info", 10),
                make_macro_call("warn", 20),
                make_macro_call("error", 30),
            ];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].line, 10);
        assert_eq!(result[1].line, 20);
        assert_eq!(result[2].line, 30);
    }

    #[test]
    fn rust_function_calls_matched() {
        let mut file = make_rust_file("src/main.rs");
        file.imports = vec![make_import("reqwest", &["Client"])];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("Client::new", 15)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 15);
    }

    #[test]
    fn rust_macro_and_function_calls_combined() {
        let mut file = make_rust_file("src/handler.rs");
        file.imports = vec![
            make_import("tracing", &["info"]),
            make_import("anyhow", &["Result"]),
        ];
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![make_macro_call("info", 5)];
            ir.function_calls = vec![make_call("Result::Ok", 10)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        // info macro matched, Result::Ok matched via strategy B (Result in names)
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn rust_no_imports_returns_empty() {
        let mut file = make_rust_file("src/lib.rs");
        // no imports set — file.imports stays empty
        if let LanguageIR::Rust(ref mut ir) = file.language_ir {
            ir.macro_calls = vec![make_macro_call("info", 10)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }

    // -- TypeScript: function_calls matched against imports --

    #[test]
    fn typescript_function_calls_matched() {
        let mut file = make_ts_file("src/logger.ts");
        file.imports = vec![make_import("winston", &["logger"])];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("logger.info", 8), make_call("logger.warn", 15)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].line, 8);
        assert_eq!(result[1].line, 15);
    }

    #[test]
    fn typescript_unrelated_calls_not_matched() {
        let mut file = make_ts_file("src/app.ts");
        file.imports = vec![make_import("winston", &["logger"])];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("console.log", 5)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }

    #[test]
    fn typescript_max_limit_respected() {
        let mut file = make_ts_file("src/service.ts");
        file.imports = vec![make_import("jest", &["expect", "describe", "it", "test"])];
        if let LanguageIR::TypeScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![
                make_call("expect", 10),
                make_call("describe", 20),
                make_call("it", 30),
                make_call("test", 40),
            ];
        }
        let result = find_usage_evidence_for_file(&file, 2);
        assert_eq!(result.len(), 2);
    }

    // -- JavaScript: function_calls matched against imports --

    #[test]
    fn javascript_function_calls_matched() {
        let mut file = make_js_file("src/routes.js");
        file.imports = vec![make_import("express", &["Router"])];
        if let LanguageIR::JavaScript(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("Router", 3)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 3);
    }

    #[test]
    fn javascript_empty_calls_returns_empty() {
        let mut file = make_js_file("src/utils.js");
        file.imports = vec![make_import("lodash", &["map", "filter"])];
        // no function_calls set — stays empty
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }

    #[test]
    fn javascript_dedup_by_callee() {
        let mut file = make_js_file("src/index.js");
        file.imports = vec![make_import("lodash", &["map"])];
        if let LanguageIR::JavaScript(ref mut ir) = file.language_ir {
            // Two calls to "map" at different lines — only first should appear
            ir.function_calls = vec![make_call("map", 5), make_call("map", 15)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 5);
    }

    // -- Python: function_calls matched against imports --

    #[test]
    fn python_function_calls_matched() {
        let mut file = make_python_file("src/app.py");
        file.imports = vec![make_import("logging", &["getLogger"])];
        if let LanguageIR::Python(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("getLogger", 4)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 4);
    }

    #[test]
    fn python_method_call_matched() {
        let mut file = make_python_file("tests/test_api.py");
        file.imports = vec![make_import("pytest", &["raises"])];
        if let LanguageIR::Python(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("raises", 20)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line, 20);
    }

    #[test]
    fn python_no_matching_calls_returns_empty() {
        let mut file = make_python_file("src/utils.py");
        file.imports = vec![make_import("os", &["path"])];
        if let LanguageIR::Python(ref mut ir) = file.language_ir {
            ir.function_calls = vec![make_call("print", 1)];
        }
        let result = find_usage_evidence_for_file(&file, 5);
        assert!(result.is_empty());
    }
}
