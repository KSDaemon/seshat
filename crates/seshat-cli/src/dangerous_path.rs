//! Dangerous-path denylist for `seshat serve`.
//!
//! When `serve` is invoked from a denylisted directory and there is no git
//! repository nearby, it would otherwise try to scan and recursively watch a
//! huge tree (`$HOME`, `/`, …), which `notify-debouncer-full` translates into
//! tens of GB of memory growth. `is_dangerous_cwd` returns `true` if a path
//! matches the per-OS built-in denylist or any user-supplied additional entry.
//!
//! Comparison rules:
//! - Both candidate and denylist entries are canonicalized via
//!   [`std::fs::canonicalize`]; symlinks resolve.
//! - Matching is component-wise via [`std::path::Path::starts_with`], so `/var2` does
//!   not match `/var`.
//! - On macOS and Windows the comparison is case-insensitive (lowercased via
//!   `to_string_lossy().to_lowercase()`); on Linux it is byte-exact.
//! - Built-in entries that don't resolve on this machine are silently skipped.
//! - Malformed `additional` entries (relative paths) are skipped with a
//!   `tracing::warn!` log.

use std::path::{Path, PathBuf};

/// Check whether `path` is a dangerous cwd — equal to or a descendant of a
/// built-in (per-OS) or user-supplied denylist entry.
///
/// See the module-level docs for full matching rules.
pub fn is_dangerous_cwd(path: &Path, additional: &[String]) -> bool {
    let home = dirs::home_dir();
    if home.is_none() {
        // Stripped env (systemd unit, container, sandbox without HOME/USERPROFILE/passwd):
        // every $HOME-derived denylist entry silently vanishes. Warn loudly so operators
        // understand why an obviously-dangerous cwd may not be flagged. We do NOT fail
        // closed here: the absolute entries (`/`, `/var`, drive roots, …) still apply,
        // and failing closed would break legitimate use from non-home, non-system trees.
        tracing::warn!(
            "could not resolve home directory; \
             $HOME-derived dangerous-cwd entries are inactive for this invocation"
        );
    }
    is_dangerous_cwd_with_home(path, additional, home.as_deref())
}

/// Test-injectable variant of [`is_dangerous_cwd`] that takes an explicit
/// home directory instead of resolving via [`dirs::home_dir`].
pub(crate) fn is_dangerous_cwd_with_home(
    path: &Path,
    additional: &[String],
    home: Option<&Path>,
) -> bool {
    let canonical_candidate = canonicalize_or_self(path);
    let builtin = builtin_denylist(home);
    is_dangerous_inner(&canonical_candidate, additional, &builtin)
}

/// Returns `true` when `path` (canonicalized) is EQUAL to a built-in or
/// user-supplied denylist entry — not merely a descendant of one.
///
/// Used by `db::check_serve_dangerous_cwd` and `db::check_repo_override_dangerous`
/// to detect a stray `.git` at a dangerous root (e.g. `~/.git` for dotfiles
/// users). When `find_git_root` walks up from a non-git cwd inside `$HOME`
/// and lands on `$HOME/.git`, the resolved git root IS the dangerous root
/// itself — not a real project — so the guard must continue to refuse.
///
/// Distinct from [`is_dangerous_cwd_with_home`]: that one returns `true`
/// for both exact matches AND descendants; this one is exact-only.
pub(crate) fn is_exact_denylist_entry(
    path: &Path,
    additional: &[String],
    home: Option<&Path>,
) -> bool {
    let canonical = canonicalize_or_self(path);
    let builtin = builtin_denylist(home);
    if builtin.iter().any(|entry| paths_equal(&canonical, entry)) {
        return true;
    }
    for raw in additional {
        let trimmed = raw.trim_start();
        if trimmed.starts_with('~') || trimmed.starts_with('$') || trimmed.starts_with('%') {
            continue;
        }
        let entry_path = Path::new(raw);
        if !entry_path.is_absolute() {
            continue;
        }
        let Ok(canonical_entry) = std::fs::canonicalize(entry_path) else {
            continue;
        };
        if paths_equal(&canonical, &canonical_entry) {
            return true;
        }
    }
    false
}

/// Path equality with the same case-folding rules as [`path_matches`].
fn paths_equal(a: &Path, b: &Path) -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        a == b
    }
}

/// Shared implementation: `candidate_canonical` is matched against `builtin`
/// followed by any absolute, resolvable entries in `additional`.
fn is_dangerous_inner(
    candidate_canonical: &Path,
    additional: &[String],
    builtin: &[PathBuf],
) -> bool {
    for entry in builtin {
        if path_matches(candidate_canonical, entry) {
            return true;
        }
    }

    for raw in additional {
        // Catch common misconfigurations that silently fail otherwise:
        // tilde and env-var prefixes are NOT expanded (per the field's
        // doc comment in `ScanConfig`). Warn the user so the silent-skip
        // doesn't read as "I told it about /tmp but it ignored me".
        let trimmed = raw.trim_start();
        if trimmed.starts_with('~') || trimmed.starts_with('$') || trimmed.starts_with('%') {
            tracing::warn!(
                entry = %raw,
                "additional_denylist_paths entry uses tilde or env-var syntax; \
                 these are NOT expanded — use an absolute path instead — skipping"
            );
            continue;
        }
        let entry_path = Path::new(raw);
        if !entry_path.is_absolute() {
            tracing::warn!(
                entry = %raw,
                "additional_denylist_paths entry is not an absolute path; skipping"
            );
            continue;
        }
        let Ok(canonical) = std::fs::canonicalize(entry_path) else {
            // Non-existent / unreadable entries are silent per spec
            // (see PRD US-001 AC: "Denylist entries that don't exist on
            // the current machine are silently skipped"). We still trace
            // at debug for diagnosis but do not warn.
            tracing::debug!(
                entry = %raw,
                "additional_denylist_paths entry could not be canonicalized; skipping"
            );
            continue;
        };
        if path_matches(candidate_canonical, &canonical) {
            return true;
        }
    }

    false
}

/// Canonicalize `path`, falling back to the path as-given on failure
/// (e.g. when the path doesn't exist on disk).
fn canonicalize_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Push `path`'s canonical form to `out` if it resolves on this machine;
/// silently skip otherwise.
fn push_canonical(out: &mut Vec<PathBuf>, path: &Path) {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        out.push(canonical);
    }
}

/// Component-wise prefix match: returns `true` when `candidate == entry` or
/// `candidate` is a descendant of `entry`. On macOS/Windows the comparison is
/// case-insensitive; on Linux it is byte-exact.
fn path_matches(candidate: &Path, entry: &Path) -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let lc_candidate = candidate.to_string_lossy().to_lowercase();
        let lc_entry = entry.to_string_lossy().to_lowercase();
        Path::new(&lc_candidate).starts_with(Path::new(&lc_entry))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        candidate.starts_with(entry)
    }
}

#[cfg(target_os = "macos")]
fn builtin_denylist(home: Option<&Path>) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    if let Some(h) = home {
        for sub in [
            "",
            "Library",
            "Documents",
            "Downloads",
            "Desktop",
            "Pictures",
            "Movies",
            "Music",
            "Public",
            ".config",
            ".cache",
        ] {
            let p = if sub.is_empty() {
                h.to_path_buf()
            } else {
                h.join(sub)
            };
            push_canonical(&mut entries, &p);
        }
    }
    for absolute in [
        "/",
        "/Users",
        "/Applications",
        "/System",
        "/Library",
        "/private",
        "/tmp",
        "/var",
        "/usr",
        "/etc",
        "/opt",
        // External-volume mounts: a 1 TB drive at `/Volumes/Photos`
        // would reproduce the original 90+ GB recursive-walk leak.
        "/Volumes",
        "/Network",
    ] {
        push_canonical(&mut entries, Path::new(absolute));
    }
    entries
}

#[cfg(target_os = "linux")]
fn builtin_denylist(home: Option<&Path>) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    if let Some(h) = home {
        push_canonical(&mut entries, h);
    }
    for absolute in [
        "/", "/home", "/etc", "/var", "/tmp", "/usr", "/opt", "/root", "/proc", "/sys", "/dev",
        // External / pseudo / package mounts that can hide huge trees:
        "/mnt", "/media", "/run", "/snap", "/srv", "/boot",
    ] {
        push_canonical(&mut entries, Path::new(absolute));
    }
    for (env_var, fallback_sub) in [
        ("XDG_CONFIG_HOME", Some(".config")),
        ("XDG_CACHE_HOME", Some(".cache")),
        ("XDG_DATA_HOME", Some(".local/share")),
        // No fallback: XDG_RUNTIME_DIR has no spec'd default — only
        // include it when the env var is set (and absolute / non-empty).
        ("XDG_RUNTIME_DIR", None),
    ] {
        let env_path = std::env::var_os(env_var)
            .map(PathBuf::from)
            // Empty / relative env values would canonicalize against cwd
            // and pollute the denylist with arbitrary paths — skip them.
            .filter(|p| !p.as_os_str().is_empty() && p.is_absolute());
        let path = env_path.or_else(|| fallback_sub.and_then(|sub| home.map(|h| h.join(sub))));
        if let Some(p) = path {
            push_canonical(&mut entries, &p);
        }
    }
    entries
}

#[cfg(target_os = "windows")]
fn builtin_denylist(home: Option<&Path>) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    if let Some(h) = home {
        for sub in ["", "Documents", "Downloads", "Desktop"] {
            let p = if sub.is_empty() {
                h.to_path_buf()
            } else {
                h.join(sub)
            };
            push_canonical(&mut entries, &p);
        }
    }
    // System paths via env (handles non-default install drive / locale):
    // - %SystemRoot%        : typically C:\Windows
    // - %ProgramFiles%      : typically C:\Program Files
    // - %ProgramFiles(x86)% : typically C:\Program Files (x86)
    // - %ProgramData%       : typically C:\ProgramData
    // - %APPDATA%, %LOCALAPPDATA%, %TEMP% : per-user roaming/local/temp
    for env_var in [
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
    ] {
        if let Some(v) = std::env::var_os(env_var) {
            if !v.is_empty() {
                push_canonical(&mut entries, Path::new(&v));
            }
        }
    }
    // Hardcoded fallbacks for the common case where env vars are unset
    // (rare on Windows but possible in service / SYSTEM contexts):
    for absolute in [
        r"C:\Windows",
        r"C:\Program Files",
        r"C:\Program Files (x86)",
        r"C:\ProgramData",
    ] {
        push_canonical(&mut entries, Path::new(absolute));
    }
    // Drive roots A:\..Z:\: only include drives that actually canonicalize
    // (i.e. exist). This intentionally avoids hitting disconnected network
    // drives — `std::fs::canonicalize` will fail fast for them and the
    // entry is silently skipped via `push_canonical`.
    for letter in b'A'..=b'Z' {
        let root = format!(r"{}:\", letter as char);
        push_canonical(&mut entries, Path::new(&root));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ----- path_matches: pure logic, no FS dependency -----

    #[test]
    fn path_matches_exact() {
        assert!(path_matches(Path::new("/var"), Path::new("/var")));
    }

    #[test]
    fn path_matches_descendant() {
        assert!(path_matches(Path::new("/var/foo"), Path::new("/var")));
    }

    #[test]
    fn path_matches_deep_descendant() {
        assert!(path_matches(
            Path::new("/var/foo/bar/baz"),
            Path::new("/var")
        ));
    }

    #[test]
    fn path_matches_sibling_var2_is_not_var() {
        // Component-wise comparison: "var2" is not a prefix of "var".
        assert!(!path_matches(Path::new("/var2"), Path::new("/var")));
        assert!(!path_matches(Path::new("/var2/sub"), Path::new("/var")));
        assert!(!path_matches(Path::new("/var/foo"), Path::new("/var2")));
    }

    #[test]
    fn path_matches_unrelated_root_is_not_matched() {
        assert!(!path_matches(Path::new("/etc"), Path::new("/var")));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn path_matches_case_insensitive_on_macos_windows() {
        assert!(path_matches(
            Path::new("/Users/Foo"),
            Path::new("/users/foo")
        ));
        assert!(path_matches(
            Path::new("/USERS/FOO/bar"),
            Path::new("/Users/Foo")
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn path_matches_case_sensitive_on_linux() {
        assert!(!path_matches(
            Path::new("/Users/Foo"),
            Path::new("/users/foo")
        ));
    }

    // ----- is_dangerous_inner: tests with controlled builtin (OS-agnostic) -----

    #[test]
    fn additional_absolute_entry_matches() {
        let tmp = TempDir::new().unwrap();
        let candidate = canonicalize_or_self(tmp.path());
        let additional = vec![tmp.path().to_string_lossy().into_owned()];
        assert!(is_dangerous_inner(&candidate, &additional, &[]));
    }

    #[test]
    fn additional_subdir_match() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let candidate = canonicalize_or_self(&sub);
        let additional = vec![tmp.path().to_string_lossy().into_owned()];
        assert!(is_dangerous_inner(&candidate, &additional, &[]));
    }

    #[test]
    fn relative_additional_entry_skipped_with_warn() {
        // No matches in builtin or absolute additional, only a relative entry
        // (which should be warn-logged and skipped).
        let tmp = TempDir::new().unwrap();
        let candidate = canonicalize_or_self(tmp.path());
        let additional = vec!["relative/path".to_string()];
        assert!(!is_dangerous_inner(&candidate, &additional, &[]));
    }

    #[test]
    fn unresolvable_additional_entry_silently_skipped() {
        let tmp = TempDir::new().unwrap();
        let candidate = canonicalize_or_self(tmp.path());
        let additional = vec!["/does/not/exist/xyzzy/seshat-test".to_string()];
        assert!(!is_dangerous_inner(&candidate, &additional, &[]));
    }

    #[test]
    fn tilde_prefix_in_additional_is_skipped() {
        // Tilde (~) and env-var ($VAR/%VAR%) prefixes are NOT expanded by
        // design — they would canonicalize against cwd and pollute the
        // denylist with arbitrary paths. The entry must be skipped.
        let tmp = TempDir::new().unwrap();
        let candidate = canonicalize_or_self(tmp.path());
        let additional = vec!["~/scratch".to_string()];
        assert!(!is_dangerous_inner(&candidate, &additional, &[]));
    }

    #[test]
    fn env_var_prefix_in_additional_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let candidate = canonicalize_or_self(tmp.path());
        let additional = vec![
            "$HOME/scratch".to_string(),
            "%USERPROFILE%\\scratch".to_string(),
        ];
        assert!(!is_dangerous_inner(&candidate, &additional, &[]));
    }

    #[test]
    fn no_home_falls_back_to_absolute_entries_only() {
        // `home: None` simulates a stripped env (systemd unit, sandbox).
        // The absolute denylist entries (e.g. `/`, `/var`) still apply, so
        // a candidate that matches one of them is still flagged dangerous.
        // We can't pick a known-canonical absolute path on every host
        // platform, so verify only that the call does not panic.
        let tmp = TempDir::new().unwrap();
        let _ = is_dangerous_cwd_with_home(tmp.path(), &[], None);
    }

    // ----- is_dangerous_cwd_with_home: home injection -----

    #[test]
    fn home_dir_itself_is_dangerous() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        assert!(is_dangerous_cwd_with_home(home, &[], Some(home)));
    }

    #[test]
    fn subdir_under_injected_home_is_dangerous() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let sub = home.join("subproj");
        std::fs::create_dir(&sub).unwrap();
        assert!(is_dangerous_cwd_with_home(&sub, &[], Some(home)));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_dangerous_is_resolved() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("real_home");
        std::fs::create_dir(&target).unwrap();
        let link = tmp.path().join("link_to_home");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // Treat `target` as the home directory; following the symlink
        // should canonicalize to `target` and match.
        assert!(is_dangerous_cwd_with_home(&link, &[], Some(&target)));
    }

    #[test]
    fn malformed_additional_does_not_panic_or_alter_result() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // home itself matches via the injected home → still dangerous, but
        // the relative additional entry must not panic.
        assert!(is_dangerous_cwd_with_home(
            home,
            &["relative/skipped".to_string()],
            Some(home),
        ));
    }

    // ----- builtin_denylist coverage -----

    #[test]
    fn builtin_denylist_contains_injected_home() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let entries = builtin_denylist(Some(home));
        let canonical_home = std::fs::canonicalize(home).unwrap();
        assert!(
            entries.iter().any(|e| e == &canonical_home),
            "builtin_denylist must include the injected home directory"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_xdg_config_fallback_when_env_unset_or_set() {
        // Whether XDG_CONFIG_HOME is set or not on the host, ~/.config under
        // the injected home must still be matched (either via the .config
        // fallback or via the home entry itself).
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let dot_config = home.join(".config");
        std::fs::create_dir(&dot_config).unwrap();
        let canonical_dot_config = std::fs::canonicalize(&dot_config).unwrap();
        let entries = builtin_denylist(Some(home));
        assert!(
            entries.iter().any(|e| canonical_dot_config.starts_with(e)),
            "~/.config must be covered by the Linux denylist"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_builtin_includes_library_under_injected_home() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let lib = home.join("Library");
        std::fs::create_dir(&lib).unwrap();
        let canonical_lib = std::fs::canonicalize(&lib).unwrap();
        let entries = builtin_denylist(Some(home));
        assert!(entries.iter().any(|e| e == &canonical_lib));
    }

    // ----- public entry point smoke test -----

    #[test]
    fn public_is_dangerous_cwd_does_not_panic() {
        // We can't predict whether the host's real cwd is dangerous, but the
        // public entry point must not panic.
        let _ = is_dangerous_cwd(Path::new("."), &[]);
    }
}
