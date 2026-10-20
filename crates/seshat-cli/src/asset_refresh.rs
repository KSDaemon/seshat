//! Keep agent assets installed by `seshat init` in sync with this binary.
//!
//! `brew upgrade` swaps the binary but leaves the skill, hook scripts and
//! instruction blocks that `seshat init` wrote, so agents keep following
//! stale docs. `seshat serve` calls [`refresh_installed_assets`] at startup:
//!
//! - Skill files and hook scripts are wholly owned by Seshat — when one is
//!   installed and its content differs from the embedded copy, it is
//!   rewritten.
//! - The Seshat block inside `CLAUDE.md` / `AGENTS.md` lives in a file the
//!   user owns and may have edited on purpose, so it is only reported.
//!
//! Nothing is created: an asset that `seshat init` never installed stays
//! absent. Detection compares content, not versions, so a release that
//! didn't touch an asset never triggers a rewrite.

use std::fs;
use std::path::{Path, PathBuf};

use crate::instructions::{
    AGENTS_MD_CONTENT, HOOK_PRE_TOOL, HOOK_SESSION_START, MARKER_END, MARKER_START,
    SKILL_MD_CONTENT, claude_home, opencode_config_dir,
};

/// What [`refresh_installed_assets`] found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Owned files rewritten with the embedded content.
    pub refreshed: Vec<PathBuf>,
    /// Owned files that differ but were left alone (`write = false`).
    pub outdated: Vec<PathBuf>,
    /// Instruction files whose Seshat block differs from the embedded one.
    pub stale_instruction_blocks: Vec<PathBuf>,
}

impl RefreshReport {
    /// Note for the MCP instructions so the agent can tell the user, or
    /// `None` when there is nothing the user needs to act on.
    pub fn instructions_notice(&self) -> Option<String> {
        let mut stale: Vec<&PathBuf> = self.stale_instruction_blocks.iter().collect();
        stale.extend(&self.outdated);
        if stale.is_empty() {
            return None;
        }
        let paths = stale
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "Note for the user: Seshat's installed instructions differ from this \
             version ({paths}). Tell the user once that running `seshat init` refreshes \
             them — unless they edited the Seshat block on purpose, in which case \
             nothing needs to change."
        ))
    }
}

/// Refresh the global Claude Code / OpenCode assets for this binary.
///
/// Release builds rewrite outdated owned files; debug builds (tests,
/// `cargo run`) only report them, so working on Seshat never touches the
/// real `~/.claude`.
pub fn refresh_installed_assets() -> RefreshReport {
    refresh_assets_in(
        claude_home().as_deref(),
        opencode_config_dir().as_deref(),
        !cfg!(debug_assertions),
    )
}

/// [`refresh_installed_assets`] against explicit directories.
pub fn refresh_assets_in(
    claude_home: Option<&Path>,
    opencode_dir: Option<&Path>,
    write: bool,
) -> RefreshReport {
    let mut report = RefreshReport::default();

    let mut owned: Vec<(PathBuf, &str, bool)> = Vec::new();
    let mut instruction_files: Vec<PathBuf> = Vec::new();
    if let Some(home) = claude_home {
        owned.push((home.join("skills/seshat/SKILL.md"), SKILL_MD_CONTENT, false));
        owned.push((
            home.join("hooks/seshat-session-start"),
            HOOK_SESSION_START,
            true,
        ));
        owned.push((home.join("hooks/seshat-pre-tool"), HOOK_PRE_TOOL, true));
        instruction_files.push(home.join("CLAUDE.md"));
    }
    if let Some(dir) = opencode_dir {
        owned.push((dir.join("skills/seshat/SKILL.md"), SKILL_MD_CONTENT, false));
        instruction_files.push(dir.join("AGENTS.md"));
    }

    for (path, content, executable) in owned {
        let Ok(current) = fs::read_to_string(&path) else {
            continue; // not installed (or unreadable) — leave it alone
        };
        if current == content {
            continue;
        }
        if !write {
            report.outdated.push(path);
            continue;
        }
        match write_atomically(&path, content, executable) {
            Ok(()) => report.refreshed.push(path),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Failed to refresh Seshat asset");
                report.outdated.push(path);
            }
        }
    }

    for path in instruction_files {
        if let Ok(text) = fs::read_to_string(&path)
            && seshat_block(&text).is_some_and(|block| block.trim() != AGENTS_MD_CONTENT.trim())
        {
            report.stale_instruction_blocks.push(path);
        }
    }

    report
}

/// The text between the Seshat markers, or `None` when the file has no
/// well-formed block (then it isn't ours to judge).
fn seshat_block(text: &str) -> Option<&str> {
    let start = text.find(MARKER_START)? + MARKER_START.len();
    let end = start + text[start..].find(MARKER_END)?;
    Some(&text[start..end])
}

/// Write via a sibling temp file and rename, so a concurrent `seshat serve`
/// (one per project) or an agent reading the file never sees it half-written.
fn write_atomically(path: &Path, content: &str, executable: bool) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("seshat-tmp-{}", std::process::id()));
    fs::write(&tmp, content)?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn block(content: &str) -> String {
        format!("# Mine\n\n{MARKER_START}\n{content}\n{MARKER_END}\n\nmore of mine\n")
    }

    #[test]
    fn rewrites_outdated_owned_files_and_keeps_current_ones() {
        let home = tempdir().unwrap();
        let skill = home.path().join("skills/seshat/SKILL.md");
        let pre_tool = home.path().join("hooks/seshat-pre-tool");
        let session = home.path().join("hooks/seshat-session-start");
        write(&skill, "old skill");
        write(&pre_tool, "old hook");
        write(&session, HOOK_SESSION_START);

        let report = refresh_assets_in(Some(home.path()), None, true);

        assert_eq!(report.refreshed, vec![skill.clone(), pre_tool.clone()]);
        assert!(report.outdated.is_empty());
        assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL_MD_CONTENT);
        assert_eq!(fs::read_to_string(&pre_tool).unwrap(), HOOK_PRE_TOOL);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&pre_tool).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        assert_eq!(report.instructions_notice(), None);
    }

    #[test]
    fn never_creates_assets_that_were_not_installed() {
        let home = tempdir().unwrap();
        let opencode = tempdir().unwrap();

        let report = refresh_assets_in(Some(home.path()), Some(opencode.path()), true);

        assert_eq!(report, RefreshReport::default());
        assert!(!home.path().join("skills").exists());
        assert!(!home.path().join("hooks").exists());
    }

    #[test]
    fn without_write_only_reports_outdated_files() {
        let home = tempdir().unwrap();
        let skill = home.path().join("skills/seshat/SKILL.md");
        write(&skill, "old skill");

        let report = refresh_assets_in(Some(home.path()), None, false);

        assert_eq!(report.outdated, vec![skill.clone()]);
        assert_eq!(fs::read_to_string(&skill).unwrap(), "old skill");
        assert!(report.instructions_notice().is_some());
    }

    #[test]
    fn stale_instruction_block_is_reported_not_rewritten() {
        let home = tempdir().unwrap();
        let claude_md = home.path().join("CLAUDE.md");
        let original = block("old seshat rules");
        write(&claude_md, &original);

        let report = refresh_assets_in(Some(home.path()), None, true);

        assert_eq!(report.stale_instruction_blocks, vec![claude_md.clone()]);
        assert_eq!(fs::read_to_string(&claude_md).unwrap(), original);
        let notice = report.instructions_notice().unwrap();
        assert!(notice.contains("seshat init"), "{notice}");
        assert!(notice.contains("CLAUDE.md"), "{notice}");
    }

    #[test]
    fn current_or_missing_instruction_block_is_not_reported() {
        let home = tempdir().unwrap();
        let opencode = tempdir().unwrap();
        write(&home.path().join("CLAUDE.md"), &block(AGENTS_MD_CONTENT));
        write(&opencode.path().join("AGENTS.md"), "no seshat block here\n");

        let report = refresh_assets_in(Some(home.path()), Some(opencode.path()), true);

        assert!(report.stale_instruction_blocks.is_empty(), "{report:?}");
    }
}
