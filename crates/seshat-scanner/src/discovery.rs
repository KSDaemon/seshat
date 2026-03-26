//! File discovery with `.gitignore` respect.
//!
//! Uses the [`ignore`] crate's [`WalkBuilder`] for native `.gitignore`
//! support and configurable exclusion patterns.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use seshat_core::{Language, ScanConfig};

use crate::ScanError;

/// A discovered source file ready for parsing.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Absolute or root-relative path to the file.
    pub path: PathBuf,
    /// Detected programming language based on file extension.
    pub language: Language,
    /// File size in bytes.
    pub size_bytes: u64,
}

/// Discover all recognised source files under `root`, respecting `.gitignore`,
/// hidden-file conventions, and the supplied [`ScanConfig`].
///
/// # Behaviour
///
/// - Uses [`WalkBuilder`] for native `.gitignore` support (including nested
///   `.gitignore` files).
/// - `.git/` directory is always excluded.
/// - Hidden files and directories (starting with `.`) are excluded by default.
/// - Custom exclude patterns from [`ScanConfig::exclude_patterns`] are applied
///   as additional override globs.
/// - Files exceeding [`ScanConfig::max_file_size_kb`] are skipped with a
///   [`tracing::warn`].
/// - Files with unrecognised extensions are silently skipped.
///
/// # Errors
///
/// Returns [`ScanError::DiscoveryError`] when the walker itself fails to
/// initialise or encounters a fatal filesystem error.
pub fn discover_files(root: &Path, config: &ScanConfig) -> Result<Vec<DiscoveredFile>, ScanError> {
    let max_size_bytes = config.max_file_size_kb * 1024;

    let mut builder = WalkBuilder::new(root);
    builder
        // Native .gitignore support is on by default in WalkBuilder.
        .hidden(true) // skip hidden files/dirs
        .git_ignore(true) // respect .gitignore
        .git_global(true) // respect global gitignore
        .git_exclude(true) // respect .git/info/exclude
        .filter_entry(|entry| {
            // Always skip .git directory itself
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                return entry.file_name() != ".git";
            }
            true
        });

    // Apply custom exclude patterns as overrides.
    // The ignore crate's overrides act like a `.gitignore` on top of everything.
    if !config.exclude_patterns.is_empty() {
        let mut overrides = ignore::overrides::OverrideBuilder::new(root);
        for pattern in &config.exclude_patterns {
            // Negate the pattern so matching entries are *excluded*.
            let negated = format!("!{pattern}");
            overrides
                .add(&negated)
                .map_err(|e| ScanError::DiscoveryError {
                    path: root.to_path_buf(),
                    reason: format!("Invalid exclude pattern '{pattern}': {e}"),
                })?;
        }
        let built = overrides.build().map_err(|e| ScanError::DiscoveryError {
            path: root.to_path_buf(),
            reason: format!("Failed to build override globs: {e}"),
        })?;
        builder.overrides(built);
    }

    let mut discovered = Vec::new();

    for entry_result in builder.build() {
        let entry = match entry_result {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("File walk error: {err}");
                continue;
            }
        };

        // Only process regular files.
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();

        // Detect language from extension; skip unrecognised files.
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Some(language) = Language::from_extension(ext) else {
            continue;
        };

        // Check file size.
        let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size_bytes > max_size_bytes {
            tracing::warn!(
                path = %path.display(),
                size_kb = size_bytes / 1024,
                limit_kb = config.max_file_size_kb,
                "Skipping file exceeding size limit"
            );
            continue;
        }

        discovered.push(DiscoveredFile {
            path: path.to_path_buf(),
            language,
            size_bytes,
        });
    }

    Ok(discovered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temporary project directory with the given file structure.
    fn setup_temp_project(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        for file in files {
            let path = dir.path().join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent dirs");
            }
            fs::write(&path, "// placeholder").expect("write file");
        }
        dir
    }

    #[test]
    fn discovers_recognised_extensions() {
        let dir = setup_temp_project(&[
            "src/main.rs",
            "src/lib.ts",
            "app/index.js",
            "scripts/run.py",
            "README.md",        // not recognised
            "data/config.yaml", // not recognised
        ]);

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        let mut names: Vec<String> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        names.sort();

        assert_eq!(names, vec!["index.js", "lib.ts", "main.rs", "run.py"]);
    }

    #[test]
    fn skips_hidden_files_and_directories() {
        let dir = setup_temp_project(&["src/main.rs", ".hidden/secret.rs", "src/.hidden_file.py"]);

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("src/main.rs"));
    }

    #[test]
    fn respects_gitignore() {
        let dir = setup_temp_project(&[
            "src/main.rs",
            "target/debug/build.rs",
            "node_modules/pkg/index.js",
        ]);

        // Create a .gitignore that excludes target/ and node_modules/
        fs::write(dir.path().join(".gitignore"), "target/\nnode_modules/\n").unwrap();

        // WalkBuilder needs a git repo to respect .gitignore
        fs::create_dir(dir.path().join(".git")).unwrap();

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("src/main.rs"));
    }

    #[test]
    fn respects_custom_exclude_patterns() {
        let dir = setup_temp_project(&["src/main.rs", "src/generated.rs", "tests/test_main.rs"]);

        let config = ScanConfig {
            exclude_patterns: vec!["tests/**".to_string()],
            ..ScanConfig::default()
        };

        let files = discover_files(dir.path(), &config).unwrap();

        let mut names: Vec<String> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        names.sort();

        assert_eq!(names, vec!["generated.rs", "main.rs"]);
    }

    #[test]
    fn skips_files_exceeding_size_limit() {
        let dir = setup_temp_project(&["src/small.rs"]);

        // Create a file that exceeds 1 KB limit
        let big_file = dir.path().join("src/big.rs");
        let big_content = "x".repeat(2048); // 2 KB
        fs::write(&big_file, big_content).unwrap();

        let config = ScanConfig {
            max_file_size_kb: 1,
            ..ScanConfig::default()
        };

        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("src/small.rs"));
    }

    #[test]
    fn skips_unrecognised_extensions() {
        let dir = setup_temp_project(&[
            "src/main.rs",
            "src/style.css",
            "src/page.html",
            "src/data.json",
        ]);

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("src/main.rs"));
    }

    #[test]
    fn detected_language_matches_extension() {
        let dir = setup_temp_project(&[
            "a.rs", "b.ts", "c.tsx", "d.js", "e.jsx", "f.mjs", "g.cjs", "h.py",
        ]);

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        for f in &files {
            let ext = f.path.extension().unwrap().to_str().unwrap();
            assert_eq!(
                f.language,
                Language::from_extension(ext).unwrap(),
                "Mismatch for extension {ext}"
            );
        }
        assert_eq!(files.len(), 8);
    }

    #[test]
    fn discovered_file_has_size() {
        let dir = setup_temp_project(&["src/main.rs"]);

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].size_bytes > 0);
    }

    #[test]
    fn empty_directory_returns_empty_vec() {
        let dir = tempfile::tempdir().expect("create temp dir");

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        assert!(files.is_empty());
    }

    #[test]
    fn git_directory_always_excluded() {
        let dir = setup_temp_project(&["src/main.rs"]);

        // Create a .git dir with a Rust file inside (should be ignored)
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("hook.rs"), "// git hook").unwrap();

        let config = ScanConfig::default();
        let files = discover_files(dir.path(), &config).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("src/main.rs"));
    }
}
