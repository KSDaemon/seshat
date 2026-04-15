//! Agent instruction file management for `seshat init`.
//!
//! Writes and maintains Seshat usage instructions in AI agent config files
//! (AGENTS.md, CLAUDE.md), installs the Seshat skill file, and registers
//! Claude Code hooks — all idempotently using HTML comment markers.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::CliError;

// ---------------------------------------------------------------------------
// Embedded source files (compiled into the binary at build time)
// ---------------------------------------------------------------------------

/// Compact instructions for AGENTS.md / CLAUDE.md.
/// Contains idempotency markers `<!-- seshat:start -->` / `<!-- seshat:end -->`.
pub const AGENTS_MD_CONTENT: &str = include_str!("../../../rules/seshat.md");

/// Full reference skill for on-demand loading by Claude Code / OpenCode.
pub const SKILL_MD_CONTENT: &str = include_str!("../../../skills/seshat/SKILL.md");

/// Soft SessionStart hook — prints a reminder at session start (exit 0).
pub const HOOK_SESSION_START: &str = include_str!("../../../rules/hooks/seshat-session-start");

/// Soft PreToolUse hook — one nudge per session before Grep/Glob/Read (exit 0).
pub const HOOK_PRE_TOOL: &str = include_str!("../../../rules/hooks/seshat-pre-tool");

// ---------------------------------------------------------------------------
// Marker constants
// ---------------------------------------------------------------------------

const MARKER_START: &str = "<!-- seshat:start -->";
const MARKER_END: &str = "<!-- seshat:end -->";

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Outcome of an `upsert_instructions` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertResult {
    /// File did not exist — created with seshat section.
    Created,
    /// File existed, no markers found — section appended.
    Appended,
    /// File existed, markers found — section replaced.
    Updated,
    /// `dry_run = true` — no file was written.
    DryRun,
}

impl UpsertResult {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Appended => "appended",
            Self::Updated => "updated",
            Self::DryRun => "dry-run (no changes written)",
        }
    }
}

/// Outcome of `install_skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillResult {
    /// Skill file written (created or overwritten).
    Installed,
    /// `dry_run = true` — no file was written.
    DryRun,
}

// ---------------------------------------------------------------------------
// Core functions
// ---------------------------------------------------------------------------

/// Write or update the Seshat instruction section in an agent instruction file.
///
/// The section is wrapped with HTML comment markers:
/// ```text
/// <!-- seshat:start -->
/// …content…
/// <!-- seshat:end -->
/// ```
///
/// Algorithm:
/// 1. File absent → create with the seshat section.
/// 2. File present, no markers → append the section.
/// 3. File present, markers found → replace content between markers.
///
/// `content` is the raw text to embed (should NOT include the markers themselves —
/// they are added by this function). Pass [`AGENTS_MD_CONTENT`] for standard use.
pub fn upsert_instructions(
    path: &Path,
    content: &str,
    dry_run: bool,
) -> Result<UpsertResult, CliError> {
    if dry_run {
        return Ok(UpsertResult::DryRun);
    }

    let section = format!("{MARKER_START}\n{content}\n{MARKER_END}\n");

    if !path.exists() {
        // Case 1: file does not exist — create it.
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| CliError::IoWithPath {
                message: format!("failed to create directory: {e}"),
                path: parent.to_path_buf(),
            })?;
        }
        fs::write(path, &section).map_err(|e| CliError::IoWithPath {
            message: format!("failed to create instruction file: {e}"),
            path: path.to_path_buf(),
        })?;
        return Ok(UpsertResult::Created);
    }

    let existing = fs::read_to_string(path).map_err(|e| CliError::IoWithPath {
        message: format!("failed to read instruction file: {e}"),
        path: path.to_path_buf(),
    })?;

    if let Some(start_pos) = existing.find(MARKER_START) {
        // Case 3: markers present — replace between them (inclusive).
        let end_pos = existing
            .find(MARKER_END)
            .map(|p| p + MARKER_END.len())
            .unwrap_or(existing.len());

        // Consume a trailing newline if present after the end marker.
        let end_pos = if existing.as_bytes().get(end_pos) == Some(&b'\n') {
            end_pos + 1
        } else {
            end_pos
        };

        // Preserve leading newline before marker if the file doesn't start with it.
        let prefix = &existing[..start_pos];
        let suffix = &existing[end_pos..];
        let new_content = format!("{prefix}{section}{suffix}");

        fs::write(path, new_content).map_err(|e| CliError::IoWithPath {
            message: format!("failed to update instruction file: {e}"),
            path: path.to_path_buf(),
        })?;
        Ok(UpsertResult::Updated)
    } else {
        // Case 2: no markers — append section.
        let separator = if existing.ends_with('\n') || existing.is_empty() {
            "\n"
        } else {
            "\n\n"
        };
        let new_content = format!("{existing}{separator}{section}");
        fs::write(path, new_content).map_err(|e| CliError::IoWithPath {
            message: format!("failed to append to instruction file: {e}"),
            path: path.to_path_buf(),
        })?;
        Ok(UpsertResult::Appended)
    }
}

/// Install the Seshat skill file into an agent's skills directory.
///
/// `target_dir` should be e.g. `~/.claude/skills/seshat/` or
/// `~/.config/opencode/skills/seshat/`. The function creates the directory if
/// absent and always overwrites `SKILL.md` (versioned via binary release).
pub fn install_skill(
    target_dir: &Path,
    content: &str,
    dry_run: bool,
) -> Result<SkillResult, CliError> {
    if dry_run {
        return Ok(SkillResult::DryRun);
    }

    fs::create_dir_all(target_dir).map_err(|e| CliError::IoWithPath {
        message: format!("failed to create skill directory: {e}"),
        path: target_dir.to_path_buf(),
    })?;

    let skill_path = target_dir.join("SKILL.md");
    fs::write(&skill_path, content).map_err(|e| CliError::IoWithPath {
        message: format!("failed to write skill file: {e}"),
        path: skill_path,
    })?;

    Ok(SkillResult::Installed)
}

/// Install Seshat hooks for Claude Code and register them in `settings.json`.
///
/// Writes two executable scripts to `hooks_dir`:
/// - `seshat-session-start` — soft SessionStart reminder
/// - `seshat-pre-tool` — soft PreToolUse nudge (1 per session)
///
/// Registers both in `settings_path` (typically `~/.claude/settings.json`)
/// under the `"hooks"` key. Idempotent: skips entries already present.
pub fn install_hooks_claude_code(
    hooks_dir: &Path,
    settings_path: &Path,
    dry_run: bool,
) -> Result<(), CliError> {
    if dry_run {
        return Ok(());
    }

    fs::create_dir_all(hooks_dir).map_err(|e| CliError::IoWithPath {
        message: format!("failed to create hooks directory: {e}"),
        path: hooks_dir.to_path_buf(),
    })?;

    // Write hook scripts.
    let session_start_path = hooks_dir.join("seshat-session-start");
    let pre_tool_path = hooks_dir.join("seshat-pre-tool");

    write_executable(&session_start_path, HOOK_SESSION_START)?;
    write_executable(&pre_tool_path, HOOK_PRE_TOOL)?;

    // Register in settings.json.
    let session_start_cmd = session_start_path.to_string_lossy().to_string();
    let pre_tool_cmd = pre_tool_path.to_string_lossy().to_string();

    register_claude_hooks(settings_path, &session_start_cmd, &pre_tool_cmd)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Write `content` to `path` and set executable permissions (Unix only).
fn write_executable(path: &Path, content: &str) -> Result<(), CliError> {
    fs::write(path, content).map_err(|e| CliError::IoWithPath {
        message: format!("failed to write hook script: {e}"),
        path: path.to_path_buf(),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| {
            CliError::IoWithPath {
                message: format!("failed to set executable permission: {e}"),
                path: path.to_path_buf(),
            }
        })?;
    }

    Ok(())
}

/// Idempotently register Seshat hooks in `~/.claude/settings.json`.
///
/// Merges into the existing `"hooks"` object without touching other entries.
/// Uses the hook command path as the idempotency key.
fn register_claude_hooks(
    settings_path: &Path,
    session_start_cmd: &str,
    pre_tool_cmd: &str,
) -> Result<(), CliError> {
    // Read existing settings (or start with empty object).
    let existing = if settings_path.exists() {
        fs::read_to_string(settings_path).map_err(|e| CliError::IoWithPath {
            message: format!("failed to read claude settings: {e}"),
            path: settings_path.to_path_buf(),
        })?
    } else {
        String::from("{}")
    };

    let mut root: serde_json::Value =
        serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));

    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .cloned()
        .unwrap_or_default();

    let mut hooks_obj = hooks;

    // --- PreToolUse ---
    let pre_tool_entry = serde_json::json!({
        "type": "command",
        "command": pre_tool_cmd
    });
    let pre_tool_hook = serde_json::json!({
        "matcher": "Grep|Glob|Read|Search",
        "hooks": [pre_tool_entry.clone()]
    });

    let pre_tool_arr = hooks_obj
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .cloned()
        .unwrap_or_default();

    let mut pre_tool_arr = pre_tool_arr;
    if !hook_command_exists(&pre_tool_arr, pre_tool_cmd) {
        pre_tool_arr.push(pre_tool_hook);
    }
    hooks_obj.insert(
        "PreToolUse".to_string(),
        serde_json::Value::Array(pre_tool_arr),
    );

    // --- SessionStart ---
    let session_matchers = ["startup", "resume", "clear", "compact"];
    let session_arr = hooks_obj
        .entry("SessionStart")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .cloned()
        .unwrap_or_default();

    let mut session_arr = session_arr;
    if !hook_command_exists(&session_arr, session_start_cmd) {
        for matcher in session_matchers {
            session_arr.push(serde_json::json!({
                "matcher": matcher,
                "hooks": [{"type": "command", "command": session_start_cmd}]
            }));
        }
    }
    hooks_obj.insert(
        "SessionStart".to_string(),
        serde_json::Value::Array(session_arr),
    );

    root["hooks"] = serde_json::Value::Object(hooks_obj.into_iter().collect());

    // Write back.
    let json_str = serde_json::to_string_pretty(&root).map_err(|e| CliError::CommandFailed {
        command: "seshat init".to_owned(),
        reason: format!("failed to serialize settings.json: {e}"),
    })?;

    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| CliError::IoWithPath {
            message: format!("failed to create .claude directory: {e}"),
            path: parent.to_path_buf(),
        })?;
    }

    fs::write(settings_path, json_str).map_err(|e| CliError::IoWithPath {
        message: format!("failed to write claude settings: {e}"),
        path: settings_path.to_path_buf(),
    })?;

    Ok(())
}

/// Check if any hook entry in `arr` already contains `cmd` as a command value.
fn hook_command_exists(arr: &[serde_json::Value], cmd: &str) -> bool {
    for entry in arr {
        if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
            for hook in hooks {
                if hook.get("command").and_then(|c| c.as_str()) == Some(cmd) {
                    return true;
                }
            }
        }
    }
    false
}

/// Resolve the Claude home directory (`~/.claude`).
pub fn claude_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Resolve the OpenCode global config directory (`~/.config/opencode`).
pub fn opencode_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|c| c.join("opencode"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    // ── upsert_instructions ──────────────────────────────────────────────

    #[test]
    fn upsert_creates_new_file_when_absent() {
        let dir = tmp();
        let path = dir.path().join("AGENTS.md");
        let result = upsert_instructions(&path, "hello world", false).unwrap();
        assert_eq!(result, UpsertResult::Created);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(MARKER_START));
        assert!(content.contains("hello world"));
        assert!(content.contains(MARKER_END));
    }

    #[test]
    fn upsert_creates_parent_directories() {
        let dir = tmp();
        let path = dir.path().join("nested").join("dir").join("AGENTS.md");
        let result = upsert_instructions(&path, "nested", false).unwrap();
        assert_eq!(result, UpsertResult::Created);
        assert!(path.exists());
    }

    #[test]
    fn upsert_appends_when_no_markers() {
        let dir = tmp();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "# Existing content\n").unwrap();

        let result = upsert_instructions(&path, "new section", false).unwrap();
        assert_eq!(result, UpsertResult::Appended);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Existing content"));
        assert!(content.contains(MARKER_START));
        assert!(content.contains("new section"));
        assert!(content.contains(MARKER_END));
    }

    #[test]
    fn upsert_replaces_between_markers() {
        let dir = tmp();
        let path = dir.path().join("AGENTS.md");
        let initial = format!("# Header\n{MARKER_START}\nold content\n{MARKER_END}\n# Footer\n");
        fs::write(&path, &initial).unwrap();

        let result = upsert_instructions(&path, "new content", false).unwrap();
        assert_eq!(result, UpsertResult::Updated);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Header"), "header preserved");
        assert!(content.contains("# Footer"), "footer preserved");
        assert!(content.contains("new content"), "new content written");
        assert!(!content.contains("old content"), "old content removed");
    }

    #[test]
    fn upsert_idempotent_on_second_run() {
        let dir = tmp();
        let path = dir.path().join("AGENTS.md");

        upsert_instructions(&path, "section content", false).unwrap();
        upsert_instructions(&path, "section content", false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // Only one start marker should be present
        let count = content.matches(MARKER_START).count();
        assert_eq!(count, 1, "exactly one seshat section after two upserts");
    }

    #[test]
    fn upsert_dry_run_does_not_write() {
        let dir = tmp();
        let path = dir.path().join("AGENTS.md");

        let result = upsert_instructions(&path, "content", true).unwrap();
        assert_eq!(result, UpsertResult::DryRun);
        assert!(!path.exists(), "file must not be created in dry-run mode");
    }

    // ── install_skill ────────────────────────────────────────────────────

    #[test]
    fn install_skill_creates_dir_and_file() {
        let dir = tmp();
        let skill_dir = dir.path().join("skills").join("seshat");

        let result = install_skill(&skill_dir, "skill content", false).unwrap();
        assert_eq!(result, SkillResult::Installed);

        let skill_path = skill_dir.join("SKILL.md");
        assert!(skill_path.exists());
        assert_eq!(fs::read_to_string(&skill_path).unwrap(), "skill content");
    }

    #[test]
    fn install_skill_overwrites_existing() {
        let dir = tmp();
        let skill_dir = dir.path().join("skills").join("seshat");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "old content").unwrap();

        install_skill(&skill_dir, "new content", false).unwrap();

        let content = fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn install_skill_dry_run_does_not_write() {
        let dir = tmp();
        let skill_dir = dir.path().join("skills").join("seshat");

        let result = install_skill(&skill_dir, "content", true).unwrap();
        assert_eq!(result, SkillResult::DryRun);
        assert!(!skill_dir.exists());
    }

    // ── install_hooks_claude_code ────────────────────────────────────────

    #[test]
    fn install_hooks_creates_scripts() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();

        assert!(hooks_dir.join("seshat-session-start").exists());
        assert!(hooks_dir.join("seshat-pre-tool").exists());
    }

    #[cfg(unix)]
    #[test]
    fn install_hooks_scripts_are_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();

        let session_meta = fs::metadata(hooks_dir.join("seshat-session-start")).unwrap();
        assert!(
            session_meta.permissions().mode() & 0o111 != 0,
            "must be executable"
        );

        let pre_tool_meta = fs::metadata(hooks_dir.join("seshat-pre-tool")).unwrap();
        assert!(
            pre_tool_meta.permissions().mode() & 0o111 != 0,
            "must be executable"
        );
    }

    #[test]
    fn install_hooks_registers_in_settings_json() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();

        let content = fs::read_to_string(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let hooks = parsed.get("hooks").expect("hooks key");

        assert!(hooks.get("PreToolUse").is_some(), "PreToolUse registered");
        assert!(
            hooks.get("SessionStart").is_some(),
            "SessionStart registered"
        );
    }

    #[test]
    fn install_hooks_idempotent_on_second_run() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();
        install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();

        let content = fs::read_to_string(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        let seshat_entries: Vec<_> = pre_tool
            .iter()
            .filter(|e| {
                e.get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|h| {
                        h.iter().any(|hk| {
                            hk.get("command")
                                .and_then(|c| c.as_str())
                                .map(|c| c.contains("seshat-pre-tool"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(seshat_entries.len(), 1, "only one seshat pre-tool entry");
    }

    #[test]
    fn install_hooks_merges_with_existing_settings() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        // Pre-populate with an unrelated hook
        fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"matcher":".*","hooks":[{"type":"command","command":"/usr/local/bin/other-hook"}]}]}}"#,
        )
        .unwrap();

        install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();

        let content = fs::read_to_string(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        // Both original and seshat entries must be present
        assert!(pre_tool.len() >= 2, "existing hooks preserved");
        assert!(
            content.contains("other-hook"),
            "original hook not overwritten"
        );
        assert!(content.contains("seshat-pre-tool"), "seshat hook added");
    }

    #[test]
    fn install_hooks_dry_run_does_not_write() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        install_hooks_claude_code(&hooks_dir, &settings, true).unwrap();

        assert!(
            !hooks_dir.exists(),
            "hooks dir must not be created in dry-run"
        );
        assert!(
            !settings.exists(),
            "settings must not be written in dry-run"
        );
    }

    // ── hook_command_exists ──────────────────────────────────────────────

    #[test]
    fn hook_command_exists_returns_true_when_found() {
        let arr = vec![serde_json::json!({
            "matcher": "startup",
            "hooks": [{"type": "command", "command": "/path/to/seshat-session-start"}]
        })];
        assert!(hook_command_exists(&arr, "/path/to/seshat-session-start"));
    }

    #[test]
    fn hook_command_exists_returns_false_when_absent() {
        let arr = vec![serde_json::json!({
            "matcher": "startup",
            "hooks": [{"type": "command", "command": "/other/hook"}]
        })];
        assert!(!hook_command_exists(&arr, "/seshat-session-start"));
    }
}
