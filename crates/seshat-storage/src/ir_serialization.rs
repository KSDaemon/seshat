//! Bincode IR serialization with version prefix (ADR-16).
//!
//! Serialized format: `[IR_SCHEMA_VERSION: u8] [bincode data: ...]`
//!
//! When `ProjectFile` changes, bump [`IR_SCHEMA_VERSION`] to auto-invalidate
//! all cached IR — stale rows trigger a re-parse instead of a migration.

use seshat_core::ProjectFile;

use crate::StorageError;

/// Current schema version for serialized IR data.
///
/// Bump this whenever the `ProjectFile` struct (or any type it transitively
/// contains) changes in a way that is incompatible with previously serialized
/// data.
pub const IR_SCHEMA_VERSION: u8 = 1;

/// Serialize a [`ProjectFile`] to bytes with a version prefix.
///
/// The first byte is [`IR_SCHEMA_VERSION`], followed by the bincode payload.
pub fn serialize_ir(ir: &ProjectFile) -> Result<Vec<u8>, StorageError> {
    let mut buf = vec![IR_SCHEMA_VERSION];
    bincode::serialize_into(&mut buf, ir)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;
    Ok(buf)
}

/// Deserialize a [`ProjectFile`] from versioned bytes.
///
/// Returns [`StorageError::StaleIR`] if the version prefix does not match
/// [`IR_SCHEMA_VERSION`] (the caller should trigger a re-parse).
pub fn deserialize_ir(data: &[u8]) -> Result<ProjectFile, StorageError> {
    if data.is_empty() {
        return Err(StorageError::SerializationError(
            "IR data is empty".to_string(),
        ));
    }

    let version = data[0];
    if version != IR_SCHEMA_VERSION {
        return Err(StorageError::StaleIR {
            cached: version,
            current: IR_SCHEMA_VERSION,
        });
    }

    bincode::deserialize(&data[1..]).map_err(|e| StorageError::SerializationError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use seshat_core::Language;
    use seshat_core::ir::{
        DeriveUsage, Export, Function, Import, JavaScriptIR, LanguageIR, ModuleSystem, PythonIR,
        RustIR, TraitImpl, TypeDef, TypeDefKind, TypeScriptIR,
    };
    use std::path::PathBuf;

    /// Build a minimal ProjectFile for testing.
    fn minimal_project_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            content_hash: "abc123def456".to_string(),
            imports: vec![Import {
                module: "std::io".to_string(),
                names: vec!["Read".to_string(), "Write".to_string()],
                is_type_only: false,
                line: 1,
            }],
            exports: vec![Export {
                name: "main".to_string(),
                is_default: false,
                is_type_only: false,
                line: 5,
            }],
            functions: vec![Function {
                name: "main".to_string(),
                is_public: true,
                is_async: false,
                line: 5,
                end_line: 10,
            }],
            types: vec![TypeDef {
                name: "Config".to_string(),
                kind: TypeDefKind::Struct,
                is_public: true,
                line: 12,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR {
                mod_declarations: vec!["config".to_string()],
                derive_macros: vec![DeriveUsage {
                    type_name: "Config".to_string(),
                    derives: vec!["Debug".to_string(), "Clone".to_string()],
                    line: 11,
                }],
                trait_implementations: vec![TraitImpl {
                    trait_name: "Display".to_string(),
                    type_name: "Config".to_string(),
                    line: 20,
                }],
                error_types: vec!["AppError".to_string()],
            }),
        }
    }

    /// Build a rich ProjectFile with TypeScript IR for roundtrip testing.
    fn typescript_project_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/app.tsx"),
            language: Language::TypeScript,
            content_hash: "ts_hash_xyz".to_string(),
            imports: vec![
                Import {
                    module: "react".to_string(),
                    names: vec!["React".to_string()],
                    is_type_only: false,
                    line: 1,
                },
                Import {
                    module: "./types".to_string(),
                    names: vec!["AppConfig".to_string()],
                    is_type_only: true,
                    line: 2,
                },
            ],
            exports: vec![Export {
                name: "App".to_string(),
                is_default: true,
                is_type_only: false,
                line: 10,
            }],
            functions: vec![Function {
                name: "App".to_string(),
                is_public: true,
                is_async: false,
                line: 10,
                end_line: 30,
            }],
            types: vec![TypeDef {
                name: "AppProps".to_string(),
                kind: TypeDefKind::Interface,
                is_public: true,
                line: 5,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::TypeScript(TypeScriptIR {
                has_barrel_exports: false,
                type_only_imports: vec!["./types".to_string()],
                decorators: vec!["Component".to_string()],
                default_export: true,
            }),
        }
    }

    /// Build a ProjectFile with JavaScript CommonJS IR.
    fn javascript_project_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("src/config.cjs"),
            language: Language::JavaScript,
            content_hash: "js_hash_abc".to_string(),
            imports: Vec::new(),
            exports: vec![Export {
                name: "config".to_string(),
                is_default: false,
                is_type_only: false,
                line: 5,
            }],
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::JavaScript(JavaScriptIR {
                module_system: ModuleSystem::CommonJS,
                has_module_exports: true,
                require_calls: vec!["path".to_string(), "fs".to_string()],
            }),
        }
    }

    /// Build a ProjectFile with Python IR.
    fn python_project_file() -> ProjectFile {
        ProjectFile {
            path: PathBuf::from("mypackage/__init__.py"),
            language: Language::Python,
            content_hash: "py_hash_def".to_string(),
            imports: vec![Import {
                module: "os".to_string(),
                names: vec!["path".to_string()],
                is_type_only: false,
                line: 1,
            }],
            exports: vec![Export {
                name: "MyClass".to_string(),
                is_default: false,
                is_type_only: false,
                line: 3,
            }],
            functions: vec![Function {
                name: "helper".to_string(),
                is_public: false,
                is_async: true,
                line: 10,
                end_line: 15,
            }],
            types: vec![TypeDef {
                name: "MyClass".to_string(),
                kind: TypeDefKind::Class,
                is_public: true,
                line: 20,
            }],
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Python(PythonIR {
                has_all_export: true,
                is_init_file: true,
                type_hints_used: true,
                decorators: vec!["dataclass".to_string()],
            }),
        }
    }

    // ------ Roundtrip tests ------

    #[test]
    fn roundtrip_rust_project_file() {
        let original = minimal_project_file();
        let bytes = serialize_ir(&original).expect("serialize");
        let restored = deserialize_ir(&bytes).expect("deserialize");

        assert_eq!(restored.path, original.path);
        assert_eq!(restored.language, original.language);
        assert_eq!(restored.content_hash, original.content_hash);
        assert_eq!(restored.imports.len(), original.imports.len());
        assert_eq!(restored.imports[0].module, "std::io");
        assert_eq!(restored.imports[0].names, vec!["Read", "Write"]);
        assert_eq!(restored.exports.len(), 1);
        assert_eq!(restored.functions.len(), 1);
        assert_eq!(restored.functions[0].name, "main");
        assert!(restored.functions[0].is_public);
        assert_eq!(restored.types.len(), 1);
        assert_eq!(restored.types[0].name, "Config");
        assert_eq!(restored.types[0].kind, TypeDefKind::Struct);

        match &restored.language_ir {
            LanguageIR::Rust(ir) => {
                assert_eq!(ir.mod_declarations, vec!["config"]);
                assert_eq!(ir.derive_macros.len(), 1);
                assert_eq!(ir.derive_macros[0].type_name, "Config");
                assert_eq!(ir.derive_macros[0].derives, vec!["Debug", "Clone"]);
                assert_eq!(ir.trait_implementations.len(), 1);
                assert_eq!(ir.trait_implementations[0].trait_name, "Display");
                assert_eq!(ir.error_types, vec!["AppError"]);
            }
            other => panic!("Expected Rust IR, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_typescript_project_file() {
        let original = typescript_project_file();
        let bytes = serialize_ir(&original).expect("serialize");
        let restored = deserialize_ir(&bytes).expect("deserialize");

        assert_eq!(restored.path, original.path);
        assert_eq!(restored.language, Language::TypeScript);
        assert_eq!(restored.imports.len(), 2);
        assert!(restored.imports[1].is_type_only);

        match &restored.language_ir {
            LanguageIR::TypeScript(ir) => {
                assert!(!ir.has_barrel_exports);
                assert_eq!(ir.type_only_imports, vec!["./types"]);
                assert_eq!(ir.decorators, vec!["Component"]);
                assert!(ir.default_export);
            }
            other => panic!("Expected TypeScript IR, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_javascript_project_file() {
        let original = javascript_project_file();
        let bytes = serialize_ir(&original).expect("serialize");
        let restored = deserialize_ir(&bytes).expect("deserialize");

        assert_eq!(restored.language, Language::JavaScript);

        match &restored.language_ir {
            LanguageIR::JavaScript(ir) => {
                assert_eq!(ir.module_system, ModuleSystem::CommonJS);
                assert!(ir.has_module_exports);
                assert_eq!(ir.require_calls, vec!["path", "fs"]);
            }
            other => panic!("Expected JavaScript IR, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_python_project_file() {
        let original = python_project_file();
        let bytes = serialize_ir(&original).expect("serialize");
        let restored = deserialize_ir(&bytes).expect("deserialize");

        assert_eq!(restored.language, Language::Python);
        assert!(restored.functions[0].is_async);

        match &restored.language_ir {
            LanguageIR::Python(ir) => {
                assert!(ir.has_all_export);
                assert!(ir.is_init_file);
                assert!(ir.type_hints_used);
                assert_eq!(ir.decorators, vec!["dataclass"]);
            }
            other => panic!("Expected Python IR, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_empty_project_file() {
        let original = ProjectFile {
            path: PathBuf::from("empty.rs"),
            language: Language::Rust,
            content_hash: String::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            dependencies_used: Vec::new(),
            language_ir: LanguageIR::Rust(RustIR::default()),
        };

        let bytes = serialize_ir(&original).expect("serialize");
        let restored = deserialize_ir(&bytes).expect("deserialize");

        assert_eq!(restored.path, original.path);
        assert!(restored.imports.is_empty());
        assert!(restored.exports.is_empty());
        assert!(restored.functions.is_empty());
        assert!(restored.types.is_empty());
    }

    // ------ Version prefix tests ------

    #[test]
    fn serialized_data_starts_with_version_byte() {
        let pf = minimal_project_file();
        let bytes = serialize_ir(&pf).expect("serialize");

        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], IR_SCHEMA_VERSION);
    }

    #[test]
    fn version_mismatch_returns_stale_ir_error() {
        let pf = minimal_project_file();
        let mut bytes = serialize_ir(&pf).expect("serialize");

        // Tamper with version byte
        bytes[0] = IR_SCHEMA_VERSION + 1;

        let result = deserialize_ir(&bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::StaleIR { cached, current } => {
                assert_eq!(cached, IR_SCHEMA_VERSION + 1);
                assert_eq!(current, IR_SCHEMA_VERSION);
            }
            other => panic!("Expected StaleIR, got {other:?}"),
        }
    }

    #[test]
    fn version_zero_returns_stale_ir_error() {
        let pf = minimal_project_file();
        let mut bytes = serialize_ir(&pf).expect("serialize");

        bytes[0] = 0;

        let result = deserialize_ir(&bytes);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            StorageError::StaleIR {
                cached: 0,
                current: 1
            }
        ));
    }

    #[test]
    fn empty_data_returns_serialization_error() {
        let result = deserialize_ir(&[]);
        assert!(result.is_err());
        match result.unwrap_err() {
            StorageError::SerializationError(msg) => {
                assert!(msg.contains("empty"), "Expected 'empty' in: {msg}");
            }
            other => panic!("Expected SerializationError, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_data_returns_serialization_error() {
        // Valid version, but garbage bincode payload
        let data = vec![IR_SCHEMA_VERSION, 0xFF, 0xFF, 0xFF, 0xFF];
        let result = deserialize_ir(&data);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            StorageError::SerializationError(_)
        ));
    }

    #[test]
    fn version_byte_only_returns_serialization_error() {
        // Just the version byte, no bincode data
        let data = vec![IR_SCHEMA_VERSION];
        let result = deserialize_ir(&data);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            StorageError::SerializationError(_)
        ));
    }

    // ------ Size comparison test ------

    #[test]
    fn bincode_is_smaller_than_json() {
        let pf = minimal_project_file();
        let bincode_bytes = serialize_ir(&pf).expect("serialize bincode");
        let json_bytes = serde_json::to_vec(&pf).expect("serialize json");

        // Bincode should be significantly more compact than JSON
        assert!(
            bincode_bytes.len() < json_bytes.len(),
            "bincode ({}) should be smaller than JSON ({})",
            bincode_bytes.len(),
            json_bytes.len()
        );
    }
}
