use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

use crate::error::ParseEnumError;

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
}

impl Language {
    /// Return the canonical snake_case representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rust => write!(f, "Rust"),
            Self::TypeScript => write!(f, "TypeScript"),
            Self::JavaScript => write!(f, "JavaScript"),
            Self::Python => write!(f, "Python"),
        }
    }
}

impl std::str::FromStr for Language {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rust" => Ok(Self::Rust),
            "typescript" => Ok(Self::TypeScript),
            "javascript" => Ok(Self::JavaScript),
            "python" => Ok(Self::Python),
            _ => Err(ParseEnumError {
                type_name: "Language",
                value: s.to_owned(),
            }),
        }
    }
}

impl Language {
    /// Returns file extensions associated with this language.
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["rs"],
            Self::TypeScript => &["ts", "tsx"],
            Self::JavaScript => &["js", "jsx", "mjs", "cjs"],
            Self::Python => &["py"],
        }
    }

    /// All supported language variants for iteration.
    pub fn all() -> &'static [Language] {
        &[Self::Rust, Self::TypeScript, Self::JavaScript, Self::Python]
    }

    /// Detect language from a file extension (without the leading dot).
    ///
    /// Returns `None` for unrecognised extensions.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "py" => Some(Self::Python),
            _ => None,
        }
    }

    /// Visibility/export marker rendered before a public symbol in this
    /// language's syntax. Empty string when the symbol is private or when the
    /// language has no syntactic visibility keyword.
    ///
    /// Used by [`crate::symbol_snippet`] so synthetic definition snippets read
    /// natively for each language instead of always borrowing Rust's `pub`.
    ///
    /// - Rust: `pub ` for public, `""` for private.
    /// - TypeScript / JavaScript: `export ` for public, `""` for private.
    ///   (TS/JS parsers set `is_public = true` exactly when a symbol carries
    ///   the `export` keyword.)
    /// - Python: always `""` — Python has no syntactic visibility marker.
    #[must_use]
    pub fn visibility_keyword(self, is_public: bool) -> &'static str {
        if !is_public {
            return "";
        }
        match self {
            Self::Rust => "pub ",
            Self::TypeScript | Self::JavaScript => "export ",
            Self::Python => "",
        }
    }

    /// Source keyword for declaring a function in this language: `fn` /
    /// `function` / `def`.
    #[must_use]
    pub fn function_keyword(self) -> &'static str {
        match self {
            Self::Rust => "fn",
            Self::TypeScript | Self::JavaScript => "function",
            Self::Python => "def",
        }
    }
}

/// Normalized intermediate representation of a parsed source file.
///
/// Common fields are shared across all languages. Language-specific
/// details live in the `language_ir` enum variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProjectFile {
    pub path: PathBuf,
    pub language: Language,
    pub content_hash: String,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub functions: Vec<Function>,
    pub types: Vec<TypeDef>,
    pub dependencies_used: Vec<DependencyUsage>,
    pub language_ir: LanguageIR,
    /// File-level doc comment extracted by the parser.
    ///
    /// - Rust: `//!` inner doc comment at the top of the file.
    /// - Python: module-level docstring (first `"""..."""` or `'''...'''`).
    /// - TypeScript/JavaScript: leading `/** ... */` or `//` comment block.
    ///
    /// `None` when no file-level documentation is present or the parser
    /// has not yet been updated to extract it.
    #[serde(default)]
    pub file_doc: Option<String>,
}

/// An import statement extracted from source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Import {
    pub module: String,
    pub names: Vec<String>,
    pub is_type_only: bool,
    pub line: usize,
}

/// An export declaration extracted from source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Export {
    pub name: String,
    pub is_default: bool,
    pub is_type_only: bool,
    pub line: usize,
    /// 1-indexed source line where the export declaration ends.
    ///
    /// Equals [`Self::line`] for single-line statements such as
    /// `pub use foo::*;`, `export { Foo };`, or `type Alias = X;`. For
    /// multi-line declarations (e.g. `export class Foo { ... }`) this is the
    /// closing line of the declaration node — matching the existing
    /// [`Function::end_line`] semantics. Hunk-intersection logic in
    /// `map_diff_impact` uses `[line, end_line]` as the symbol's range.
    ///
    /// Required (no `#[serde(default)]`): IR_SCHEMA_VERSION 8 added this
    /// field; older v7 IR rows fail StaleIR detection and are re-scanned,
    /// so deserialisation here should never legitimately encounter a
    /// missing value. Failing loudly surfaces actual data corruption.
    pub end_line: usize,
}

/// A function or method definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Function {
    pub name: String,
    pub is_public: bool,
    pub is_async: bool,
    pub line: usize,
    pub end_line: usize,
    /// Parameter names extracted by tree-sitter (empty if not yet extracted).
    #[serde(default)]
    pub parameters: Vec<String>,
    /// Doc comment / docstring attached to this function.
    ///
    /// - Rust: consecutive `///` lines immediately preceding the function.
    /// - Python: triple-quoted string as the first statement of the body.
    /// - TypeScript/JavaScript: JSDoc `/** ... */` comment preceding the function.
    ///
    /// `None` when absent or when the parser has not yet been updated.
    #[serde(default)]
    pub doc_comment: Option<String>,
}

/// A type definition (struct, enum, interface, class, type alias).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeDefKind,
    pub is_public: bool,
    pub line: usize,
    /// 1-indexed source line where the type definition ends.
    ///
    /// Equals [`Self::line`] for single-line type aliases (`type Alias = X;`).
    /// For multi-line declarations (struct, enum, trait, interface, class)
    /// this is the closing line of the declaration node — matching the
    /// existing [`Function::end_line`] semantics. Hunk-intersection logic in
    /// `map_diff_impact` uses `[line, end_line]` as the symbol's range.
    ///
    /// Required (no `#[serde(default)]`): IR_SCHEMA_VERSION 8 added this
    /// field; older v7 IR rows fail StaleIR detection and are re-scanned,
    /// so deserialisation here should never legitimately encounter a
    /// missing value. Failing loudly surfaces actual data corruption.
    pub end_line: usize,
    /// Doc comment attached to this type definition.
    ///
    /// Same conventions as [`Function::doc_comment`].
    /// `None` when absent or parser not yet updated.
    #[serde(default)]
    pub doc_comment: Option<String>,
}

/// The kind of a type definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeDefKind {
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    TypeAlias,
}

impl TypeDefKind {
    /// Source keyword used to declare a type of this kind: `struct` / `enum` /
    /// `trait` / `interface` / `class` / `type`.
    ///
    /// Note `TypeAlias` renders as `type`, matching both Rust's `type Foo =
    /// …;` and TS's `type Foo = …;`. The old debug-derived spelling
    /// `typealias` was not valid syntax in any supported language.
    #[must_use]
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::TypeAlias => "type",
        }
    }
}

/// A dependency usage reference found in source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DependencyUsage {
    pub package: String,
    pub import_path: String,
    pub line: usize,
}

/// Language-specific IR details.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageIR {
    Rust(RustIR),
    TypeScript(TypeScriptIR),
    JavaScript(JavaScriptIR),
    Python(PythonIR),
}

/// A `mod foo;` or `mod foo { ... }` declaration in a Rust file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ModDeclaration {
    /// Name of the declared module (e.g. `"config"`, `"tests"`).
    pub name: String,
    /// 1-indexed source line of the `mod` keyword.
    pub line: usize,
}

/// A macro invocation in a Rust file (e.g. `tracing::info!(...)`, `vec![...]`).
///
/// Stores the full macro path as written in source and the call-site line so
/// that detectors can point to real usage rather than import declarations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MacroCall {
    /// Full macro name as written, e.g. `"tracing::info"`, `"vec"`.
    pub name: String,
    /// 1-indexed source line of the macro invocation.
    pub line: usize,
}

/// A function or method call-site in a Rust file.
///
/// Stores **one example per unique callee name** (deduplication happens in the
/// parser).  The snippet captures a window around the call so that MCP clients
/// can show real usage patterns without additional disk I/O at query time.
///
/// # Snippet layout
///
/// ```text
/// [2 lines of context before the opening line]
/// [all lines of the call expression — may span many lines for multi-arg calls]
/// [4 lines of context after the closing line]
/// ```
///
/// Hard cap: 30 lines total.  Lines are taken from the raw source file and
/// include the original indentation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct FunctionCall {
    /// Full callee name as written in source, e.g. `"scan_project"`,
    /// `"Arc::new"`, `"db.execute"`, `"tracing::info"`.
    pub callee: String,
    /// 1-indexed line of the **opening** of the call expression (function name
    /// position).
    pub line: usize,
    /// 1-indexed line of the **closing parenthesis** of the call expression.
    /// Equals `line` for single-line calls.
    pub end_line: usize,
    /// Multi-line snippet centered on the call site (see type-level docs).
    /// Empty string if source was unavailable at parse time.
    pub snippet: String,
}

/// Rust-specific IR details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RustIR {
    pub mod_declarations: Vec<ModDeclaration>,
    pub derive_macros: Vec<DeriveUsage>,
    pub trait_implementations: Vec<TraitImpl>,
    pub error_types: Vec<String>,
    /// All macro invocations found in this file.
    ///
    /// Populated by the Rust tree-sitter parser.  Detectors use this to
    /// produce call-site evidence (e.g. `tracing::info!` lines) instead of
    /// pointing at import declarations.
    #[serde(default)]
    pub macro_calls: Vec<MacroCall>,
    /// Function and method call-sites found in this file.
    ///
    /// Deduplicated by callee name — at most one example per unique callee.
    /// Hard limit: 500 entries per file.  Used by `query_code_pattern` to
    /// return real call-site snippets alongside symbol definitions.
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,
}

/// A `#[derive(...)]` usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DeriveUsage {
    pub type_name: String,
    pub derives: Vec<String>,
    pub line: usize,
}

/// A trait implementation (`impl Trait for Type`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TraitImpl {
    pub trait_name: String,
    pub type_name: String,
    pub line: usize,
}

/// TypeScript-specific IR details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TypeScriptIR {
    pub has_barrel_exports: bool,
    pub type_only_imports: Vec<String>,
    pub decorators: Vec<String>,
    pub default_export: bool,
    /// Function and method call-sites found in this file (v7+).
    ///
    /// Deduplicated by callee name — at most one example per unique callee.
    /// Hard limit: 500 entries per file.
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,
}

/// JavaScript-specific IR details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct JavaScriptIR {
    pub module_system: ModuleSystem,
    pub has_module_exports: bool,
    pub require_calls: Vec<String>,
    /// Function and method call-sites found in this file (v7+).
    ///
    /// Deduplicated by callee name — at most one example per unique callee.
    /// Hard limit: 500 entries per file.  `require` calls are excluded
    /// (already captured in `require_calls`).
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,
}

/// JavaScript module system.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleSystem {
    #[default]
    Unknown,
    CommonJS,
    ESM,
}

/// Python-specific IR details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PythonIR {
    pub has_all_export: bool,
    pub is_init_file: bool,
    pub type_hints_used: bool,
    pub decorators: Vec<String>,
    /// Function and method call-sites found in this file (v7+).
    ///
    /// Deduplicated by callee name — at most one example per unique callee.
    /// Hard limit: 500 entries per file.
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_display() {
        assert_eq!(Language::Rust.to_string(), "Rust");
        assert_eq!(Language::TypeScript.to_string(), "TypeScript");
        assert_eq!(Language::JavaScript.to_string(), "JavaScript");
        assert_eq!(Language::Python.to_string(), "Python");
    }

    #[test]
    fn language_roundtrip_str() {
        let langs = [
            Language::Rust,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
        ];
        for l in langs {
            let parsed: Language = l.as_str().parse().unwrap();
            assert_eq!(parsed, l);
        }
    }

    #[test]
    fn language_parse_unknown() {
        assert!("go".parse::<Language>().is_err());
    }

    #[test]
    fn language_extensions() {
        assert_eq!(Language::Rust.extensions(), &["rs"]);
        assert!(Language::TypeScript.extensions().contains(&"tsx"));
        assert!(Language::JavaScript.extensions().contains(&"mjs"));
        assert_eq!(Language::Python.extensions(), &["py"]);
    }

    #[test]
    fn language_from_extension() {
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("jsx"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("mjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("cjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("go"), None);
        assert_eq!(Language::from_extension(""), None);
    }

    #[test]
    fn language_all() {
        let all = Language::all();
        assert_eq!(all.len(), 4);
        assert!(all.contains(&Language::Rust));
        assert!(all.contains(&Language::TypeScript));
        assert!(all.contains(&Language::JavaScript));
        assert!(all.contains(&Language::Python));
    }

    #[test]
    fn language_ir_enum_covers_all_languages() {
        // Verify each variant can be constructed
        let _rust = LanguageIR::Rust(RustIR::default());
        let _ts = LanguageIR::TypeScript(TypeScriptIR::default());
        let _js = LanguageIR::JavaScript(JavaScriptIR::default());
        let _py = LanguageIR::Python(PythonIR::default());
    }

    #[test]
    fn project_file_serialization_roundtrip() {
        let pf = ProjectFile {
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            content_hash: "abc123".to_owned(),
            imports: vec![Import {
                module: "std::io".to_owned(),
                names: vec!["Read".to_owned()],
                is_type_only: false,
                line: 1,
            }],
            exports: Vec::new(),
            functions: vec![Function {
                name: "main".to_owned(),
                is_public: false,
                is_async: false,
                line: 3,
                end_line: 5,
                parameters: vec![],
                doc_comment: None,
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
            file_doc: None,
        };

        let json = serde_json::to_string(&pf).expect("serialize");
        let deserialized: ProjectFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.path, pf.path);
        assert_eq!(deserialized.language, pf.language);
        assert_eq!(deserialized.content_hash, pf.content_hash);
        assert_eq!(deserialized.imports.len(), 1);
        assert_eq!(deserialized.functions.len(), 1);
    }

    #[test]
    fn module_system_default_is_unknown() {
        assert_eq!(ModuleSystem::default(), ModuleSystem::Unknown);
    }
}
