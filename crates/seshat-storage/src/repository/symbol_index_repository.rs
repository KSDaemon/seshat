//! SQLite implementation of [`SymbolIndexRepository`].
//!
//! Owns reads/writes for the `symbol_definitions` and `symbol_imports`
//! tables introduced by migration V13.  These tables turn the symbol-by-name
//! lookup powering `query_code_pattern` into an O(log N) SQL probe instead of
//! a full-IR scan.
//!
//! All writes for a single file go through [`replace_file`] so re-scans /
//! hot-tier updates are idempotent: existing rows for `(branch_id, file_path)`
//! are deleted before the new set is inserted, in one transaction.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};
use seshat_core::MAX_DEFINITION_SNIPPET_LINES;
use seshat_core::{
    BranchId, ProjectFile, export_definition_snippet, function_definition_snippet,
    truncate_snippet_to, type_definition_snippet,
};

use super::{SymbolIndexRepository, lock_conn};
use crate::StorageError;

/// One row in `symbol_definitions` — produced from a single
/// [`seshat_core::Function`] / [`seshat_core::TypeDef`] / [`seshat_core::Export`]
/// emitted by the IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDefinitionRow {
    pub symbol_name: String,
    pub file_path: String,
    pub line: u32,
    pub end_line: u32,
    pub kind: SymbolKind,
    pub is_public: bool,
    /// Truncated definition snippet (see
    /// [`seshat_core::MAX_DEFINITION_SNIPPET_LINES`]).
    pub snippet: String,
}

/// Kind discriminator stored in `symbol_definitions.kind`.
///
/// Mapped to the `CHECK (kind IN (...))` constraint in V13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Type,
    Export,
}

impl SymbolKind {
    /// SQL-side spelling used in the CHECK constraint and string columns.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Type => "type",
            Self::Export => "export",
        }
    }
}

/// One row in `symbol_imports`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolImportRow {
    pub imported_name: String,
    pub importer_file: String,
}

// ─── Extraction ─────────────────────────────────────────────────────────────

/// Extract every [`SymbolDefinitionRow`] that should be indexed for a parsed
/// file.  One row per [`seshat_core::Function`], [`seshat_core::TypeDef`], and
/// [`seshat_core::Export`].
///
/// The synthetic snippets come from
/// [`seshat_core::symbol_snippet`] so the writer here and the reader in
/// `seshat-graph` produce identical strings.
#[must_use]
pub fn extract_definitions(file: &ProjectFile) -> Vec<SymbolDefinitionRow> {
    let file_path = file.path.to_string_lossy().into_owned();
    let mut rows = Vec::with_capacity(file.functions.len() + file.types.len() + file.exports.len());

    for f in &file.functions {
        let snippet_raw = function_definition_snippet(f);
        rows.push(SymbolDefinitionRow {
            symbol_name: f.name.clone(),
            file_path: file_path.clone(),
            line: u32::try_from(f.line).unwrap_or(0),
            end_line: u32::try_from(f.end_line).unwrap_or(0),
            kind: SymbolKind::Function,
            is_public: f.is_public,
            snippet: truncate_snippet_to(&snippet_raw, MAX_DEFINITION_SNIPPET_LINES).content,
        });
    }

    for t in &file.types {
        let snippet_raw = type_definition_snippet(t);
        rows.push(SymbolDefinitionRow {
            symbol_name: t.name.clone(),
            file_path: file_path.clone(),
            line: u32::try_from(t.line).unwrap_or(0),
            end_line: u32::try_from(t.end_line).unwrap_or(0),
            kind: SymbolKind::Type,
            is_public: t.is_public,
            snippet: truncate_snippet_to(&snippet_raw, MAX_DEFINITION_SNIPPET_LINES).content,
        });
    }

    for e in &file.exports {
        let snippet_raw = export_definition_snippet(e);
        rows.push(SymbolDefinitionRow {
            symbol_name: e.name.clone(),
            file_path: file_path.clone(),
            line: u32::try_from(e.line).unwrap_or(0),
            end_line: u32::try_from(e.end_line).unwrap_or(0),
            kind: SymbolKind::Export,
            // exports are by definition reachable from outside the module.
            is_public: true,
            snippet: truncate_snippet_to(&snippet_raw, MAX_DEFINITION_SNIPPET_LINES).content,
        });
    }

    rows
}

/// Extract every [`SymbolImportRow`] that should be indexed for a parsed file.
///
/// Filters out:
/// - wildcard imports — any entry whose first non-whitespace token is `*`
///   (`"*"`, `"* as foo"`, `"*as foo"`, `" *  as  foo"`).  The TS/JS parsers
///   produce these for `import * as foo from '…'`; they tell us nothing
///   about which concrete symbols the file consumes.
/// - empty names defensively.
///
/// Across all four parsers (Rust / Python / TypeScript / JavaScript), aliased
/// imports already store the defining (rightmost) name in `names[]` rather than
/// the local alias — `use foo::Bar as Baz`, `from foo import Bar as Baz`, and
/// `import { Bar as Baz } from 'foo'` all yield `"Bar"`.  No alias-stripping
/// happens here; pass the IR through.
#[must_use]
pub fn extract_imports(file: &ProjectFile) -> Vec<SymbolImportRow> {
    let importer_file = file.path.to_string_lossy().into_owned();
    let mut rows = Vec::new();

    for import in &file.imports {
        for name in &import.names {
            let trimmed = name.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('*') {
                continue;
            }
            rows.push(SymbolImportRow {
                imported_name: name.clone(),
                importer_file: importer_file.clone(),
            });
        }
    }

    rows
}

// ─── Repository ─────────────────────────────────────────────────────────────

/// SQLite-backed symbol-index repository.
#[derive(Debug, Clone)]
pub struct SqliteSymbolIndexRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteSymbolIndexRepository {
    /// Create a new repository backed by the given connection.
    #[must_use]
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

impl SymbolIndexRepository for SqliteSymbolIndexRepository {
    fn replace_file(
        &self,
        branch_id: &BranchId,
        file_path: &str,
        definitions: &[SymbolDefinitionRow],
        imports: &[SymbolImportRow],
    ) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::QueryError(format!("begin symbol-index tx: {e}")))?;

        delete_definitions(&tx, &branch_id.0, file_path)?;
        delete_imports(&tx, &branch_id.0, file_path)?;
        insert_definitions(&tx, &branch_id.0, definitions)?;
        insert_imports(&tx, &branch_id.0, imports)?;

        tx.commit()
            .map_err(|e| StorageError::QueryError(format!("commit symbol-index tx: {e}")))?;
        Ok(())
    }

    fn delete_file(&self, branch_id: &BranchId, file_path: &str) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::QueryError(format!("begin symbol-index tx: {e}")))?;

        delete_definitions(&tx, &branch_id.0, file_path)?;
        delete_imports(&tx, &branch_id.0, file_path)?;

        tx.commit()
            .map_err(|e| StorageError::QueryError(format!("commit symbol-index tx: {e}")))?;
        Ok(())
    }

    fn delete_branch(&self, branch_id: &BranchId) -> Result<(), StorageError> {
        let conn = lock_conn(&self.conn)?;
        conn.execute(
            "DELETE FROM symbol_definitions WHERE branch_id = ?1",
            params![branch_id.0],
        )?;
        conn.execute(
            "DELETE FROM symbol_imports WHERE branch_id = ?1",
            params![branch_id.0],
        )?;
        Ok(())
    }

    fn count_definitions(&self, branch_id: &BranchId) -> Result<usize, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM symbol_definitions WHERE branch_id = ?1",
            params![branch_id.0],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    fn count_imports(&self, branch_id: &BranchId) -> Result<usize, StorageError> {
        let conn = lock_conn(&self.conn)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM symbol_imports WHERE branch_id = ?1",
            params![branch_id.0],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}

// ─── SQL helpers ────────────────────────────────────────────────────────────
//
// Visible to sibling repository modules (`pub(super)`) so the combined
// "upsert files_ir + symbol-index" writer in `file_ir_repository` can run all
// four statements inside one outer transaction without duplicating SQL.

pub(super) fn delete_definitions(
    tx: &rusqlite::Transaction<'_>,
    branch_id: &str,
    file_path: &str,
) -> Result<(), StorageError> {
    tx.execute(
        "DELETE FROM symbol_definitions WHERE branch_id = ?1 AND file_path = ?2",
        params![branch_id, file_path],
    )?;
    Ok(())
}

pub(super) fn delete_imports(
    tx: &rusqlite::Transaction<'_>,
    branch_id: &str,
    file_path: &str,
) -> Result<(), StorageError> {
    tx.execute(
        "DELETE FROM symbol_imports WHERE branch_id = ?1 AND importer_file = ?2",
        params![branch_id, file_path],
    )?;
    Ok(())
}

pub(super) fn insert_definitions(
    tx: &rusqlite::Transaction<'_>,
    branch_id: &str,
    rows: &[SymbolDefinitionRow],
) -> Result<(), StorageError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "INSERT INTO symbol_definitions
            (branch_id, symbol_name, file_path, line, end_line, kind, is_public, snippet)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for row in rows {
        stmt.execute(params![
            branch_id,
            row.symbol_name,
            row.file_path,
            row.line,
            row.end_line,
            row.kind.as_str(),
            i64::from(row.is_public),
            row.snippet,
        ])?;
    }
    Ok(())
}

pub(super) fn insert_imports(
    tx: &rusqlite::Transaction<'_>,
    branch_id: &str,
    rows: &[SymbolImportRow],
) -> Result<(), StorageError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "INSERT INTO symbol_imports (branch_id, imported_name, importer_file)
         VALUES (?1, ?2, ?3)",
    )?;
    for row in rows {
        stmt.execute(params![branch_id, row.imported_name, row.importer_file])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use seshat_core::test_helpers::make_project_file;
    use seshat_core::{Export, Function, Import, Language, TypeDef, TypeDefKind};

    fn test_repo() -> SqliteSymbolIndexRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqliteSymbolIndexRepository::new(db.connection().clone())
    }

    fn sample_file() -> ProjectFile {
        let mut file = make_project_file(Language::Rust);
        file.path = "src/lib.rs".into();
        file.functions = vec![Function {
            name: "do_thing".to_owned(),
            is_public: true,
            is_async: false,
            line: 10,
            end_line: 12,
            parameters: vec!["x".to_owned()],
            doc_comment: None,
        }];
        file.types = vec![TypeDef {
            name: "Widget".to_owned(),
            kind: TypeDefKind::Struct,
            is_public: false,
            line: 20,
            end_line: 25,
            doc_comment: None,
        }];
        file.exports = vec![Export {
            name: "ApiHandle".to_owned(),
            is_default: false,
            is_type_only: false,
            line: 30,
            end_line: 30,
        }];
        file.imports = vec![
            Import {
                module: "foo".to_owned(),
                names: vec!["Bar".to_owned(), "Baz".to_owned()],
                is_type_only: false,
                line: 1,
            },
            Import {
                module: "wild".to_owned(),
                names: vec!["*".to_owned()],
                is_type_only: false,
                line: 2,
            },
            Import {
                module: "ns".to_owned(),
                names: vec!["* as alias".to_owned()],
                is_type_only: false,
                line: 3,
            },
        ];
        file
    }

    #[test]
    fn extract_definitions_emits_one_row_per_symbol() {
        let file = sample_file();
        let rows = extract_definitions(&file);
        assert_eq!(rows.len(), 3);

        let func = rows
            .iter()
            .find(|r| r.kind == SymbolKind::Function)
            .unwrap();
        assert_eq!(func.symbol_name, "do_thing");
        assert_eq!(func.line, 10);
        assert_eq!(func.end_line, 12);
        assert!(func.is_public);
        assert_eq!(func.snippet, "pub fn do_thing(x)");

        let ty = rows.iter().find(|r| r.kind == SymbolKind::Type).unwrap();
        assert_eq!(ty.symbol_name, "Widget");
        assert!(!ty.is_public);
        assert_eq!(ty.snippet, "struct Widget");

        let exp = rows.iter().find(|r| r.kind == SymbolKind::Export).unwrap();
        assert_eq!(exp.symbol_name, "ApiHandle");
        assert!(exp.is_public, "exports are always treated as public");
        assert_eq!(exp.snippet, "export ApiHandle");
    }

    #[test]
    fn extract_imports_filters_wildcards() {
        let file = sample_file();
        let rows = extract_imports(&file);
        // Two concrete names from the first import; wildcard + namespace
        // wildcard skipped.
        assert_eq!(rows.len(), 2);
        let names: Vec<&str> = rows.iter().map(|r| r.imported_name.as_str()).collect();
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"Baz"));
    }

    #[test]
    fn replace_file_inserts_and_replaces_idempotently() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let file = sample_file();

        let defs = extract_definitions(&file);
        let imps = extract_imports(&file);

        repo.replace_file(&branch, "src/lib.rs", &defs, &imps)
            .unwrap();
        assert_eq!(repo.count_definitions(&branch).unwrap(), 3);
        assert_eq!(repo.count_imports(&branch).unwrap(), 2);

        // Re-run: counts must be stable (delete-then-insert).
        repo.replace_file(&branch, "src/lib.rs", &defs, &imps)
            .unwrap();
        assert_eq!(repo.count_definitions(&branch).unwrap(), 3);
        assert_eq!(repo.count_imports(&branch).unwrap(), 2);
    }

    #[test]
    fn replace_file_with_empty_rows_clears_existing() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let file = sample_file();
        let defs = extract_definitions(&file);
        let imps = extract_imports(&file);

        repo.replace_file(&branch, "src/lib.rs", &defs, &imps)
            .unwrap();
        repo.replace_file(&branch, "src/lib.rs", &[], &[]).unwrap();
        assert_eq!(repo.count_definitions(&branch).unwrap(), 0);
        assert_eq!(repo.count_imports(&branch).unwrap(), 0);
    }

    #[test]
    fn delete_file_removes_all_rows_for_that_file() {
        let repo = test_repo();
        let branch = BranchId::from("main");
        let file = sample_file();
        let defs = extract_definitions(&file);
        let imps = extract_imports(&file);

        repo.replace_file(&branch, "src/lib.rs", &defs, &imps)
            .unwrap();
        repo.delete_file(&branch, "src/lib.rs").unwrap();
        assert_eq!(repo.count_definitions(&branch).unwrap(), 0);
        assert_eq!(repo.count_imports(&branch).unwrap(), 0);
    }

    #[test]
    fn delete_branch_clears_only_target_branch() {
        let repo = test_repo();
        let branch_a = BranchId::from("a");
        let branch_b = BranchId::from("b");
        let file = sample_file();
        let defs = extract_definitions(&file);
        let imps = extract_imports(&file);

        repo.replace_file(&branch_a, "src/lib.rs", &defs, &imps)
            .unwrap();
        repo.replace_file(&branch_b, "src/lib.rs", &defs, &imps)
            .unwrap();
        repo.delete_branch(&branch_a).unwrap();

        assert_eq!(repo.count_definitions(&branch_a).unwrap(), 0);
        assert_eq!(repo.count_definitions(&branch_b).unwrap(), 3);
    }

    #[test]
    fn check_constraint_rejects_unknown_kind() {
        // Defensive: the `kind` CHECK constraint must reject anything outside
        // the enum so a future bug that forgets to update SymbolKind::as_str()
        // fails fast.
        let db = Database::open(":memory:").unwrap();
        let conn = db.connection().lock().unwrap();
        let err = conn.execute(
            "INSERT INTO symbol_definitions
                (branch_id, symbol_name, file_path, line, end_line, kind, is_public, snippet)
             VALUES ('main','foo','f.rs',1,1,'BOGUS',1,'snip')",
            [],
        );
        assert!(err.is_err(), "CHECK constraint must reject unknown kinds");
    }
}
