//! Automatic database backup logic.
//!
//! Creates timestamped copies of the SQLite database file and manages
//! retention (deleting old backups beyond a configured count).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use seshat_core::BackupConfig;
use tracing;

use crate::StorageError;

/// Suffix format for backup files: `.seshat.db.YYYY-MM-DD` style.
/// We use the database filename as a base and append a timestamp.
fn backup_filename(db_path: &Path, timestamp: SystemTime) -> PathBuf {
    let datetime = format_timestamp(timestamp);
    let file_name = db_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let backup_name = format!("{file_name}.{datetime}");
    db_path.with_file_name(backup_name)
}

/// Format a SystemTime as `YYYY-MM-DD` for backup file suffixes.
fn format_timestamp(t: SystemTime) -> String {
    let duration = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();

    // Simple date calculation (no external chrono dependency needed).
    let days = secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm adapted from Howard Hinnant's civil_from_days.
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// List existing backup files for the given database path, sorted by name
/// (oldest first due to date format).
fn list_backups(db_path: &Path) -> Vec<PathBuf> {
    let parent = match db_path.parent() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let file_name = db_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Backup files match pattern: <db_filename>.<YYYY-MM-DD>
    let prefix = format!("{file_name}.");

    let mut backups: Vec<PathBuf> = fs::read_dir(parent)
        .unwrap_or_else(|_| fs::read_dir(".").unwrap()) // fallback should never happen
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) {
                return false;
            }
            let suffix = &name[prefix.len()..];
            // Validate date format: YYYY-MM-DD (10 chars, digits and dashes)
            suffix.len() == 10
                && suffix.chars().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        c == '-'
                    } else {
                        c.is_ascii_digit()
                    }
                })
        })
        .map(|entry| entry.path())
        .collect();

    backups.sort();
    backups
}

/// Checks the last backup time by examining existing backup files.
/// Returns `None` if no backups exist.
fn last_backup_time(db_path: &Path) -> Option<SystemTime> {
    let backups = list_backups(db_path);
    backups.last().and_then(|path| {
        // Extract the date suffix and convert back to SystemTime
        let name = path.file_name()?.to_string_lossy().to_string();
        let date_part = &name[name.len() - 10..];
        parse_backup_date(date_part)
    })
}

/// Parse a `YYYY-MM-DD` string back to a SystemTime (at midnight UTC).
fn parse_backup_date(date: &str) -> Option<SystemTime> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: u64 = parts[0].parse().ok()?;
    let month: u64 = parts[1].parse().ok()?;
    let day: u64 = parts[2].parse().ok()?;

    let days = ymd_to_days(year, month, day);
    let secs = days * 86400;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

/// Convert (year, month, day) to days since Unix epoch.
fn ymd_to_days(year: u64, month: u64, day: u64) -> u64 {
    // Inverse of days_to_ymd — Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Creates a backup of the database file if one is needed based on
/// the configured interval. Manages retention by deleting old backups.
///
/// # Arguments
/// - `db_path`: path to the SQLite database file
/// - `config`: backup configuration
///
/// # Returns
/// - `Ok(true)` if a backup was created
/// - `Ok(false)` if no backup was needed (interval not elapsed or disabled)
/// - `Err(StorageError)` if the backup operation failed
pub fn backup_if_needed(db_path: &Path, config: &BackupConfig) -> Result<bool, StorageError> {
    // Skip if backups are disabled.
    if !config.enabled {
        tracing::debug!("Database backups are disabled");
        return Ok(false);
    }

    // Skip for in-memory databases.
    let path_str = db_path.to_string_lossy();
    if path_str == ":memory:" || path_str.is_empty() {
        tracing::debug!("Skipping backup for in-memory database");
        return Ok(false);
    }

    // Check if the database file exists.
    if !db_path.exists() {
        tracing::warn!("Database file does not exist: {}", db_path.display());
        return Ok(false);
    }

    // Check if enough time has passed since the last backup.
    let interval = Duration::from_secs(config.interval_hours * 3600);
    if let Some(last_time) = last_backup_time(db_path) {
        let now = SystemTime::now();
        if let Ok(elapsed) = now.duration_since(last_time) {
            if elapsed < interval {
                tracing::debug!(
                    "Backup not needed: last backup was {:.1} hours ago (interval: {} hours)",
                    elapsed.as_secs_f64() / 3600.0,
                    config.interval_hours
                );
                return Ok(false);
            }
        }
    }

    // Create the backup.
    let now = SystemTime::now();
    let backup_path = backup_filename(db_path, now);

    tracing::info!(
        "Creating database backup: {} -> {}",
        db_path.display(),
        backup_path.display()
    );

    fs::copy(db_path, &backup_path).map_err(StorageError::Io)?;

    tracing::info!("Backup created: {}", backup_path.display());

    // Enforce retention: delete old backups beyond retention_count.
    cleanup_old_backups(db_path, config.retention_count)?;

    Ok(true)
}

/// Deletes old backup files beyond the retention count (keeps newest).
fn cleanup_old_backups(db_path: &Path, retention_count: usize) -> Result<(), StorageError> {
    let backups = list_backups(db_path);

    if backups.len() > retention_count {
        let to_delete = backups.len() - retention_count;
        for backup in backups.iter().take(to_delete) {
            tracing::info!("Deleting old backup: {}", backup.display());
            fs::remove_file(backup)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// Helper: create a temp directory for tests.
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("seshat_backup_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Helper: clean up temp directory.
    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    /// Helper: create a fake DB file with some content.
    fn create_fake_db(path: &Path) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(b"fake database content for testing").unwrap();
    }

    // ── Date conversion tests ────────────────────────────────────

    #[test]
    fn format_timestamp_produces_valid_date() {
        // 2026-03-26 is about 20538 days since epoch
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(20538 * 86400);
        let result = format_timestamp(t);
        assert_eq!(result, "2026-03-26");
    }

    #[test]
    fn format_timestamp_unix_epoch() {
        let result = format_timestamp(SystemTime::UNIX_EPOCH);
        assert_eq!(result, "1970-01-01");
    }

    #[test]
    fn ymd_roundtrip() {
        // Test several dates for roundtrip correctness
        let test_dates = [
            (1970, 1, 1),
            (2000, 1, 1),
            (2024, 2, 29), // leap year
            (2026, 3, 26),
            (2026, 12, 31),
        ];
        for (y, m, d) in test_dates {
            let days = ymd_to_days(y, m, d);
            let (y2, m2, d2) = days_to_ymd(days);
            assert_eq!((y, m, d), (y2, m2, d2), "roundtrip failed for {y}-{m}-{d}");
        }
    }

    #[test]
    fn parse_backup_date_valid() {
        let t = parse_backup_date("2026-03-26").unwrap();
        let expected =
            SystemTime::UNIX_EPOCH + Duration::from_secs(ymd_to_days(2026, 3, 26) * 86400);
        assert_eq!(t, expected);
    }

    #[test]
    fn parse_backup_date_invalid() {
        assert!(parse_backup_date("not-a-date").is_none());
        assert!(parse_backup_date("2026-13-01").is_some()); // We don't validate ranges, just parsing
        assert!(parse_backup_date("20260326").is_none()); // No dashes
    }

    // ── Backup filename tests ────────────────────────────────────

    #[test]
    fn backup_filename_format() {
        let db = Path::new("/data/seshat.db");
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(20538 * 86400);
        let result = backup_filename(db, t);
        assert_eq!(result, PathBuf::from("/data/seshat.db.2026-03-26"));
    }

    // ── list_backups tests ───────────────────────────────────────

    #[test]
    fn list_backups_finds_matching_files() {
        let dir = temp_dir("list_backups");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        // Create some backup files
        fs::write(dir.join("test.db.2026-03-24"), b"backup1").unwrap();
        fs::write(dir.join("test.db.2026-03-25"), b"backup2").unwrap();
        fs::write(dir.join("test.db.2026-03-26"), b"backup3").unwrap();

        // Create a non-backup file that shouldn't match
        fs::write(dir.join("test.db.wal"), b"wal file").unwrap();
        fs::write(dir.join("other.db.2026-03-26"), b"other backup").unwrap();

        let backups = list_backups(&db_path);
        assert_eq!(backups.len(), 3);
        assert!(backups[0].ends_with("test.db.2026-03-24"));
        assert!(backups[1].ends_with("test.db.2026-03-25"));
        assert!(backups[2].ends_with("test.db.2026-03-26"));

        cleanup(&dir);
    }

    #[test]
    fn list_backups_empty_when_no_backups() {
        let dir = temp_dir("list_backups_empty");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        let backups = list_backups(&db_path);
        assert!(backups.is_empty());

        cleanup(&dir);
    }

    // ── backup_if_needed tests ───────────────────────────────────

    #[test]
    fn backup_disabled_skips() {
        let dir = temp_dir("backup_disabled");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        let config = BackupConfig {
            enabled: false,
            ..Default::default()
        };

        let result = backup_if_needed(&db_path, &config).unwrap();
        assert!(!result, "should not create backup when disabled");
        assert!(list_backups(&db_path).is_empty());

        cleanup(&dir);
    }

    #[test]
    fn backup_in_memory_skips() {
        let config = BackupConfig::default();
        let result = backup_if_needed(Path::new(":memory:"), &config).unwrap();
        assert!(!result, "should not create backup for in-memory DB");
    }

    #[test]
    fn backup_nonexistent_db_skips() {
        let config = BackupConfig::default();
        let result = backup_if_needed(Path::new("/nonexistent/path/db.sqlite"), &config).unwrap();
        assert!(!result, "should not create backup for nonexistent DB");
    }

    #[test]
    fn backup_creates_file_on_first_run() {
        let dir = temp_dir("backup_first_run");
        let db_path = dir.join("seshat.db");
        create_fake_db(&db_path);

        let config = BackupConfig {
            enabled: true,
            retention_count: 3,
            interval_hours: 24,
        };

        let result = backup_if_needed(&db_path, &config).unwrap();
        assert!(result, "should create backup on first run");

        let backups = list_backups(&db_path);
        assert_eq!(backups.len(), 1, "should have exactly one backup");

        // Verify backup content matches original
        let original = fs::read(&db_path).unwrap();
        let backup_content = fs::read(&backups[0]).unwrap();
        assert_eq!(
            original, backup_content,
            "backup content should match original"
        );

        cleanup(&dir);
    }

    #[test]
    fn backup_skips_when_interval_not_elapsed() {
        let dir = temp_dir("backup_interval");
        let db_path = dir.join("seshat.db");
        create_fake_db(&db_path);

        let config = BackupConfig {
            enabled: true,
            retention_count: 3,
            interval_hours: 24,
        };

        // First backup should succeed
        let result = backup_if_needed(&db_path, &config).unwrap();
        assert!(result);

        // Second immediate backup should be skipped (interval not elapsed)
        let result = backup_if_needed(&db_path, &config).unwrap();
        assert!(!result, "should skip backup when interval not elapsed");

        let backups = list_backups(&db_path);
        assert_eq!(backups.len(), 1, "should still have only one backup");

        cleanup(&dir);
    }

    #[test]
    fn backup_with_zero_interval_always_creates() {
        let dir = temp_dir("backup_zero_interval");
        let db_path = dir.join("seshat.db");
        create_fake_db(&db_path);

        let config = BackupConfig {
            enabled: true,
            retention_count: 5,
            interval_hours: 0, // zero interval means always backup
        };

        // First backup
        let result = backup_if_needed(&db_path, &config).unwrap();
        assert!(result);

        // With interval_hours=0 the interval is 0 seconds, so elapsed >= interval.
        // However, both backups have the same date suffix, so the second
        // overwrites the first (same filename). This is expected behavior.
        let result = backup_if_needed(&db_path, &config).unwrap();
        assert!(result, "should create backup with zero interval");

        cleanup(&dir);
    }

    // ── Retention / cleanup tests ────────────────────────────────

    #[test]
    fn cleanup_deletes_old_backups_beyond_retention() {
        let dir = temp_dir("cleanup_retention");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        // Create 5 backup files
        fs::write(dir.join("test.db.2026-03-20"), b"backup1").unwrap();
        fs::write(dir.join("test.db.2026-03-21"), b"backup2").unwrap();
        fs::write(dir.join("test.db.2026-03-22"), b"backup3").unwrap();
        fs::write(dir.join("test.db.2026-03-23"), b"backup4").unwrap();
        fs::write(dir.join("test.db.2026-03-24"), b"backup5").unwrap();

        // Retain only 3
        cleanup_old_backups(&db_path, 3).unwrap();

        let remaining = list_backups(&db_path);
        assert_eq!(remaining.len(), 3);
        // Should keep the newest 3
        assert!(remaining[0].ends_with("test.db.2026-03-22"));
        assert!(remaining[1].ends_with("test.db.2026-03-23"));
        assert!(remaining[2].ends_with("test.db.2026-03-24"));

        cleanup(&dir);
    }

    #[test]
    fn cleanup_does_nothing_when_within_retention() {
        let dir = temp_dir("cleanup_within");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        // Create 2 backup files (retention is 3)
        fs::write(dir.join("test.db.2026-03-25"), b"backup1").unwrap();
        fs::write(dir.join("test.db.2026-03-26"), b"backup2").unwrap();

        cleanup_old_backups(&db_path, 3).unwrap();

        let remaining = list_backups(&db_path);
        assert_eq!(remaining.len(), 2, "should not delete any backups");

        cleanup(&dir);
    }

    #[test]
    fn cleanup_with_retention_zero_deletes_all() {
        let dir = temp_dir("cleanup_zero");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        fs::write(dir.join("test.db.2026-03-25"), b"backup1").unwrap();
        fs::write(dir.join("test.db.2026-03-26"), b"backup2").unwrap();

        cleanup_old_backups(&db_path, 0).unwrap();

        let remaining = list_backups(&db_path);
        assert!(
            remaining.is_empty(),
            "should delete all backups with retention 0"
        );

        cleanup(&dir);
    }

    // ── Integration-style test ───────────────────────────────────

    #[test]
    fn full_backup_lifecycle() {
        let dir = temp_dir("lifecycle");
        let db_path = dir.join("seshat.db");
        create_fake_db(&db_path);

        let config = BackupConfig {
            enabled: true,
            retention_count: 2,
            interval_hours: 0, // force backup every time
        };

        // Create a backup
        let created = backup_if_needed(&db_path, &config).unwrap();
        assert!(created);

        // Simulate older backups by manually creating them
        fs::write(dir.join("seshat.db.2020-01-01"), b"old1").unwrap();
        fs::write(dir.join("seshat.db.2020-01-02"), b"old2").unwrap();
        fs::write(dir.join("seshat.db.2020-01-03"), b"old3").unwrap();

        // Run backup again — should trigger cleanup
        let created = backup_if_needed(&db_path, &config).unwrap();
        assert!(created);

        // Check retention: should have at most 2 backups
        let backups = list_backups(&db_path);
        assert!(
            backups.len() <= 2,
            "should retain at most 2 backups, found {}",
            backups.len()
        );

        cleanup(&dir);
    }

    // ── last_backup_time tests ───────────────────────────────────

    #[test]
    fn last_backup_time_returns_none_when_no_backups() {
        let dir = temp_dir("last_time_none");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        assert!(last_backup_time(&db_path).is_none());

        cleanup(&dir);
    }

    #[test]
    fn last_backup_time_returns_newest() {
        let dir = temp_dir("last_time_newest");
        let db_path = dir.join("test.db");
        create_fake_db(&db_path);

        fs::write(dir.join("test.db.2026-03-24"), b"backup1").unwrap();
        fs::write(dir.join("test.db.2026-03-26"), b"backup2").unwrap();
        fs::write(dir.join("test.db.2026-03-25"), b"backup3").unwrap();

        let last = last_backup_time(&db_path).unwrap();
        let expected = parse_backup_date("2026-03-26").unwrap();
        assert_eq!(last, expected, "should return the most recent backup date");

        cleanup(&dir);
    }
}
