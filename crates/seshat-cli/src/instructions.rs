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
        // Guard: MARKER_END must follow MARKER_START; if absent the file is
        // corrupted (e.g. interrupted write). Fail explicitly instead of
        // silently truncating the suffix.
        let end_marker_pos = existing
            .find(MARKER_END)
            .ok_or_else(|| CliError::CommandFailed {
                command: "seshat init".to_owned(),
                reason: format!(
                    "{} contains `<!-- seshat:start -->` but no matching \
                     `<!-- seshat:end -->`. \
                     Fix the file manually and retry.",
                    path.display()
                ),
            })?;

        // Verify ordering: end marker must come after start marker.
        if end_marker_pos < start_pos {
            return Err(CliError::CommandFailed {
                command: "seshat init".to_owned(),
                reason: format!(
                    "{} has `<!-- seshat:end -->` before `<!-- seshat:start -->`. \
                     Fix the file manually and retry.",
                    path.display()
                ),
            });
        }

        let end_pos = end_marker_pos + MARKER_END.len();

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

    // Fail explicitly if the file exists but is not valid JSON — we must not
    // silently overwrite user settings.
    let mut root: serde_json::Value =
        serde_json::from_str(&existing).map_err(|e| CliError::CommandFailed {
            command: "seshat init".to_owned(),
            reason: format!(
                "settings.json at {} is not valid JSON: {e}. \
                 Fix or remove it and retry.",
                settings_path.display()
            ),
        })?;

    // Ensure root is an object; if it isn't (e.g. bare array/string), fail.
    if !root.is_object() {
        return Err(CliError::CommandFailed {
            command: "seshat init".to_owned(),
            reason: format!(
                "settings.json at {} is not a JSON object.",
                settings_path.display()
            ),
        });
    }

    // Work directly on root to avoid clone-and-reinsert losing unknown keys.
    // Ensure root["hooks"] is an object.
    {
        let hooks_entry = root
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));
        if !hooks_entry.is_object() {
            *hooks_entry = serde_json::json!({});
        }
    }

    // --- PreToolUse ---
    let pre_tool_hook = serde_json::json!({
        "matcher": "Grep|Glob|Read|Search",
        "hooks": [{"type": "command", "command": pre_tool_cmd}]
    });

    {
        let pre_tool_arr = root["hooks"]["PreToolUse"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !hook_command_exists(&pre_tool_arr, pre_tool_cmd) {
            let mut arr = pre_tool_arr;
            arr.push(pre_tool_hook);
            root["hooks"]["PreToolUse"] = serde_json::Value::Array(arr);
        } else {
            // Ensure the key exists even if we didn't push.
            root["hooks"]
                .as_object_mut()
                .unwrap()
                .entry("PreToolUse")
                .or_insert_with(|| serde_json::json!([]));
        }
    }

    // --- SessionStart ---
    let session_matchers = ["startup", "resume", "clear", "compact"];
    {
        let session_arr = root["hooks"]["SessionStart"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !hook_command_exists(&session_arr, session_start_cmd) {
            let mut arr = session_arr;
            for matcher in session_matchers {
                arr.push(serde_json::json!({
                    "matcher": matcher,
                    "hooks": [{"type": "command", "command": session_start_cmd}]
                }));
            }
            root["hooks"]["SessionStart"] = serde_json::Value::Array(arr);
        } else {
            root["hooks"]
                .as_object_mut()
                .unwrap()
                .entry("SessionStart")
                .or_insert_with(|| serde_json::json!([]));
        }
    }

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

/// Resolve the OpenCode global config directory.
///
/// OpenCode follows XDG conventions on all platforms: it reads
/// `$XDG_CONFIG_HOME/opencode` when the env var is set, and falls back to
/// `~/.config/opencode` otherwise — including on macOS where
/// `dirs::config_dir()` would incorrectly return `~/Library/Application Support/`.
pub fn opencode_config_dir() -> Option<PathBuf> {
    // Respect $XDG_CONFIG_HOME if set and non-empty.
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("opencode"));
        }
    }
    // Default XDG fallback: ~/.config/opencode
    dirs::home_dir().map(|h| h.join(".config").join("opencode"))
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

    // ── P2: unpaired markers ─────────────────────────────────────────────

    #[test]
    fn upsert_errors_on_start_without_end_marker() {
        let dir = tmp();
        let path = dir.path().join("AGENTS.md");
        // File with only the start marker — no end marker.
        fs::write(
            &path,
            format!("# Header\n{MARKER_START}\norphaned content\n"),
        )
        .unwrap();

        let result = upsert_instructions(&path, "new content", false);
        assert!(result.is_err(), "must fail with unpaired start marker");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("seshat:end"),
            "error must mention missing end marker; got: {err_msg}"
        );
    }

    #[test]
    fn upsert_errors_on_end_before_start_marker() {
        let dir = tmp();
        let path = dir.path().join("AGENTS.md");
        // Inverted marker order.
        fs::write(
            &path,
            format!("# Header\n{MARKER_END}\nstuff\n{MARKER_START}\ncontent\n"),
        )
        .unwrap();

        let result = upsert_instructions(&path, "new content", false);
        assert!(result.is_err(), "must fail with inverted markers");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("seshat:end") || err_msg.contains("before"),
            "error must describe ordering issue; got: {err_msg}"
        );
    }

    // ── P3: malformed settings.json ──────────────────────────────────────

    #[test]
    fn install_hooks_errors_on_invalid_json_settings() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        // Write invalid JSON (trailing comma).
        fs::write(&settings, r#"{"hooks": {"bad": true,}}"#).unwrap();

        let result = install_hooks_claude_code(&hooks_dir, &settings, false);
        assert!(result.is_err(), "must fail on malformed settings.json");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not valid JSON") || err_msg.contains("JSON"),
            "error must mention JSON; got: {err_msg}"
        );
    }

    #[test]
    fn install_hooks_preserves_existing_non_hook_settings_keys() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");

        // Pre-populate with unrelated top-level keys AND a hook from another tool.
        fs::write(
            &settings,
            r#"{
  "theme": "dark",
  "fontSize": 14,
  "hooks": {
    "SomeOtherEvent": [{"matcher": ".*", "hooks": [{"type": "command", "command": "/other/tool"}]}]
  }
}"#,
        )
        .unwrap();

        install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();

        let content = fs::read_to_string(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Non-hook top-level keys must survive.
        assert_eq!(parsed["theme"], "dark", "theme key preserved");
        assert_eq!(parsed["fontSize"], 14, "fontSize key preserved");

        // Pre-existing hook event must survive.
        assert!(
            parsed["hooks"]["SomeOtherEvent"].is_array(),
            "SomeOtherEvent hook preserved"
        );
        assert!(
            content.contains("/other/tool"),
            "other tool hook command preserved"
        );

        // Seshat hooks must be present.
        assert!(parsed["hooks"]["PreToolUse"].is_array(), "PreToolUse added");
        assert!(
            parsed["hooks"]["SessionStart"].is_array(),
            "SessionStart added"
        );
    }
}
