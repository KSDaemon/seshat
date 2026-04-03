//! File discovery with `.gitignore` respect.
//!
//! Uses the [`ignore`] crate's [`WalkBuilder`] for native `.gitignore`
//! support and configurable exclusion patterns.

use std::collections::HashSet;
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

/// Result of the file discovery phase.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    /// The discovered source files.
    pub files: Vec<DiscoveredFile>,
    /// Submodule paths that were excluded from discovery.
    /// Root discovery always excludes submodule dirs (they get their own DBs).
    /// Empty when there is no `.gitmodules`.
    pub excluded_submodules: Vec<String>,
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
pub fn discover_files(root: &Path, config: &ScanConfig) -> Result<DiscoveryResult, ScanError> {
    let max_size_bytes = config.max_file_size_kb * 1024;

    // Root discovery ALWAYS excludes submodule directories — they get their own
    // separate DBs. The `exclude_submodules` config flag controls whether those
    // separate submodule scans happen at all, not whether the root walk includes them.
    let excluded_submodules = detect_submodule_paths(root);

    // Build a set of submodule directory names for the filter_entry closure.
    // We need to exclude these directories during the walk, not just report them.
    let submodule_dirs: HashSet<std::ffi::OsString> = excluded_submodules
        .iter()
        .filter_map(|p| {
            // Use the last component of the submodule path for directory matching.
            // For nested submodules like "libs/shared", we match on the full
            // relative path in the walker instead.
            Path::new(p).file_name().map(|n| n.to_os_string())
        })
        .collect();

    // Also keep full relative paths for nested submodules.
    let submodule_rel_paths: HashSet<PathBuf> =
        excluded_submodules.iter().map(PathBuf::from).collect();

    let root_for_closure = root.to_path_buf();

    let mut builder = WalkBuilder::new(root);
    builder
        // Native .gitignore support is on by default in WalkBuilder.
        .hidden(true) // skip hidden files/dirs
        .git_ignore(true) // respect .gitignore
        .git_global(true) // respect global gitignore
        .git_exclude(true) // respect .git/info/exclude
        .filter_entry(move |entry| {
            // Always skip .git directory itself
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if entry.file_name() == ".git" {
                    return false;
                }
                // Skip submodule directories when not included.
                if !submodule_dirs.is_empty() {
                    // Check by relative path (handles nested submodules).
                    if let Ok(rel) = entry.path().strip_prefix(&root_for_closure) {
                        if submodule_rel_paths.contains(rel) {
                            return false;
                        }
                    }
                    // Fallback: check by directory name (top-level submodules).
                    if submodule_dirs.contains(&entry.file_name().to_os_string()) {
                        if let Ok(rel) = entry.path().strip_prefix(&root_for_closure) {
                            if submodule_rel_paths.contains(rel) {
                                return false;
                            }
                        }
                    }
                }
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

    Ok(DiscoveryResult {
        files: discovered,
        excluded_submodules,
    })
}

/// Parse `.gitmodules` to extract submodule paths.
///
/// Returns a list of relative path strings from `path = ...` entries.
/// If `.gitmodules` doesn't exist or cannot be read, returns an empty vec.
pub fn detect_submodule_paths(root: &Path) -> Vec<String> {
    let gitmodules_path = root.join(".gitmodules");
    let content = match std::fs::read_to_string(&gitmodules_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut paths = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("path") {
            if let Some((_key, value)) = trimmed.split_once('=') {
                let path = value.trim().to_string();
                if !path.is_empty() {
                    paths.push(path);
                }
            }
        }
    }
    paths
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
        let result = discover_files(dir.path(), &config).unwrap();

        let mut names: Vec<String> = result
            .files
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
        let result = discover_files(dir.path(), &config).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].path.ends_with("src/main.rs"));
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
        let result = discover_files(dir.path(), &config).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].path.ends_with("src/main.rs"));
    }

    #[test]
    fn respects_custom_exclude_patterns() {
        let dir = setup_temp_project(&["src/main.rs", "src/generated.rs", "tests/test_main.rs"]);

        let config = ScanConfig {
            exclude_patterns: vec!["tests/**".to_string()],
            ..ScanConfig::default()
        };

        let result = discover_files(dir.path(), &config).unwrap();

        let mut names: Vec<String> = result
            .files
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

        let result = discover_files(dir.path(), &config).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].path.ends_with("src/small.rs"));
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
        let result = discover_files(dir.path(), &config).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].path.ends_with("src/main.rs"));
    }

    #[test]
    fn detected_language_matches_extension() {
        let dir = setup_temp_project(&[
            "a.rs", "b.ts", "c.tsx", "d.js", "e.jsx", "f.mjs", "g.cjs", "h.py",
        ]);

        let config = ScanConfig::default();
        let result = discover_files(dir.path(), &config).unwrap();

        for f in &result.files {
            let ext = f.path.extension().unwrap().to_str().unwrap();
            assert_eq!(
                f.language,
                Language::from_extension(ext).unwrap(),
                "Mismatch for extension {ext}"
            );
        }
        assert_eq!(result.files.len(), 8);
    }

    #[test]
    fn discovered_file_has_size() {
        let dir = setup_temp_project(&["src/main.rs"]);

        let config = ScanConfig::default();
        let result = discover_files(dir.path(), &config).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].size_bytes > 0);
    }

    #[test]
    fn empty_directory_returns_empty_vec() {
        let dir = tempfile::tempdir().expect("create temp dir");

        let config = ScanConfig::default();
        let result = discover_files(dir.path(), &config).unwrap();

        assert!(result.files.is_empty());
    }

    #[test]
    fn git_directory_always_excluded() {
        let dir = setup_temp_project(&["src/main.rs"]);

        // Create a .git dir with a Rust file inside (should be ignored)
        let git_dir = dir.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("hook.rs"), "// git hook").unwrap();

        let config = ScanConfig::default();
        let result = discover_files(dir.path(), &config).unwrap();

        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].path.ends_with("src/main.rs"));
    }

    // -- Submodule tests ---------------------------------------------------

    #[test]
    fn detect_submodule_paths_parses_gitmodules() {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::write(
            dir.path().join(".gitmodules"),
            "[submodule \"frontend\"]\n\tpath = frontend\n\turl = https://example.com/frontend.git\n\
             [submodule \"libs/shared\"]\n\tpath = libs/shared\n\turl = https://example.com/shared.git\n",
        )
        .unwrap();

        let paths = detect_submodule_paths(dir.path());
        assert_eq!(paths, vec!["frontend", "libs/shared"]);
    }

    #[test]
    fn detect_submodule_paths_no_gitmodules() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let paths = detect_submodule_paths(dir.path());
        assert!(paths.is_empty());
    }

    #[test]
    fn excluded_submodules_reported_when_gitmodules_present() {
        let dir = setup_temp_project(&["src/main.rs"]);
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".gitmodules"),
            "[submodule \"frontend\"]\n\tpath = frontend\n\turl = https://example.com/fe.git\n",
        )
        .unwrap();

        let config = ScanConfig::default(); // exclude_submodules = false
        let result = discover_files(dir.path(), &config).unwrap();

        // Root discovery always excludes submodule dirs (they get their own DBs).
        assert_eq!(result.excluded_submodules, vec!["frontend"]);
    }

    #[test]
    fn submodule_dirs_always_excluded_from_root_walk() {
        let dir = setup_temp_project(&["src/main.rs", "frontend/src/app.ts"]);
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".gitmodules"),
            "[submodule \"frontend\"]\n\tpath = frontend\n\turl = https://example.com/fe.git\n",
        )
        .unwrap();

        // Even with exclude_submodules = false (default), root discovery
        // excludes submodule dirs. They get their own separate scans.
        let config = ScanConfig::default();
        let result = discover_files(dir.path(), &config).unwrap();

        assert_eq!(result.excluded_submodules, vec!["frontend"]);
        // frontend/src/app.ts should NOT appear in discovered files.
        let file_names: Vec<String> = result
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(
            !file_names.contains(&"app.ts".to_string()),
            "submodule files should be excluded from root discovery"
        );
    }
}
