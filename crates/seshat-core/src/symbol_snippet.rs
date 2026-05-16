//! Synthetic definition snippets used by both the symbol-index writers
//! (`seshat-storage`) and the symbol-index readers (`seshat-graph`).
//!
//! Lives in `seshat-core` so the storage layer (which fills the
//! `symbol_definitions.snippet` column) and the graph layer (which renders
//! `query_code_pattern` results) build snippets the same way without one
//! crate depending on the other.
//!
//! Each snippet builder takes the symbol's source [`Language`] so the
//! rendered preview reads natively for the file it came from — `pub fn` for
//! Rust, `export function` for TS/JS, `def` for Python — instead of always
//! borrowing Rust's syntax. The shared per-language keyword tables live on
//! [`Language`] and [`crate::TypeDefKind`].

use crate::{Export, Function, Language, TypeDef};

/// Maximum lines retained when truncating a definition snippet.
///
/// `seshat-graph::code_pattern::MAX_PATTERN_SNIPPET_LINES` is defined as
/// `MAX_DEFINITION_SNIPPET_LINES` so the read- and write-side bounds stay in
/// lockstep; update both if this changes.
pub const MAX_DEFINITION_SNIPPET_LINES: usize = 10;

/// Build a synthetic snippet for a function definition.
///
/// Format: `[<vis>][async ]<fn-kw> <name>(<params>)` where `<vis>` and
/// `<fn-kw>` come from [`Language::visibility_keyword`] and
/// [`Language::function_keyword`].
///
/// Examples by language:
/// - Rust: `pub async fn handle(req)`
/// - TS/JS: `export async function handle(req)`
/// - Python: `async def handle(req)` (Python has no visibility keyword)
#[must_use]
pub fn function_definition_snippet(f: &Function, lang: Language) -> String {
    let vis = lang.visibility_keyword(f.is_public);
    let async_kw = if f.is_async { "async " } else { "" };
    let fn_kw = lang.function_keyword();
    let params = f.parameters.join(", ");
    format!("{vis}{async_kw}{fn_kw} {}({params})", f.name)
}

/// Build a synthetic snippet for a type definition.
///
/// Format: `[<vis>]<kind-kw> <name>` where `<vis>` is
/// [`Language::visibility_keyword`] and `<kind-kw>` is
/// [`crate::TypeDefKind::keyword`].
///
/// Examples by language:
/// - Rust: `pub struct Foo`, `pub trait Service`, `pub type Alias`
/// - TS: `export interface Foo`, `export type Alias`, `export class Foo`
/// - JS: `export class Foo`
/// - Python: `class Foo` (no visibility keyword)
#[must_use]
pub fn type_definition_snippet(t: &TypeDef, lang: Language) -> String {
    let vis = lang.visibility_keyword(t.is_public);
    format!("{vis}{} {}", t.kind.keyword(), t.name)
}

/// Build a synthetic snippet for an export declaration.
///
/// `Export` rows are emitted by every parser (Rust pushes one for every `pub`
/// item, TS/JS for every `export` statement, Python for `__all__` and
/// `from … import …` re-exports). Rendering follows each language's natural
/// syntax instead of forcing the TS-flavoured `export …` form everywhere:
/// - Rust: `pub use <name>` (the closest analog to a re-export marker).
///   `is_default` / `is_type_only` are ignored — neither concept exists in
///   Rust.
/// - TS/JS: `export [default ][type ]<name>` — the literal source syntax.
///   `is_type_only` is TS-only; in JS the parser always leaves it `false`.
/// - Python: bare `<name>` — Python has no syntactic export marker, so any
///   keyword we picked would be misleading.
#[must_use]
pub fn export_definition_snippet(e: &Export, lang: Language) -> String {
    match lang {
        Language::Rust => format!("pub use {}", e.name),
        Language::TypeScript | Language::JavaScript => {
            let default = if e.is_default { "default " } else { "" };
            let type_only = if e.is_type_only { "type " } else { "" };
            format!("export {default}{type_only}{}", e.name)
        }
        Language::Python => e.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TypeDefKind;

    fn mk_fn(name: &str, is_public: bool, is_async: bool, params: Vec<&str>) -> Function {
        Function {
            name: name.to_owned(),
            is_public,
            is_async,
            line: 1,
            end_line: 1,
            parameters: params.into_iter().map(str::to_owned).collect(),
            doc_comment: None,
        }
    }

    fn mk_type(name: &str, kind: TypeDefKind, is_public: bool) -> TypeDef {
        TypeDef {
            name: name.to_owned(),
            kind,
            is_public,
            line: 1,
            end_line: 1,
            doc_comment: None,
        }
    }

    fn mk_export(name: &str, is_default: bool, is_type_only: bool) -> Export {
        Export {
            name: name.to_owned(),
            is_default,
            is_type_only,
            line: 1,
            end_line: 1,
        }
    }

    // ─── Functions ──────────────────────────────────────────────────────────

    #[test]
    fn rust_function_pub_and_async() {
        let f = mk_fn("foo", true, true, vec!["a", "b"]);
        assert_eq!(
            function_definition_snippet(&f, Language::Rust),
            "pub async fn foo(a, b)"
        );
    }

    #[test]
    fn rust_function_private_sync() {
        let f = mk_fn("bar", false, false, vec![]);
        assert_eq!(function_definition_snippet(&f, Language::Rust), "fn bar()");
    }

    #[test]
    fn typescript_function_export_async() {
        let f = mk_fn("handle", true, true, vec!["req"]);
        assert_eq!(
            function_definition_snippet(&f, Language::TypeScript),
            "export async function handle(req)"
        );
    }

    #[test]
    fn javascript_function_no_export() {
        let f = mk_fn("helper", false, false, vec![]);
        assert_eq!(
            function_definition_snippet(&f, Language::JavaScript),
            "function helper()"
        );
    }

    #[test]
    fn python_function_async_no_vis_keyword() {
        // Python parser always sets is_public=false; the keyword is omitted
        // regardless of what callers pass — but we exercise both to pin the
        // contract: Python never emits "pub " or "export ".
        let private = mk_fn("_helper", false, false, vec![]);
        let async_pub = mk_fn("handler", true, true, vec!["req"]);
        assert_eq!(
            function_definition_snippet(&private, Language::Python),
            "def _helper()"
        );
        assert_eq!(
            function_definition_snippet(&async_pub, Language::Python),
            "async def handler(req)"
        );
    }

    // ─── Types ──────────────────────────────────────────────────────────────

    #[test]
    fn rust_type_struct_pub() {
        let t = mk_type("Foo", TypeDefKind::Struct, true);
        assert_eq!(
            type_definition_snippet(&t, Language::Rust),
            "pub struct Foo"
        );
    }

    #[test]
    fn rust_type_alias_private_renders_as_type_not_typealias() {
        // Regression: the old `format!("{:?}", kind).to_lowercase()` rendering
        // produced "typealias" which is not valid Rust syntax. `type` is.
        let t = mk_type("Alias", TypeDefKind::TypeAlias, false);
        assert_eq!(type_definition_snippet(&t, Language::Rust), "type Alias");
    }

    #[test]
    fn typescript_type_interface_export() {
        let t = mk_type("Handle", TypeDefKind::Interface, true);
        assert_eq!(
            type_definition_snippet(&t, Language::TypeScript),
            "export interface Handle"
        );
    }

    #[test]
    fn typescript_type_alias_export() {
        let t = mk_type("Id", TypeDefKind::TypeAlias, true);
        assert_eq!(
            type_definition_snippet(&t, Language::TypeScript),
            "export type Id"
        );
    }

    #[test]
    fn javascript_class_export() {
        let t = mk_type("Foo", TypeDefKind::Class, true);
        assert_eq!(
            type_definition_snippet(&t, Language::JavaScript),
            "export class Foo"
        );
    }

    #[test]
    fn python_class_no_vis_keyword() {
        let t = mk_type("Foo", TypeDefKind::Class, false);
        assert_eq!(type_definition_snippet(&t, Language::Python), "class Foo");
    }

    // ─── Exports ────────────────────────────────────────────────────────────

    #[test]
    fn rust_export_renders_as_pub_use() {
        let e = mk_export("ApiHandle", false, false);
        assert_eq!(
            export_definition_snippet(&e, Language::Rust),
            "pub use ApiHandle"
        );
    }

    #[test]
    fn rust_export_ignores_default_and_type_only_flags() {
        // is_default / is_type_only don't exist in Rust; the Rust parser
        // sets is_type_only=true for type-ish items but the snippet still
        // renders as `pub use <name>` so it's recognisably Rust.
        let e = mk_export("Foo", true, true);
        assert_eq!(export_definition_snippet(&e, Language::Rust), "pub use Foo");
    }

    #[test]
    fn typescript_export_default_type_only() {
        let e = mk_export("Foo", true, true);
        assert_eq!(
            export_definition_snippet(&e, Language::TypeScript),
            "export default type Foo"
        );
    }

    #[test]
    fn javascript_export_plain() {
        let e = mk_export("bar", false, false);
        assert_eq!(
            export_definition_snippet(&e, Language::JavaScript),
            "export bar"
        );
    }

    #[test]
    fn python_export_renders_as_bare_name() {
        let e = mk_export("Bar", false, false);
        assert_eq!(export_definition_snippet(&e, Language::Python), "Bar");
    }
}
