//! Scope resolution for MCP tool queries.
//!
//! Routes each MCP request to the correct database (root project or submodule)
//! based on an explicit `scope` parameter, a `file_path` prefix match, or
//! falling back to the root project.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::envelope::ErrorCode;

// ── ProjectConnection ────────────────────────────────────────

/// A named database connection for a project or submodule.
///
/// Holds the shared `Connection`, the human-readable project name, and the
/// active branch. Used by tool handlers to route queries to the correct
/// knowledge graph.
#[derive(Debug, Clone)]
pub struct ProjectConnection {
    /// Shared database connection.
    pub conn: Arc<Mutex<Connection>>,
    /// Human-readable project/submodule name (e.g. `"seshat"` or `"vendor/lib"`).
    pub name: String,
    /// Active branch stored in this database.
    pub branch: String,
}

impl ProjectConnection {
    /// Create a new `ProjectConnection`.
    pub fn new(
        conn: Arc<Mutex<Connection>>,
        name: impl Into<String>,
        branch: impl Into<String>,
    ) -> Self {
        Self {
            conn,
            name: name.into(),
            branch: branch.into(),
        }
    }
}

// ── resolve_scope ────────────────────────────────────────────

/// Resolve which `ProjectConnection` should handle a request.
///
/// **Priority order:**
/// 1. Explicit `scope` parameter (full mount path → direct lookup; short name
///    → fallback with ambiguity check; `"root"` → root connection).
/// 2. `file_path` prefix match — longest matching mount path wins.
/// 3. Default to root connection.
///
/// # Errors
///
/// Returns `ErrorCode::UnknownScope` if an explicit scope doesn't match any
/// known project or submodule.
pub fn resolve_scope<'a>(
    scope: Option<&str>,
    file_path: Option<&str>,
    root: &'a ProjectConnection,
    submodules: &'a std::collections::HashMap<String, ProjectConnection>,
    mount_paths: &[String],
) -> Result<(&'a ProjectConnection, String), ErrorCode> {
    // ── 1. Explicit scope ────────────────────────────────────
    if let Some(scope_str) = scope {
        let s = normalize_file_path(scope_str.trim());
        if s.is_empty() || s == "." || s.eq_ignore_ascii_case("root") {
            return Ok((root, "root".to_owned()));
        }

        // Try full mount path first (exact match).
        if let Some(pc) = submodules.get(s) {
            return Ok((pc, s.to_owned()));
        }

        // Fallback: short name match (last path segment of mount paths).
        let mut matches: Vec<(&String, &ProjectConnection)> = submodules
            .iter()
            .filter(|(mount, _)| {
                mount
                    .rsplit('/')
                    .next()
                    .is_some_and(|short| short.eq_ignore_ascii_case(s))
            })
            .collect();

        match matches.len() {
            1 => {
                let (mount, pc) = matches.remove(0);
                return Ok((pc, mount.clone()));
            }
            n if n > 1 => {
                // Ambiguous short name — treat as unknown so the caller gets
                // a clear error rather than a random pick.
                return Err(ErrorCode::UnknownScope);
            }
            _ => {}
        }

        // Nothing matched.
        return Err(ErrorCode::UnknownScope);
    }

    // ── 2. file_path prefix match (longest wins) ─────────────
    if let Some(fp) = file_path {
        let normalized = normalize_file_path(fp);

        let mut best: Option<(&String, &ProjectConnection)> = None;
        let mut best_len = 0;

        for mount in mount_paths {
            // Check if the file path starts with this mount path
            // and the next character (if any) is a `/` separator.
            if normalized.starts_with(mount.as_str())
                && (normalized.len() == mount.len()
                    || normalized.as_bytes().get(mount.len()) == Some(&b'/'))
                && mount.len() > best_len
            {
                if let Some(pc) = submodules.get(mount) {
                    best = Some((mount, pc));
                    best_len = mount.len();
                }
            }
        }

        if let Some((mount, pc)) = best {
            return Ok((pc, mount.clone()));
        }
    }

    // ── 3. Default to root ───────────────────────────────────
    Ok((root, "root".to_owned()))
}

/// Strip leading `./` or `/` so that file paths are always relative.
fn normalize_file_path(fp: &str) -> &str {
    let s = fp.strip_prefix("./").unwrap_or(fp);
    s.strip_prefix('/').unwrap_or(s)
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::test_helpers::make_conn;

    /// Build a test fixture with root + two submodules.
    fn fixture() -> (
        ProjectConnection,
        HashMap<String, ProjectConnection>,
        Vec<String>,
    ) {
        let root = make_conn("my-project", "main");

        let mut subs = HashMap::new();
        subs.insert(
            "vendor/libfoo".to_owned(),
            make_conn("vendor/libfoo", "main"),
        );
        subs.insert(
            "vendor/libbar".to_owned(),
            make_conn("vendor/libbar", "develop"),
        );

        let mount_paths = vec!["vendor/libfoo".to_owned(), "vendor/libbar".to_owned()];

        (root, subs, mount_paths)
    }

    // ── Test: explicit scope = "root" ────────────────────────

    #[test]
    fn explicit_scope_root_returns_root() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(Some("root"), None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        // Verify it's the root connection by name.
        assert_eq!(pc.name, "my-project");
    }

    #[test]
    fn explicit_scope_root_case_insensitive() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(Some("ROOT"), None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "my-project");
    }

    // ── Test: explicit scope = full mount path ───────────────

    #[test]
    fn explicit_scope_full_mount_path() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) =
            resolve_scope(Some("vendor/libfoo"), None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "vendor/libfoo");
        assert_eq!(pc.name, "vendor/libfoo");
    }

    // ── Test: explicit scope = short name (unambiguous) ──────

    #[test]
    fn explicit_scope_short_name_unambiguous() {
        let root = make_conn("proj", "main");
        let mut subs = HashMap::new();
        subs.insert("libs/unique".to_owned(), make_conn("libs/unique", "main"));
        let mounts = vec!["libs/unique".to_owned()];

        let (pc, scope_name) = resolve_scope(Some("unique"), None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "libs/unique");
        assert_eq!(pc.name, "libs/unique");
    }

    // ── Test: explicit scope = short name (ambiguous) ────────

    #[test]
    fn explicit_scope_short_name_ambiguous_returns_error() {
        let root = make_conn("proj", "main");
        let mut subs = HashMap::new();
        subs.insert("a/common".to_owned(), make_conn("a/common", "main"));
        subs.insert("b/common".to_owned(), make_conn("b/common", "main"));
        let mounts = vec!["a/common".to_owned(), "b/common".to_owned()];

        let err = resolve_scope(Some("common"), None, &root, &subs, &mounts).unwrap_err();
        assert_eq!(err, ErrorCode::UnknownScope);
    }

    // ── Test: explicit scope = unknown ───────────────────────

    #[test]
    fn explicit_scope_unknown_returns_error() {
        let (root, subs, mounts) = fixture();
        let err = resolve_scope(Some("nonexistent"), None, &root, &subs, &mounts).unwrap_err();
        assert_eq!(err, ErrorCode::UnknownScope);
    }

    // ── Test: file_path auto-detect ──────────────────────────

    #[test]
    fn file_path_prefix_match() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(
            None,
            Some("vendor/libfoo/src/main.rs"),
            &root,
            &subs,
            &mounts,
        )
        .unwrap();
        assert_eq!(scope_name, "vendor/libfoo");
        assert_eq!(pc.name, "vendor/libfoo");
    }

    #[test]
    fn file_path_longest_prefix_wins() {
        let root = make_conn("proj", "main");
        let mut subs = HashMap::new();
        subs.insert("vendor".to_owned(), make_conn("vendor", "main"));
        subs.insert(
            "vendor/deep/nested".to_owned(),
            make_conn("vendor/deep/nested", "main"),
        );
        let mounts = vec!["vendor".to_owned(), "vendor/deep/nested".to_owned()];

        let (pc, scope_name) = resolve_scope(
            None,
            Some("vendor/deep/nested/src/lib.rs"),
            &root,
            &subs,
            &mounts,
        )
        .unwrap();
        assert_eq!(scope_name, "vendor/deep/nested");
        assert_eq!(pc.name, "vendor/deep/nested");
    }

    #[test]
    fn file_path_no_match_falls_through_to_root() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) =
            resolve_scope(None, Some("src/main.rs"), &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "my-project");
    }

    #[test]
    fn file_path_normalized_leading_dot_slash() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(
            None,
            Some("./vendor/libbar/src/lib.rs"),
            &root,
            &subs,
            &mounts,
        )
        .unwrap();
        assert_eq!(scope_name, "vendor/libbar");
        assert_eq!(pc.name, "vendor/libbar");
    }

    #[test]
    fn file_path_normalized_leading_slash() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(
            None,
            Some("/vendor/libfoo/Cargo.toml"),
            &root,
            &subs,
            &mounts,
        )
        .unwrap();
        assert_eq!(scope_name, "vendor/libfoo");
        assert_eq!(pc.name, "vendor/libfoo");
    }

    // ── Test: default root fallback ──────────────────────────

    #[test]
    fn no_scope_no_file_path_returns_root() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(None, None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "my-project");
    }

    // ── Test: empty scope treated as root ────────────────────

    #[test]
    fn empty_scope_returns_root() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(Some(""), None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "my-project");
    }

    // ── Test: dot scope returns root ─────────────────────────

    #[test]
    fn dot_scope_returns_root() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(Some("."), None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "my-project");
    }

    #[test]
    fn dot_slash_scope_returns_root() {
        let (root, subs, mounts) = fixture();
        let (pc, scope_name) = resolve_scope(Some("./"), None, &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "my-project");
    }

    // ── Test: explicit scope takes priority over file_path ───

    #[test]
    fn explicit_scope_overrides_file_path() {
        let (root, subs, mounts) = fixture();
        // file_path points to libfoo, but scope explicitly says libbar.
        let (pc, scope_name) = resolve_scope(
            Some("vendor/libbar"),
            Some("vendor/libfoo/src/main.rs"),
            &root,
            &subs,
            &mounts,
        )
        .unwrap();
        assert_eq!(scope_name, "vendor/libbar");
        assert_eq!(pc.name, "vendor/libbar");
        assert_eq!(pc.branch, "develop");
    }

    // ── Test: file_path partial mount name doesn't match ─────

    #[test]
    fn file_path_partial_mount_name_no_false_positive() {
        let root = make_conn("proj", "main");
        let mut subs = HashMap::new();
        subs.insert("vendor/lib".to_owned(), make_conn("vendor/lib", "main"));
        let mounts = vec!["vendor/lib".to_owned()];

        // "vendor/library/foo.rs" should NOT match "vendor/lib" because
        // after the mount path there's no `/` separator (it's "rary/foo.rs").
        let (pc, scope_name) =
            resolve_scope(None, Some("vendor/library/foo.rs"), &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "proj");
    }

    // ── Test: empty submodules (single-project mode) ─────────

    #[test]
    fn empty_submodules_always_returns_root() {
        let root = make_conn("solo", "main");
        let subs = HashMap::new();
        let mounts: Vec<String> = vec![];

        let (pc, scope_name) =
            resolve_scope(None, Some("src/main.rs"), &root, &subs, &mounts).unwrap();
        assert_eq!(scope_name, "root");
        assert_eq!(pc.name, "solo");
    }
}
