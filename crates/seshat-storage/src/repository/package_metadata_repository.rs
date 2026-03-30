//! SQLite implementation of [`PackageMetadataRepository`].

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use super::PackageMetadataRepository;
use crate::StorageError;

/// A row from the `package_metadata` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMetadataRow {
    /// Package name.
    pub name: String,
    /// Registry identifier (e.g., `"crates_io"`, `"npm"`, `"pypi"`).
    pub registry: String,
    /// JSON array of category strings.
    pub categories: Vec<String>,
    /// JSON array of keyword strings.
    pub keywords: Vec<String>,
    /// Package description, if available.
    pub description: Option<String>,
    /// Unix timestamp when metadata was fetched.
    pub fetched_at: i64,
}

/// SQLite-backed package metadata repository.
#[derive(Debug, Clone)]
pub struct SqlitePackageMetadataRepository {
    conn: Arc<Mutex<Connection>>,
}

impl SqlitePackageMetadataRepository {
    /// Create a new repository backed by the given connection.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.conn.lock().map_err(|e| {
            StorageError::QueryError(format!("Failed to acquire connection lock: {e}"))
        })
    }
}

impl PackageMetadataRepository for SqlitePackageMetadataRepository {
    #[tracing::instrument(skip(self))]
    fn upsert(&self, row: &PackageMetadataRow) -> Result<(), StorageError> {
        let conn = self.conn()?;

        let categories_json = serde_json::to_string(&row.categories)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;
        let keywords_json = serde_json::to_string(&row.keywords)
            .map_err(|e| StorageError::SerializationError(e.to_string()))?;

        conn.execute(
            "INSERT INTO package_metadata (name, registry, categories, keywords, description, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(name, registry) DO UPDATE SET
               categories = excluded.categories,
               keywords   = excluded.keywords,
               description = excluded.description,
               fetched_at  = excluded.fetched_at",
            params![
                row.name,
                row.registry,
                categories_json,
                keywords_json,
                row.description,
                row.fetched_at,
            ],
        )?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn get(&self, name: &str, registry: &str) -> Result<Option<PackageMetadataRow>, StorageError> {
        let conn = self.conn()?;

        let result = conn.query_row(
            "SELECT name, registry, categories, keywords, description, fetched_at
             FROM package_metadata WHERE name = ?1 AND registry = ?2",
            params![name, registry],
            row_to_package_metadata,
        );

        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StorageError::from(e)),
        }
    }

    #[tracing::instrument(skip(self))]
    fn get_by_registry(&self, registry: &str) -> Result<Vec<PackageMetadataRow>, StorageError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT name, registry, categories, keywords, description, fetched_at
             FROM package_metadata WHERE registry = ?1",
        )?;
        let rows = stmt.query_map([registry], row_to_package_metadata)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[tracing::instrument(skip(self))]
    fn delete_stale(&self, before_timestamp: i64) -> Result<usize, StorageError> {
        let conn = self.conn()?;
        let affected = conn.execute(
            "DELETE FROM package_metadata WHERE fetched_at < ?1",
            params![before_timestamp],
        )?;
        Ok(affected)
    }
}

/// Map a rusqlite `Row` to a [`PackageMetadataRow`].
fn row_to_package_metadata(row: &rusqlite::Row<'_>) -> rusqlite::Result<PackageMetadataRow> {
    let name: String = row.get(0)?;
    let registry: String = row.get(1)?;
    let categories_json: String = row.get(2)?;
    let keywords_json: String = row.get(3)?;
    let description: Option<String> = row.get(4)?;
    let fetched_at: i64 = row.get(5)?;

    let categories: Vec<String> = serde_json::from_str(&categories_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let keywords: Vec<String> = serde_json::from_str(&keywords_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(PackageMetadataRow {
        name,
        registry,
        categories,
        keywords,
        description,
        fetched_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    /// Helper: create an in-memory DB and return a `SqlitePackageMetadataRepository`.
    fn test_repo() -> SqlitePackageMetadataRepository {
        let db = Database::open(":memory:").expect("in-memory DB");
        SqlitePackageMetadataRepository::new(db.connection().clone())
    }

    fn make_row(name: &str, registry: &str) -> PackageMetadataRow {
        PackageMetadataRow {
            name: name.to_string(),
            registry: registry.to_string(),
            categories: vec!["web".to_string(), "http".to_string()],
            keywords: vec!["async".to_string(), "server".to_string()],
            description: Some("A web framework".to_string()),
            fetched_at: 1_700_000_000,
        }
    }

    #[test]
    fn upsert_and_get() {
        let repo = test_repo();
        let row = make_row("actix-web", "crates_io");

        repo.upsert(&row).expect("upsert should succeed");

        let fetched = repo
            .get("actix-web", "crates_io")
            .expect("get should succeed")
            .expect("row should exist");

        assert_eq!(fetched.name, "actix-web");
        assert_eq!(fetched.registry, "crates_io");
        assert_eq!(fetched.categories, vec!["web", "http"]);
        assert_eq!(fetched.keywords, vec!["async", "server"]);
        assert_eq!(fetched.description, Some("A web framework".to_string()));
        assert_eq!(fetched.fetched_at, 1_700_000_000);
    }

    #[test]
    fn upsert_updates_existing() {
        let repo = test_repo();
        let mut row = make_row("serde", "crates_io");
        repo.upsert(&row).expect("first upsert");

        // Update with new data
        row.categories = vec!["serialization".to_string()];
        row.keywords = vec!["json".to_string(), "serde".to_string()];
        row.description = Some("Serialization framework".to_string());
        row.fetched_at = 1_700_100_000;
        repo.upsert(&row).expect("second upsert");

        let fetched = repo
            .get("serde", "crates_io")
            .expect("get should succeed")
            .expect("row should exist");

        assert_eq!(fetched.categories, vec!["serialization"]);
        assert_eq!(fetched.keywords, vec!["json", "serde"]);
        assert_eq!(
            fetched.description,
            Some("Serialization framework".to_string())
        );
        assert_eq!(fetched.fetched_at, 1_700_100_000);
    }

    #[test]
    fn get_not_found() {
        let repo = test_repo();

        let result = repo
            .get("nonexistent", "crates_io")
            .expect("get should not error");

        assert!(result.is_none());
    }

    #[test]
    fn get_by_registry() {
        let repo = test_repo();

        repo.upsert(&make_row("serde", "crates_io")).unwrap();
        repo.upsert(&make_row("tokio", "crates_io")).unwrap();
        repo.upsert(&make_row("express", "npm")).unwrap();

        let crates = repo
            .get_by_registry("crates_io")
            .expect("query should succeed");
        assert_eq!(crates.len(), 2);

        let npm = repo.get_by_registry("npm").expect("query should succeed");
        assert_eq!(npm.len(), 1);
        assert_eq!(npm[0].name, "express");

        let pypi = repo.get_by_registry("pypi").expect("query should succeed");
        assert!(pypi.is_empty());
    }

    #[test]
    fn delete_stale() {
        let repo = test_repo();

        let mut old = make_row("old-pkg", "crates_io");
        old.fetched_at = 1_000_000;
        repo.upsert(&old).unwrap();

        let mut recent = make_row("new-pkg", "crates_io");
        recent.fetched_at = 2_000_000;
        repo.upsert(&recent).unwrap();

        let deleted = repo.delete_stale(1_500_000).expect("delete should succeed");
        assert_eq!(deleted, 1);

        assert!(repo.get("old-pkg", "crates_io").unwrap().is_none());
        assert!(repo.get("new-pkg", "crates_io").unwrap().is_some());
    }

    #[test]
    fn empty_categories_and_keywords() {
        let repo = test_repo();

        let row = PackageMetadataRow {
            name: "minimal".to_string(),
            registry: "npm".to_string(),
            categories: vec![],
            keywords: vec![],
            description: None,
            fetched_at: 1_700_000_000,
        };

        repo.upsert(&row).expect("upsert should succeed");

        let fetched = repo
            .get("minimal", "npm")
            .expect("get should succeed")
            .expect("row should exist");

        assert!(fetched.categories.is_empty());
        assert!(fetched.keywords.is_empty());
        assert!(fetched.description.is_none());
    }

    #[test]
    fn same_name_different_registry() {
        let repo = test_repo();

        let crate_row = PackageMetadataRow {
            name: "requests".to_string(),
            registry: "crates_io".to_string(),
            categories: vec!["http".to_string()],
            keywords: vec!["http".to_string()],
            description: Some("Rust HTTP".to_string()),
            fetched_at: 1_700_000_000,
        };

        let pypi_row = PackageMetadataRow {
            name: "requests".to_string(),
            registry: "pypi".to_string(),
            categories: vec!["internet".to_string()],
            keywords: vec!["http".to_string(), "python".to_string()],
            description: Some("Python HTTP".to_string()),
            fetched_at: 1_700_000_000,
        };

        repo.upsert(&crate_row).unwrap();
        repo.upsert(&pypi_row).unwrap();

        let crate_fetched = repo.get("requests", "crates_io").unwrap().unwrap();
        let pypi_fetched = repo.get("requests", "pypi").unwrap().unwrap();

        assert_eq!(crate_fetched.categories, vec!["http"]);
        assert_eq!(pypi_fetched.categories, vec!["internet"]);
        assert_eq!(crate_fetched.description, Some("Rust HTTP".to_string()));
        assert_eq!(pypi_fetched.description, Some("Python HTTP".to_string()));
    }
}
