//! Synthetic definition snippets used by both the symbol-index writers
//! (`seshat-storage`) and the symbol-index readers (`seshat-graph`).
//!
//! Lives in `seshat-core` so the storage layer (which fills the
//! `symbol_definitions.snippet` column) and the graph layer (which renders
//! `query_code_pattern` results) build snippets the same way without one
//! crate depending on the other.

use crate::{Export, Function, TypeDef};

/// Maximum lines retained when truncating a definition snippet.
///
/// Matches the previous per-call truncation limit in `code_pattern.rs`
/// (`MAX_PATTERN_SNIPPET_LINES = 10`).
pub const MAX_DEFINITION_SNIPPET_LINES: usize = 10;

/// Build a synthetic snippet for a function definition.
///
/// Format: `[pub ][async ]fn <name>(<params>)` — matches what
/// `query_code_pattern` historically returned for function results.
#[must_use]
pub fn function_definition_snippet(f: &Function) -> String {
    let vis = if f.is_public { "pub " } else { "" };
    let async_kw = if f.is_async { "async " } else { "" };
    let params = f.parameters.join(", ");
    format!("{vis}{async_kw}fn {}({params})", f.name)
}

/// Build a synthetic snippet for a type definition.
///
/// Format: `[pub ]<kind> <name>` — kind is the lowercase
/// [`TypeDefKind`](crate::TypeDefKind) variant (`struct`, `enum`, `trait`, …).
#[must_use]
pub fn type_definition_snippet(t: &TypeDef) -> String {
    let vis = if t.is_public { "pub " } else { "" };
    let kind = format!("{:?}", t.kind).to_lowercase();
    format!("{vis}{kind} {}", t.name)
}

/// Build a synthetic snippet for an export declaration.
///
/// Format: `export [default ][type ]<name>` — matches what
/// `query_code_pattern` historically returned for export results.
#[must_use]
pub fn export_definition_snippet(e: &Export) -> String {
    let default = if e.is_default { "default " } else { "" };
    let type_only = if e.is_type_only { "type " } else { "" };
    format!("export {default}{type_only}{}", e.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TypeDefKind;

    #[test]
    fn function_snippet_includes_pub_and_async() {
        let f = Function {
            name: "foo".to_owned(),
            is_public: true,
            is_async: true,
            line: 1,
            end_line: 1,
            parameters: vec!["a".to_owned(), "b".to_owned()],
            doc_comment: None,
        };
        assert_eq!(function_definition_snippet(&f), "pub async fn foo(a, b)");
    }

    #[test]
    fn function_snippet_private_sync() {
        let f = Function {
            name: "bar".to_owned(),
            is_public: false,
            is_async: false,
            line: 1,
            end_line: 1,
            parameters: vec![],
            doc_comment: None,
        };
        assert_eq!(function_definition_snippet(&f), "fn bar()");
    }

    #[test]
    fn type_snippet_struct_pub() {
        let t = TypeDef {
            name: "Foo".to_owned(),
            kind: TypeDefKind::Struct,
            is_public: true,
            line: 1,
            end_line: 5,
            doc_comment: None,
        };
        assert_eq!(type_definition_snippet(&t), "pub struct Foo");
    }

    #[test]
    fn type_snippet_typealias_private() {
        let t = TypeDef {
            name: "Alias".to_owned(),
            kind: TypeDefKind::TypeAlias,
            is_public: false,
            line: 1,
            end_line: 1,
            doc_comment: None,
        };
        assert_eq!(type_definition_snippet(&t), "typealias Alias");
    }

    #[test]
    fn export_snippet_default_type_only() {
        let e = Export {
            name: "Foo".to_owned(),
            is_default: true,
            is_type_only: true,
            line: 1,
            end_line: 1,
        };
        assert_eq!(export_definition_snippet(&e), "export default type Foo");
    }

    #[test]
    fn export_snippet_plain() {
        let e = Export {
            name: "bar".to_owned(),
            is_default: false,
            is_type_only: false,
            line: 1,
            end_line: 1,
        };
        assert_eq!(export_definition_snippet(&e), "export bar");
    }
}
