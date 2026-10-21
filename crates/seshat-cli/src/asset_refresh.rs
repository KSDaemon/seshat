//! `brew upgrade` swaps the binary but leaves the skill, hook scripts and
//! instruction blocks that `seshat init` wrote, so agents keep following
//! stale docs. [`refresh_installed_assets`](crate::asset_refresh::refresh_installed_assets)
//! brings them back in line:
//!
//! - Skill files and hook scripts are owned by Seshat. Each asset root
//!   (`~/.claude`, the OpenCode config dir) keeps a `.seshat-assets.json`
//!   stamp with the version that last synced them. An installed file that
//!   differs from the embedded copy is rewritten only when the stamp is
//!   older than this binary: a difference under the same version is a user
//!   edit, and an older binary never downgrades what a newer one wrote.
//!   Symlinked files belong to whoever manages the link and are never
//!   rewritten.
//! - The Seshat block inside `CLAUDE.md` / `AGENTS.md` lives in a file the
//!   user owns and may have edited on purpose, so it is only reported — once
//!   per distinct block content, tracked in the same stamp.
//!
//! Nothing is created: an asset that `seshat init` never installed stays
//! absent. Line endings are normalised before comparing, so a CRLF checkout
//! of the same content is not a difference.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::instructions::{
    AGENTS_MD_CONTENT, HOOK_PRE_TOOL, HOOK_SESSION_START, MARKER_END, MARKER_START,
    SKILL_MD_CONTENT, claude_home, opencode_config_dir,
};

/// Stamp file kept in each asset root.
pub const STAMP_FILE: &str = ".seshat-assets.json";

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// An asset path together with the `seshat init` command that rewrites it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub path: PathBuf,
    pub init_command: &'static str,
}

/// What [`refresh_installed_assets`] found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RefreshReport {
    /// Owned files rewritten with the embedded content.
    pub refreshed: Vec<PathBuf>,
    /// Owned files that should have been refreshed but could not be: a
    /// symlink, or a failed write.
    pub not_refreshed: Vec<Asset>,
    /// Owned files that differ but were deliberately kept: edited under the
    /// current version, synced by a newer binary, or writing disabled.
    pub kept: Vec<PathBuf>,
    /// Instruction files whose Seshat block differs from the embedded one
    /// and has not been reported before.
    pub stale_instruction_blocks: Vec<Asset>,
    /// Whether the findings should reach the agent; `false` when writing is
    /// disabled, so a development build never asks the user to run
    /// `seshat init` with branch content.
    pub notify: bool,
}

impl RefreshReport {
    /// Note for the MCP instructions so the agent can tell the user, or
    /// `None` when there is nothing the user needs to act on.
    pub fn instructions_notice(&self) -> Option<String> {
        if !self.notify {
            return None;
        }
        let home = dirs::home_dir();
        let mut parts = Vec::new();
        if !self.stale_instruction_blocks.is_empty() {
            parts.push(format!(
                "the Seshat block in {} differs from this version. Unless they edited it on \
                 purpose, {} refreshes it",
                list_paths(&self.stale_instruction_blocks, home.as_deref()),
                list_commands(&self.stale_instruction_blocks),
            ));
        }
        if !self.not_refreshed.is_empty() {
            parts.push(format!(
                "{} could not be updated to this version (symlinked or not writable); {} \
                 reinstalls them",
                list_paths(&self.not_refreshed, home.as_deref()),
                list_commands(&self.not_refreshed),
            ));
        }
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "Note for the user, mention it once: {}.",
            parts.join("; ")
        ))
    }
}

/// Refresh the global Claude Code / OpenCode assets for this binary.
///
/// Release builds rewrite outdated owned files and update the stamps; debug
/// builds (tests, `cargo run`) only report, so working on Seshat never
/// touches the real `~/.claude`.
pub fn refresh_installed_assets() -> RefreshReport {
    refresh_assets_in(
        claude_home().as_deref(),
        opencode_config_dir().as_deref(),
        CURRENT_VERSION,
        !cfg!(debug_assertions),
    )
}

/// Record that `root`'s owned assets were just installed by this binary, so
/// a later edit under the same version is kept rather than reverted.
pub fn record_installed_version(root: &Path) {
    let mut stamp = Stamp::load(root);
    if stamp.version_order(CURRENT_VERSION) == std::cmp::Ordering::Less {
        stamp.version = Some(CURRENT_VERSION.to_owned());
        stamp.save(root);
    }
}

/// [`refresh_installed_assets`] against explicit directories and version.
pub fn refresh_assets_in(
    claude_home: Option<&Path>,
    opencode_dir: Option<&Path>,
    version: &str,
    write: bool,
) -> RefreshReport {
    let mut report = RefreshReport {
        notify: write,
        ..RefreshReport::default()
    };
    if let Some(home) = claude_home {
        let root = AssetRoot {
            dir: home,
            init_command: "`seshat init claude-code`",
            owned: vec![
                ("skills/seshat/SKILL.md", SKILL_MD_CONTENT),
                ("hooks/seshat-session-start", HOOK_SESSION_START),
                ("hooks/seshat-pre-tool", HOOK_PRE_TOOL),
            ],
            instruction_file: "CLAUDE.md",
        };
        root.refresh(version, write, &mut report);
    }
    if let Some(dir) = opencode_dir {
        let root = AssetRoot {
            dir,
            init_command: "`seshat init opencode`",
            owned: vec![("skills/seshat/SKILL.md", SKILL_MD_CONTENT)],
            instruction_file: "AGENTS.md",
        };
        root.refresh(version, write, &mut report);
    }
    report
}

struct AssetRoot<'a> {
    dir: &'a Path,
    init_command: &'static str,
    owned: Vec<(&'static str, &'static str)>,
    instruction_file: &'static str,
}

impl AssetRoot<'_> {
    fn refresh(&self, version: &str, write: bool, report: &mut RefreshReport) {
        if !self.dir.is_dir() {
            return;
        }
        let mut stamp = Stamp::load(self.dir);
        let order = stamp.version_order(version);
        let may_rewrite = order == std::cmp::Ordering::Less;
        let mut stamp_changed = false;
        let mut write_failed = false;

        for (rel, content) in &self.owned {
            let path = self.dir.join(rel);
            let Some(current) = read_installed(&path) else {
                continue;
            };
            if normalize(&current) == normalize(content) {
                continue;
            }
            if !may_rewrite {
                tracing::info!(
                    path = %path.display(),
                    stamp = stamp.version.as_deref().unwrap_or("none"),
                    "Seshat asset differs but was synced by this or a newer version; keeping it"
                );
                report.kept.push(path);
            } else if has_symlink(self.dir, &path) {
                tracing::info!(path = %path.display(), "Seshat asset is a symlink; not rewriting it");
                report.not_refreshed.push(self.asset(path));
            } else if !write {
                report.kept.push(path);
            } else {
                match write_atomically(&path, content) {
                    Ok(()) => report.refreshed.push(path),
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "Failed to refresh Seshat asset");
                        report.not_refreshed.push(self.asset(path));
                        write_failed = true;
                    }
                }
            }
        }
        // A failed write keeps the old stamp so the next start retries; a
        // symlink is reported once per version, then kept.
        if may_rewrite && !write_failed {
            stamp.version = Some(version.to_owned());
            stamp_changed = true;
        }

        let instructions = self.dir.join(self.instruction_file);
        if let Some(text) = read_installed(&instructions)
            && let Some(block) = seshat_block(&text)
        {
            let block = normalize(block);
            let key = self.instruction_file.to_owned();
            if block.trim() == normalize(AGENTS_MD_CONTENT).trim() {
                stamp_changed |= stamp.acknowledged_blocks.remove(&key).is_some();
            } else {
                let hash = block_hash(&block);
                if stamp.acknowledged_blocks.get(&key) != Some(&hash) {
                    report
                        .stale_instruction_blocks
                        .push(self.asset(instructions));
                    stamp.acknowledged_blocks.insert(key, hash);
                    stamp_changed = true;
                }
            }
        }

        if write && stamp_changed {
            stamp.save(self.dir);
        }
    }

    fn asset(&self, path: PathBuf) -> Asset {
        Asset {
            path,
            init_command: self.init_command,
        }
    }
}

/// Contents of a root's [`STAMP_FILE`].
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stamp {
    /// Version that last synced the owned assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// Instruction file name → hash of the stale block already reported.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    acknowledged_blocks: BTreeMap<String, String>,
}

impl Stamp {
    /// Missing or unreadable stamps load as empty, which lets the next run
    /// refresh — the same as before stamps existed.
    fn load(root: &Path) -> Self {
        read_installed(&root.join(STAMP_FILE))
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, root: &Path) {
        let path = root.join(STAMP_FILE);
        let result = serde_json::to_string_pretty(self)
            .map_err(io::Error::other)
            .and_then(|json| write_atomically(&path, &json));
        if let Err(e) = result {
            tracing::warn!(path = %path.display(), error = %e, "Failed to write Seshat asset stamp");
        }
    }

    /// The stamp's version compared with `version`; a missing or unparsable
    /// stamp counts as older.
    fn version_order(&self, version: &str) -> std::cmp::Ordering {
        match (
            self.version.as_deref().and_then(parse_version),
            parse_version(version),
        ) {
            (Some(stamp), Some(current)) => stamp.cmp(&current),
            _ => std::cmp::Ordering::Less,
        }
    }
}

/// `major.minor.patch`, ignoring any pre-release or build suffix.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Read an installed file. A missing file is simply not installed; any
/// other failure is logged, since the asset then stays stale unnoticed.
fn read_installed(path: &Path) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Cannot read Seshat asset");
            None
        }
    }
}

fn normalize(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

fn block_hash(block: &str) -> String {
    let digest = Sha256::digest(block.trim().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `true` when `path`, or any directory between it and `root`, is a symlink.
fn has_symlink(root: &Path, path: &Path) -> bool {
    path.ancestors()
        .take_while(|p| *p != root)
        .any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
}

/// The text between the Seshat markers, located the way `seshat init`
/// locates them, or `None` when the file has no well-formed block (then it
/// isn't ours to judge, and `init` would refuse it too).
fn seshat_block(text: &str) -> Option<&str> {
    let start = text.find(MARKER_START)?;
    let end = text.find(MARKER_END)?;
    text.get(start + MARKER_START.len()..end)
}

fn list_paths(assets: &[Asset], home: Option<&Path>) -> String {
    assets
        .iter()
        .map(|a| display_path(&a.path, home))
        .collect::<Vec<_>>()
        .join(", ")
}

fn list_commands(assets: &[Asset]) -> String {
    let mut commands: Vec<&str> = assets.iter().map(|a| a.init_command).collect();
    commands.dedup();
    commands.join(" / ")
}

/// `path` with the home directory shown as `~`, so the user's account name
/// and layout don't go into the model's instructions.
fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Write via a sibling temp file and rename, so a concurrent `seshat serve`
/// (one per project) or an agent reading the file never sees it half-written.
/// The replaced file's permissions carry over — a hook the user made
/// non-executable stays that way.
fn write_atomically(path: &Path, content: &str) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("asset path has no file name"))?;
    let tmp = path.with_file_name(format!(
        ".{}.seshat-tmp-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        fs::write(&tmp, content)?;
        if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const V: &str = "0.9.0";

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn block(content: &str) -> String {
        format!("# Mine\n\n{MARKER_START}\n{content}\n{MARKER_END}\n\nmore of mine\n")
    }

    fn stamp(root: &Path, version: &str) {
        write(
            &root.join(STAMP_FILE),
            &format!("{{\"version\":\"{version}\"}}"),
        );
    }

    fn stamp_version(root: &Path) -> Option<String> {
        Stamp::load(root).version
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

        let report = refresh_assets_in(Some(home.path()), None, V, true);

        assert_eq!(report.refreshed, vec![skill.clone(), pre_tool.clone()]);
        assert!(report.not_refreshed.is_empty());
        assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL_MD_CONTENT);
        assert_eq!(fs::read_to_string(&pre_tool).unwrap(), HOOK_PRE_TOOL);
        assert_eq!(stamp_version(home.path()).as_deref(), Some(V));
        assert_eq!(report.instructions_notice(), None);
    }

    #[test]
    fn older_stamp_allows_rewrite() {
        let home = tempdir().unwrap();
        let skill = home.path().join("skills/seshat/SKILL.md");
        write(&skill, "old skill");
        stamp(home.path(), "0.8.3");

        let report = refresh_assets_in(Some(home.path()), None, V, true);

        assert_eq!(report.refreshed, vec![skill.clone()]);
        assert_eq!(stamp_version(home.path()).as_deref(), Some(V));
    }

    #[test]
    fn same_version_edit_is_kept() {
        let home = tempdir().unwrap();
        let skill = home.path().join("skills/seshat/SKILL.md");
        write(&skill, "my edited skill");
        stamp(home.path(), V);

        let report = refresh_assets_in(Some(home.path()), None, V, true);

        assert_eq!(report.kept, vec![skill.clone()]);
        assert!(report.refreshed.is_empty());
        assert_eq!(fs::read_to_string(&skill).unwrap(), "my edited skill");
        assert_eq!(report.instructions_notice(), None);
    }

    #[test]
    fn older_binary_never_downgrades_newer_assets() {
        let home = tempdir().unwrap();
        let skill = home.path().join("skills/seshat/SKILL.md");
        write(&skill, "skill from 0.10");
        stamp(home.path(), "0.10.0");

        let report = refresh_assets_in(Some(home.path()), None, V, true);

        assert_eq!(report.kept, vec![skill.clone()]);
        assert_eq!(fs::read_to_string(&skill).unwrap(), "skill from 0.10");
        assert_eq!(stamp_version(home.path()).as_deref(), Some("0.10.0"));
    }

    #[test]
    fn version_parsing_compares_numerically() {
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
    }

    #[test]
    fn never_creates_assets_that_were_not_installed() {
        let home = tempdir().unwrap();
        let opencode = tempdir().unwrap();
        // The directories exist, so a write would succeed if one were attempted.
        fs::create_dir_all(home.path().join("skills/seshat")).unwrap();
        fs::create_dir_all(home.path().join("hooks")).unwrap();
        fs::create_dir_all(opencode.path().join("skills/seshat")).unwrap();

        let report = refresh_assets_in(Some(home.path()), Some(opencode.path()), V, true);

        assert!(
            report.refreshed.is_empty()
                && report.kept.is_empty()
                && report.not_refreshed.is_empty(),
            "{report:?}"
        );
        assert!(!home.path().join("skills/seshat/SKILL.md").exists());
        assert!(!home.path().join("hooks/seshat-pre-tool").exists());
        assert!(!home.path().join("hooks/seshat-session-start").exists());
        assert!(!opencode.path().join("skills/seshat/SKILL.md").exists());
    }

    #[test]
    fn missing_root_dir_is_left_alone() {
        let parent = tempdir().unwrap();
        let home = parent.path().join("no-such-claude");

        refresh_assets_in(Some(&home), None, V, true);

        assert!(!home.exists());
    }

    #[test]
    fn refreshes_opencode_skill() {
        let opencode = tempdir().unwrap();
        let skill = opencode.path().join("skills/seshat/SKILL.md");
        write(&skill, "old skill");

        let report = refresh_assets_in(None, Some(opencode.path()), V, true);

        assert_eq!(report.refreshed, vec![skill.clone()]);
        assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL_MD_CONTENT);
    }

    #[test]
    fn without_write_nothing_is_written_and_nothing_is_announced() {
        let home = tempdir().unwrap();
        let skill = home.path().join("skills/seshat/SKILL.md");
        write(&skill, "old skill");
        write(&home.path().join("CLAUDE.md"), &block("old seshat rules"));

        let report = refresh_assets_in(Some(home.path()), None, V, false);

        assert_eq!(report.kept, vec![skill.clone()]);
        assert_eq!(report.stale_instruction_blocks.len(), 1);
        assert_eq!(fs::read_to_string(&skill).unwrap(), "old skill");
        assert!(!home.path().join(STAMP_FILE).exists());
        assert_eq!(report.instructions_notice(), None);
    }

    #[test]
    fn crlf_copy_of_current_content_is_not_a_difference() {
        let home = tempdir().unwrap();
        let hook = home.path().join("hooks/seshat-pre-tool");
        write(&hook, &HOOK_PRE_TOOL.replace('\n', "\r\n"));
        write(
            &home.path().join("CLAUDE.md"),
            &block(AGENTS_MD_CONTENT).replace('\n', "\r\n"),
        );

        let report = refresh_assets_in(Some(home.path()), None, V, true);

        assert!(report.refreshed.is_empty(), "{report:?}");
        assert!(report.stale_instruction_blocks.is_empty(), "{report:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_asset_is_not_rewritten() {
        let home = tempdir().unwrap();
        let dotfiles = tempdir().unwrap();
        let target = dotfiles.path().join("seshat-pre-tool");
        write(&target, "old hook");
        let hook = home.path().join("hooks/seshat-pre-tool");
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &hook).unwrap();

        let report = refresh_assets_in(Some(home.path()), None, V, true);

        assert!(
            fs::symlink_metadata(&hook)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "old hook");
        assert_eq!(report.not_refreshed.len(), 1);
        assert_eq!(report.not_refreshed[0].path, hook);

        let again = refresh_assets_in(Some(home.path()), None, V, true);
        assert!(
            again.not_refreshed.is_empty(),
            "reported once per version: {again:?}"
        );
        assert_eq!(again.instructions_notice(), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_dir_is_not_rewritten() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        write(&repo.path().join("SKILL.md"), "repo skill");
        fs::create_dir_all(home.path().join("skills")).unwrap();
        std::os::unix::fs::symlink(repo.path(), home.path().join("skills/seshat")).unwrap();

        let report = refresh_assets_in(Some(home.path()), None, V, true);

        assert_eq!(
            fs::read_to_string(repo.path().join("SKILL.md")).unwrap(),
            "repo skill"
        );
        assert_eq!(report.not_refreshed.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn rewrite_keeps_the_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempdir().unwrap();
        let hook = home.path().join("hooks/seshat-pre-tool");
        write(&hook, "old hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o640)).unwrap();

        refresh_assets_in(Some(home.path()), None, V, true);

        assert_eq!(fs::read_to_string(&hook).unwrap(), HOOK_PRE_TOOL);
        let mode = fs::metadata(&hook).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn failed_write_is_reported_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempdir().unwrap();
        let hooks = home.path().join("hooks");
        let hook = hooks.join("seshat-pre-tool");
        write(&hook, "old hook");
        fs::set_permissions(&hooks, fs::Permissions::from_mode(0o500)).unwrap();

        let report = refresh_assets_in(Some(home.path()), None, V, true);
        fs::set_permissions(&hooks, fs::Permissions::from_mode(0o755)).unwrap();

        if report.refreshed.is_empty() {
            // Running as root ignores the directory mode; only check when
            // the write really failed.
            assert_eq!(report.not_refreshed.len(), 1);
            let names: Vec<_> = fs::read_dir(&hooks)
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert_eq!(names, vec![std::ffi::OsString::from("seshat-pre-tool")]);
            let notice = report.instructions_notice().unwrap();
            assert!(notice.contains("could not be updated"), "{notice}");
            assert!(notice.contains("seshat init claude-code"), "{notice}");
            assert_eq!(
                stamp_version(home.path()),
                None,
                "a failed write must be retried"
            );
        }
    }

    #[test]
    fn write_atomically_cleans_up_when_rename_fails() {
        let dir = tempdir().unwrap();
        // A directory in the way makes the rename fail after the temp file
        // was written.
        let target = dir.path().join("seshat-pre-tool");
        fs::create_dir_all(target.join("child")).unwrap();

        assert!(write_atomically(&target, "x").is_err());

        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("seshat-pre-tool")]);
    }

    #[test]
    fn stale_instruction_block_is_reported_once() {
        let home = tempdir().unwrap();
        let claude_md = home.path().join("CLAUDE.md");
        let original = block("old seshat rules");
        write(&claude_md, &original);

        let first = refresh_assets_in(Some(home.path()), None, V, true);
        assert_eq!(first.stale_instruction_blocks.len(), 1);
        assert_eq!(fs::read_to_string(&claude_md).unwrap(), original);
        let notice = first.instructions_notice().unwrap();
        assert!(notice.contains("seshat init claude-code"), "{notice}");
        assert!(notice.contains("CLAUDE.md"), "{notice}");
        assert!(notice.contains("on purpose"), "{notice}");

        let second = refresh_assets_in(Some(home.path()), None, V, true);
        assert!(second.stale_instruction_blocks.is_empty(), "{second:?}");
        assert_eq!(second.instructions_notice(), None);

        write(&claude_md, &block("edited again"));
        let third = refresh_assets_in(Some(home.path()), None, V, true);
        assert_eq!(third.stale_instruction_blocks.len(), 1);
    }

    #[test]
    fn stale_opencode_block_points_at_the_opencode_init() {
        let opencode = tempdir().unwrap();
        write(
            &opencode.path().join("AGENTS.md"),
            &block("old seshat rules"),
        );

        let report = refresh_assets_in(None, Some(opencode.path()), V, true);

        let notice = report.instructions_notice().unwrap();
        assert!(notice.contains("`seshat init opencode`"), "{notice}");
        assert!(!notice.contains("claude-code"), "{notice}");
    }

    #[test]
    fn current_or_missing_instruction_block_is_not_reported() {
        let home = tempdir().unwrap();
        let opencode = tempdir().unwrap();
        write(&home.path().join("CLAUDE.md"), &block(AGENTS_MD_CONTENT));
        write(&opencode.path().join("AGENTS.md"), "no seshat block here\n");

        let report = refresh_assets_in(Some(home.path()), Some(opencode.path()), V, true);

        assert!(report.stale_instruction_blocks.is_empty(), "{report:?}");
    }

    #[test]
    fn malformed_markers_are_not_judged() {
        let end_first = format!("{MARKER_END}\nstray\n{MARKER_START}\nold\n{MARKER_END}\n");
        assert_eq!(seshat_block(&end_first), None);
        assert_eq!(seshat_block(&format!("{MARKER_START}\nold\n")), None);
        assert_eq!(
            seshat_block(&format!("{MARKER_START}\nbody\n{MARKER_END}")),
            Some("\nbody\n")
        );
    }

    #[test]
    fn notice_names_the_client_command_and_hides_the_home_dir() {
        let report = RefreshReport {
            stale_instruction_blocks: vec![Asset {
                path: PathBuf::from("/home/u/.config/opencode/AGENTS.md"),
                init_command: "`seshat init opencode`",
            }],
            notify: true,
            ..RefreshReport::default()
        };
        let notice = report.instructions_notice().unwrap();
        assert!(notice.contains("seshat init opencode"), "{notice}");
        assert!(!notice.contains("could not be updated"), "{notice}");

        assert_eq!(
            display_path(
                Path::new("/home/u/.claude/CLAUDE.md"),
                Some(Path::new("/home/u"))
            ),
            "~/.claude/CLAUDE.md"
        );
    }

    #[test]
    fn record_installed_version_only_moves_forward() {
        let home = tempdir().unwrap();
        record_installed_version(home.path());
        assert_eq!(stamp_version(home.path()).as_deref(), Some(CURRENT_VERSION));

        stamp(home.path(), "999.0.0");
        record_installed_version(home.path());
        assert_eq!(stamp_version(home.path()).as_deref(), Some("999.0.0"));
    }
}
