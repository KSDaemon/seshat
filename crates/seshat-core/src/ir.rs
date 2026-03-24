use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
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
