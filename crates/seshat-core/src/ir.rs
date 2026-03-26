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
}

/// A type definition (struct, enum, interface, class, type alias).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeDefKind,
    pub is_public: bool,
    pub line: usize,
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

/// Rust-specific IR details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RustIR {
    pub mod_declarations: Vec<String>,
    pub derive_macros: Vec<DeriveUsage>,
    pub trait_implementations: Vec<TraitImpl>,
    pub error_types: Vec<String>,
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
}

/// JavaScript-specific IR details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct JavaScriptIR {
    pub module_system: ModuleSystem,
    pub has_module_exports: bool,
    pub require_calls: Vec<String>,
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
            }],
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
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
