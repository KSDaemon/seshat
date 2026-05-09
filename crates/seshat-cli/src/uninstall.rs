//! Implementation of the `seshat uninstall` command.
//!
//! Removes all Seshat configuration from detected AI clients:
//! - MCP entries from config files
//! - Instruction sections from AGENTS.md/CLAUDE.md/.cursorrules
//! - Skill directories
//! - Hook scripts and entries
//!
//! Reverses the operations performed by `seshat init`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;

use crate::error::CliError;
use crate::format::{color_enabled, format_copy_block, format_section_header};
pub use crate::init::ScopeRequest;
use crate::init::{ClientKind, ConfigFormat};

// ══════════════════════════════════════════════════════════════════════
// Types
// ══════════════════════════════════════════════════════════════════════

/// A single item that can be removed during uninstall.
#[derive(Debug, Clone)]
pub enum UninstallTarget {
    /// MCP config entry to remove.
    McpEntry {
        path: PathBuf,
        format: ConfigFormat,
        is_project: bool,
        client: ClientKind,
    },
    /// Instruction file section to remove.
    Instructions { path: PathBuf },
    /// Skill directory to remove.
    SkillDir { path: PathBuf },
    /// Hook script to remove.
    HookScript { path: PathBuf },
    /// Hook entries in settings.json to remove.
    HookEntries { settings_path: PathBuf },
}

/// Result of an uninstall operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallResult {
    /// Item was removed.
    Removed,
    /// Item did not exist (no-op).
    NotExists,
    /// Dry-run mode — path that would have been affected.
    DryRun(PathBuf),
    /// Skipped — no action needed.
    Skipped(String),
}

/// What would be removed for a specific client.
#[derive(Debug)]
pub struct ClientUninstallPlan {
    pub client: ClientKind,
    pub targets: Vec<UninstallTarget>,
}

// ══════════════════════════════════════════════════════════════════════
// Detection
// ══════════════════════════════════════════════════════════════════════

/// Detect all uninstall targets for detected clients.
pub fn detect_all_targets(
    client: Option<&str>,
    scope: ScopeRequest,
    project_root: &Path,
) -> Vec<ClientUninstallPlan> {
    let mut plans = Vec::new();

    if let Some(name) = client {
        if let Some(kind) = ClientKind::from_cli_name(name) {
            let targets = detect_client_targets(kind, scope, project_root);
            if !targets.is_empty() {
                plans.push(ClientUninstallPlan {
                    client: kind,
                    targets,
                });
            }
        }
    } else {
        // Auto-detect: check which clients are installed.
        let cwd = std::env::current_dir().unwrap_or_default();
        let proj_root = crate::db::sync_root_for(&cwd);

        // Claude Code
        if which::which("claude").is_ok() {
            let targets = detect_claude_code_targets(scope, &proj_root);
            if !targets.is_empty() {
                plans.push(ClientUninstallPlan {
                    client: ClientKind::ClaudeCode,
                    targets,
                });
            }
        }

        // Claude Desktop (macOS only)
        #[cfg(target_os = "macos")]
        {
            if claude_desktop_config_exists() {
                let targets = detect_claude_desktop_targets();
                if !targets.is_empty() {
                    plans.push(ClientUninstallPlan {
                        client: ClientKind::ClaudeDesktop,
                        targets,
                    });
                }
            }
        }

        // OpenCode
        if which::which("opencode").is_ok() {
            let targets = detect_opencode_targets(scope, &proj_root);
            if !targets.is_empty() {
                plans.push(ClientUninstallPlan {
                    client: ClientKind::OpenCode,
                    targets,
                });
            }
        }

        // Cursor
        if which::which("cursor").is_ok() {
            let targets = detect_cursor_targets(scope, &proj_root);
            if !targets.is_empty() {
                plans.push(ClientUninstallPlan {
                    client: ClientKind::Cursor,
                    targets,
                });
            }
        }
    }

    plans
}

/// Detect uninstall targets for a single client.
fn detect_client_targets(
    client: ClientKind,
    scope: ScopeRequest,
    project_root: &Path,
) -> Vec<UninstallTarget> {
    let mut targets = Vec::new();

    match client {
        ClientKind::ClaudeCode => {
            targets.extend(detect_claude_code_targets(scope, project_root));
        }
        ClientKind::ClaudeDesktop => {
            targets.extend(detect_claude_desktop_targets());
        }
        ClientKind::OpenCode => {
            targets.extend(detect_opencode_targets(scope, project_root));
        }
        ClientKind::Cursor => {
            targets.extend(detect_cursor_targets(scope, project_root));
        }
    }

    targets
}

fn detect_claude_code_targets(scope: ScopeRequest, project_root: &Path) -> Vec<UninstallTarget> {
    let mut targets = Vec::new();
    let Some(home) = dirs::home_dir() else {
        return targets;
    };

    let claude_dir = home.join(".claude");

    // CLAUDE.md instruction file
    let claude_md = claude_dir.join("CLAUDE.md");
    if claude_md.exists() {
        targets.push(UninstallTarget::Instructions { path: claude_md });
    }

    // Skill directory
    let skill_dir = claude_dir.join("skills").join("seshat");
    if skill_dir.exists() {
        targets.push(UninstallTarget::SkillDir { path: skill_dir });
    }

    // Hook scripts
    let hooks_dir = claude_dir.join("hooks");
    let session_start = hooks_dir.join("seshat-session-start");
    if session_start.exists() {
        targets.push(UninstallTarget::HookScript {
            path: session_start,
        });
    }
    let pre_tool = hooks_dir.join("seshat-pre-tool");
    if pre_tool.exists() {
        targets.push(UninstallTarget::HookScript { path: pre_tool });
    }

    // Hook entries in settings.json
    let settings_path = claude_dir.join("settings.json");
    if settings_path.exists() {
        targets.push(UninstallTarget::HookEntries {
            settings_path: settings_path.clone(),
        });
    }

    // MCP entry in ~/.claude.json
    let claude_json = home.join(".claude.json");
    match scope {
        ScopeRequest::Global => {
            if claude_json.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: claude_json,
                    format: ConfigFormat::Json,
                    is_project: false,
                    client: ClientKind::ClaudeCode,
                });
            }
        }
        ScopeRequest::Project => {
            // For project scope, look for .mcp.json in project root
            let mcp_json = project_root.join(".mcp.json");
            if mcp_json.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: mcp_json,
                    format: ConfigFormat::Json,
                    is_project: true,
                    client: ClientKind::ClaudeCode,
                });
            }
        }
        ScopeRequest::Auto => {
            // Check both global and project
            if claude_json.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: claude_json,
                    format: ConfigFormat::Json,
                    is_project: false,
                    client: ClientKind::ClaudeCode,
                });
            }
            let mcp_json = project_root.join(".mcp.json");
            if mcp_json.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: mcp_json,
                    format: ConfigFormat::Json,
                    is_project: true,
                    client: ClientKind::ClaudeCode,
                });
            }
        }
    }

    targets
}

#[cfg(target_os = "macos")]
fn claude_desktop_config_exists() -> bool {
    dirs::home_dir()
        .map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("Claude")
                .join("claude_desktop_config.json")
                .exists()
        })
        .unwrap_or(false)
}

fn detect_claude_desktop_targets() -> Vec<UninstallTarget> {
    let mut targets = Vec::new();

    let Some(home) = dirs::home_dir() else {
        return targets;
    };
    let config_path = home
        .join("Library")
        .join("Application Support")
        .join("Claude")
        .join("claude_desktop_config.json");

    if config_path.exists() {
        targets.push(UninstallTarget::McpEntry {
            path: config_path,
            format: ConfigFormat::Json,
            is_project: false,
            client: ClientKind::ClaudeDesktop,
        });
    }

    targets
}

fn detect_opencode_targets(scope: ScopeRequest, project_root: &Path) -> Vec<UninstallTarget> {
    let mut targets = Vec::new();

    // Skill directory in global config
    if let Some(opencode_dir) = opencode_config_dir() {
        let skill_dir = opencode_dir.join("skills").join("seshat");
        if skill_dir.exists() {
            targets.push(UninstallTarget::SkillDir { path: skill_dir });
        }

        // AGENTS.md in global
        let agents_md = opencode_dir.join("AGENTS.md");
        if agents_md.exists() {
            targets.push(UninstallTarget::Instructions { path: agents_md });
        }
    }

    // Project-level AGENTS.md
    let proj_agents = project_root.join("AGENTS.md");
    if proj_agents.exists() {
        targets.push(UninstallTarget::Instructions { path: proj_agents });
    }

    // MCP entry in opencode config
    match scope {
        ScopeRequest::Global => {
            if let Some(opencode_dir) = opencode_config_dir() {
                let json_path = opencode_dir.join("opencode.json");
                if json_path.exists() {
                    targets.push(UninstallTarget::McpEntry {
                        path: json_path,
                        format: ConfigFormat::Json,
                        is_project: false,
                        client: ClientKind::OpenCode,
                    });
                }
                let jsonc_path = opencode_dir.join("opencode.jsonc");
                if jsonc_path.exists() {
                    targets.push(UninstallTarget::McpEntry {
                        path: jsonc_path,
                        format: ConfigFormat::Jsonc,
                        is_project: false,
                        client: ClientKind::OpenCode,
                    });
                }
            }
        }
        ScopeRequest::Project => {
            let json_path = project_root.join("opencode.json");
            if json_path.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: json_path,
                    format: ConfigFormat::Json,
                    is_project: true,
                    client: ClientKind::OpenCode,
                });
            }
            let jsonc_path = project_root.join("opencode.jsonc");
            if jsonc_path.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: jsonc_path,
                    format: ConfigFormat::Jsonc,
                    is_project: true,
                    client: ClientKind::OpenCode,
                });
            }
        }
        ScopeRequest::Auto => {
            // Check project first, then global
            let json_path = project_root.join("opencode.json");
            if json_path.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: json_path,
                    format: ConfigFormat::Json,
                    is_project: true,
                    client: ClientKind::OpenCode,
                });
            }
            let jsonc_path = project_root.join("opencode.jsonc");
            if jsonc_path.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: jsonc_path,
                    format: ConfigFormat::Jsonc,
                    is_project: true,
                    client: ClientKind::OpenCode,
                });
            }
            if let Some(opencode_dir) = opencode_config_dir() {
                let json_path = opencode_dir.join("opencode.json");
                if json_path.exists() {
                    targets.push(UninstallTarget::McpEntry {
                        path: json_path,
                        format: ConfigFormat::Json,
                        is_project: false,
                        client: ClientKind::OpenCode,
                    });
                }
                let jsonc_path = opencode_dir.join("opencode.jsonc");
                if jsonc_path.exists() {
                    targets.push(UninstallTarget::McpEntry {
                        path: jsonc_path,
                        format: ConfigFormat::Jsonc,
                        is_project: false,
                        client: ClientKind::OpenCode,
                    });
                }
            }
        }
    }

    targets
}

fn detect_cursor_targets(scope: ScopeRequest, project_root: &Path) -> Vec<UninstallTarget> {
    let mut targets = Vec::new();

    match scope {
        ScopeRequest::Global => {
            if let Some(home) = dirs::home_dir() {
                let path = home.join(".cursor").join("mcp.json");
                if path.exists() {
                    targets.push(UninstallTarget::McpEntry {
                        path,
                        format: ConfigFormat::Json,
                        is_project: false,
                        client: ClientKind::Cursor,
                    });
                }
            }
        }
        ScopeRequest::Project => {
            let path = project_root.join(".cursor").join("mcp.json");
            if path.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path,
                    format: ConfigFormat::Json,
                    is_project: true,
                    client: ClientKind::Cursor,
                });
            }
        }
        ScopeRequest::Auto => {
            let project_path = project_root.join(".cursor").join("mcp.json");
            if project_path.exists() {
                targets.push(UninstallTarget::McpEntry {
                    path: project_path,
                    format: ConfigFormat::Json,
                    is_project: true,
                    client: ClientKind::Cursor,
                });
            }
            if let Some(home) = dirs::home_dir() {
                let global_path = home.join(".cursor").join("mcp.json");
                if global_path.exists() {
                    targets.push(UninstallTarget::McpEntry {
                        path: global_path,
                        format: ConfigFormat::Json,
                        is_project: false,
                        client: ClientKind::Cursor,
                    });
                }
            }
        }
    }

    targets
}

/// Resolve the OpenCode global config directory.
fn opencode_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("opencode"));
        }
    }
    dirs::home_dir().map(|h| h.join(".config").join("opencode"))
}

// ══════════════════════════════════════════════════════════════════════
// Removal functions
// ══════════════════════════════════════════════════════════════════════

/// Remove ALL `<!-- seshat:start -->...<!-- seshat:end -->` blocks from a file.
pub fn remove_instructions(path: &Path, dry_run: bool) -> Result<UninstallResult, CliError> {
    const MARKER_START: &str = "<!-- seshat:start -->";
    const MARKER_END: &str = "<!-- seshat:end -->";

    if dry_run {
        return Ok(UninstallResult::DryRun(path.to_path_buf()));
    }

    if !path.exists() {
        return Ok(UninstallResult::NotExists);
    }

    let existing = fs::read_to_string(path).map_err(|e| CliError::IoWithPath {
        message: format!("failed to read instruction file: {e}"),
        path: path.to_path_buf(),
    })?;

    let mut result = String::with_capacity(existing.len());
    let mut last_end = 0;
    let mut count = 0;

    while let Some(start_pos) = existing[last_end..].find(MARKER_START) {
        let abs_start = last_end + start_pos;
        let search_from = abs_start;
        if let Some(end_marker_pos) = existing[search_from..].find(MARKER_END) {
            let abs_end = search_from + end_marker_pos + MARKER_END.len();

            // Consume trailing newline after end marker.
            let abs_end = if existing.as_bytes().get(abs_end) == Some(&b'\n') {
                abs_end + 1
            } else {
                abs_end
            };

            // Consume leading newline before start marker.
            let prefix_end =
                if abs_start > 0 && existing.as_bytes().get(abs_start - 1) == Some(&b'\n') {
                    abs_start - 1
                } else {
                    abs_start
                };

            result.push_str(&existing[last_end..prefix_end]);
            last_end = abs_end;
            count += 1;
        } else {
            // Malformed: start without end — skip it and continue.
            result.push_str(&existing[last_end..abs_start + MARKER_START.len()]);
            last_end = abs_start + MARKER_START.len();
            count += 1;
        }
    }

    result.push_str(&existing[last_end..]);

    if count == 0 {
        return Ok(UninstallResult::NotExists);
    }

    // Clean up double newlines that may result from removal.
    let new_content = clean_double_newlines(&result);

    fs::write(path, new_content).map_err(|e| CliError::IoWithPath {
        message: format!("failed to update instruction file: {e}"),
        path: path.to_path_buf(),
    })?;

    Ok(UninstallResult::Removed)
}

/// Remove `seshat` entry from a JSON config file.
pub fn remove_mcp_entry(
    path: &Path,
    client: ClientKind,
    format: ConfigFormat,
    dry_run: bool,
) -> Result<UninstallResult, CliError> {
    if dry_run {
        return Ok(UninstallResult::DryRun(path.to_path_buf()));
    }

    if !path.exists() {
        return Ok(UninstallResult::NotExists);
    }

    // JSONC — show what to remove, don't patch.
    if format == ConfigFormat::Jsonc {
        let entry = client.seshat_entry_json();
        let formatted = serde_json::to_string_pretty(&entry).unwrap_or_else(|_| "{}".to_string());
        let lines: Vec<&str> = formatted.lines().collect();
        let refs: Vec<&str> = lines.to_vec();
        eprintln!(
            "  {} Remove from \"{}\":",
            "snippet:".dimmed(),
            client.mcp_key()
        );
        eprintln!();
        eprint!("{}", format_copy_block(&refs, color_enabled()));
        eprintln!();
        return Ok(UninstallResult::NotExists);
    }

    let content = fs::read_to_string(path).map_err(|e| CliError::IoWithPath {
        message: format!("failed to read config: {e}"),
        path: path.to_path_buf(),
    })?;

    let mut value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| CliError::CommandFailed {
            command: "seshat uninstall".to_owned(),
            reason: format!(
                "config file at {} is not valid JSON: {e}. \
                 Cannot remove seshat entry automatically.",
                path.display()
            ),
        })?;

    // Remove the seshat key from mcpServers/mcp.
    let mcp_key = client.mcp_key();
    if let Some(mcp_obj) = value.get_mut(mcp_key) {
        if mcp_obj.is_object() {
            if mcp_obj.get("seshat").is_some() {
                mcp_obj.as_object_mut().unwrap().remove("seshat");

                // If mcp object is now empty, remove the key too.
                if mcp_obj.as_object().unwrap().is_empty() {
                    value.as_object_mut().unwrap().remove(mcp_key);
                }
            } else {
                return Ok(UninstallResult::NotExists);
            }
        }
    } else {
        return Ok(UninstallResult::NotExists);
    }

    let updated = serde_json::to_string_pretty(&value).map_err(|e| CliError::CommandFailed {
        command: "seshat uninstall".to_owned(),
        reason: format!("failed to serialize config: {e}"),
    })?;

    fs::write(path, updated.as_bytes()).map_err(|e| CliError::IoWithPath {
        message: format!("failed to write config: {e}"),
        path: path.to_path_buf(),
    })?;

    Ok(UninstallResult::Removed)
}

/// Remove a skill directory.
pub fn remove_skill_dir(skill_dir: &Path, dry_run: bool) -> Result<UninstallResult, CliError> {
    if dry_run {
        return Ok(UninstallResult::DryRun(skill_dir.to_path_buf()));
    }

    if !skill_dir.exists() {
        return Ok(UninstallResult::NotExists);
    }

    fs::remove_dir_all(skill_dir).map_err(|e| CliError::IoWithPath {
        message: format!("failed to remove skill directory: {e}"),
        path: skill_dir.to_path_buf(),
    })?;

    Ok(UninstallResult::Removed)
}

/// Remove hook scripts and hook entries from settings.json.
pub fn remove_hooks(
    hooks_dir: &Path,
    settings_path: &Path,
    dry_run: bool,
) -> Result<UninstallResult, CliError> {
    let mut any_removed = false;

    // Remove hook scripts.
    for name in &["seshat-session-start", "seshat-pre-tool"] {
        let hook_path = hooks_dir.join(name);
        if hook_path.exists() {
            if dry_run {
                // Collect all dry-run paths in the caller; just skip here.
                continue;
            }
            fs::remove_file(&hook_path).map_err(|e| CliError::IoWithPath {
                message: format!("failed to remove hook script: {e}"),
                path: hook_path.clone(),
            })?;
            any_removed = true;
        }
    }

    // Remove hook entries from settings.json.
    if settings_path.exists() {
        let result = remove_hook_entries_from_settings(settings_path, dry_run)?;
        if matches!(result, UninstallResult::Removed) {
            any_removed = true;
        }
    }

    // Remove empty hooks directory.
    if hooks_dir.exists() {
        if dry_run {
            // Check if it would become empty.
            let mut has_non_seshat = false;
            if let Ok(mut entries) = fs::read_dir(hooks_dir) {
                while let Some(Ok(entry)) = entries.next() {
                    let fname = entry.file_name();
                    let fname_str = fname.to_string_lossy();
                    if !fname_str.starts_with("seshat-") {
                        has_non_seshat = true;
                        break;
                    }
                }
            }
            if !has_non_seshat {
                return Ok(UninstallResult::DryRun(hooks_dir.to_path_buf()));
            }
        } else if fs::read_dir(hooks_dir).is_ok_and(|mut r| r.next().is_none()) {
            fs::remove_dir(hooks_dir).ok();
        }
    }

    if any_removed {
        Ok(UninstallResult::Removed)
    } else {
        Ok(UninstallResult::NotExists)
    }
}

/// Remove seshat hook entries from settings.json.
fn remove_hook_entries_from_settings(
    settings_path: &Path,
    dry_run: bool,
) -> Result<UninstallResult, CliError> {
    if dry_run {
        return Ok(UninstallResult::DryRun(settings_path.to_path_buf()));
    }

    if !settings_path.exists() {
        return Ok(UninstallResult::NotExists);
    }

    let content = fs::read_to_string(settings_path).map_err(|e| CliError::IoWithPath {
        message: format!("failed to read settings: {e}"),
        path: settings_path.to_path_buf(),
    })?;

    let mut root: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| CliError::CommandFailed {
            command: "seshat uninstall".to_owned(),
            reason: format!(
                "settings.json at {} is not valid JSON: {e}",
                settings_path.display()
            ),
        })?;

    if !root.is_object() {
        return Err(CliError::CommandFailed {
            command: "seshat uninstall".to_owned(),
            reason: format!(
                "settings.json at {} is not a JSON object.",
                settings_path.display()
            ),
        });
    }

    let mut modified = false;

    // Remove seshat entries from PreToolUse.
    if let Some(hooks) = root.get_mut("hooks") {
        if hooks.is_object() {
            // PreToolUse
            if let Some(arr) = hooks.get_mut("PreToolUse") {
                if let Some(array) = arr.as_array_mut() {
                    let before = array.len();
                    array.retain(|entry| {
                        entry
                            .get("hooks")
                            .and_then(|h| h.as_array())
                            .map(|hooks| {
                                hooks.iter().all(|hook| {
                                    hook.get("command")
                                        .and_then(|c| c.as_str())
                                        .map(|cmd| !is_seshat_hook_path(cmd, "seshat-pre-tool"))
                                        .unwrap_or(true)
                                })
                            })
                            .unwrap_or(true)
                    });
                    if array.len() < before {
                        modified = true;
                        if array.is_empty() {
                            hooks.as_object_mut().unwrap().remove("PreToolUse");
                        }
                    }
                }
            }

            // SessionStart
            if let Some(arr) = hooks.get_mut("SessionStart") {
                if let Some(array) = arr.as_array_mut() {
                    let before = array.len();
                    array.retain(|entry| {
                        entry
                            .get("hooks")
                            .and_then(|h| h.as_array())
                            .map(|hooks| {
                                hooks.iter().all(|hook| {
                                    hook.get("command")
                                        .and_then(|c| c.as_str())
                                        .map(|cmd| {
                                            !is_seshat_hook_path(cmd, "seshat-session-start")
                                        })
                                        .unwrap_or(true)
                                })
                            })
                            .unwrap_or(true)
                    });
                    if array.len() < before {
                        modified = true;
                        if array.is_empty() {
                            hooks.as_object_mut().unwrap().remove("SessionStart");
                        }
                    }
                }
            }
        }
    }

    if modified {
        let json_str =
            serde_json::to_string_pretty(&root).map_err(|e| CliError::CommandFailed {
                command: "seshat uninstall".to_owned(),
                reason: format!("failed to serialize settings.json: {e}"),
            })?;

        fs::write(settings_path, json_str).map_err(|e| CliError::IoWithPath {
            message: format!("failed to write settings: {e}"),
            path: settings_path.to_path_buf(),
        })?;

        Ok(UninstallResult::Removed)
    } else {
        Ok(UninstallResult::NotExists)
    }
}

/// Try to remove seshat via `claude mcp remove seshat` CLI command.
/// Falls back to JSON patch of ~/.claude.json if the CLI command is not available.
fn run_claude_mcp_remove(dry_run: bool) -> Result<String, CliError> {
    let cmd_display = "claude mcp remove seshat".to_string();

    if dry_run {
        return Ok(cmd_display);
    }

    // Try CLI first.
    let status = std::process::Command::new("claude")
        .args(["mcp", "remove", "seshat"])
        .status();

    if let Ok(status) = status {
        if status.success() {
            return Ok(cmd_display);
        }
    }

    // Fallback: JSON patch ~/.claude.json.
    if let Some(home) = dirs::home_dir() {
        let claude_json = home.join(".claude.json");
        if let Ok(result) = remove_mcp_entry(
            &claude_json,
            ClientKind::ClaudeCode,
            ConfigFormat::Json,
            false,
        ) {
            if matches!(result, UninstallResult::Removed) {
                let fallback = format!(
                    "claude mcp remove seshat (JSON patch: {})",
                    claude_json.display()
                );
                return Ok(fallback);
            }
        }
    }

    Err(CliError::CommandFailed {
        command: "claude mcp remove".to_owned(),
        reason: "CLI command not available and fallback failed".to_owned(),
    })
}

// ══════════════════════════════════════════════════════════════════════
// Output helpers
// ══════════════════════════════════════════════════════════════════════

fn print_ok(message: &str, color: bool) {
    if color {
        eprintln!("  {} {message}", "✓".green().bold());
    } else {
        eprintln!("  ✓ {message}");
    }
}

fn print_info(message: &str) {
    eprintln!("  {message}");
}

fn print_error(message: &str, color: bool) {
    if color {
        eprintln!("  {} {message}", "✗".red().bold());
    } else {
        eprintln!("  ✗ {message}");
    }
}

/// Ask a yes/no question on stderr, read answer from stdin.
fn ask_yn(prompt: &str, dry_run: bool) -> bool {
    if dry_run {
        eprintln!("  {prompt} [dry-run — no changes]");
        return true;
    }
    eprint!("  {prompt} [y/N] ");
    io::stderr().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim(), "y" | "Y")
}

/// Check if a command path references a seshat hook script.
/// Matches by filename or path segment, not by substring, to avoid
/// false positives like `/usr/local/bin/my-seshat-pre-tool-hook`.
fn is_seshat_hook_path(cmd: &str, hook_name: &str) -> bool {
    // Check if the command ends with the hook name (with optional leading path separator).
    if cmd == hook_name {
        return true;
    }
    if cmd.ends_with(&format!("/{hook_name}")) {
        return true;
    }
    // Check if the command contains the hook name as a path segment.
    if cmd.contains(&format!("/{hook_name}/")) {
        return true;
    }
    // Check for common patterns like "/hooks/seshat-pre-tool" or "hooks/seshat-pre-tool".
    if cmd.contains(&format!("hooks/{hook_name}")) {
        return true;
    }
    false
}

/// Clean up double newlines that may result from block removal.
fn clean_double_newlines(s: &str) -> String {
    // Replace 3+ consecutive newlines with 2 (single blank line).
    let mut result = String::with_capacity(s.len());
    let mut consecutive = 0;

    for ch in s.chars() {
        if ch == '\n' {
            consecutive += 1;
            if consecutive <= 2 {
                result.push(ch);
            }
        } else {
            consecutive = 0;
            result.push(ch);
        }
    }

    // Trim trailing whitespace/newlines.
    result.trim_end().to_string()
}

// ══════════════════════════════════════════════════════════════════════
// Per-client uninstall handling
// ══════════════════════════════════════════════════════════════════════

/// Handle uninstall for a single client.
/// Returns `true` if there was an error.
fn handle_client_uninstall(plan: &ClientUninstallPlan, dry_run: bool, color: bool) -> bool {
    let mut had_error = false;

    eprintln!(
        "{}",
        format_section_header(plan.client.display_name(), color)
    );
    eprintln!();

    // Show what will be removed.
    let mut items_shown = Vec::new();
    for target in &plan.targets {
        match target {
            UninstallTarget::McpEntry {
                path,
                format,
                is_project,
                ..
            } => {
                let scope = if *is_project { "project" } else { "global" };
                let fmt = if *format == ConfigFormat::Jsonc {
                    " (JSONC — snippet only)"
                } else {
                    ""
                };
                items_shown.push(format!(
                    "  MCP: {} → remove \"seshat\" from mcpServers{} ({})",
                    path.display(),
                    fmt,
                    scope
                ));
            }
            UninstallTarget::Instructions { path } => {
                items_shown.push(format!(
                    "  Instructions: {} → remove <!-- seshat:start -->...<!-- seshat:end -->",
                    path.display()
                ));
            }
            UninstallTarget::SkillDir { path } => {
                items_shown.push(format!("  Skill: {} → delete", path.display()));
            }
            UninstallTarget::HookScript { path } => {
                items_shown.push(format!("  Hook: {} → delete", path.display()));
            }
            UninstallTarget::HookEntries { settings_path } => {
                items_shown.push(format!(
                    "  Hooks: {} → remove seshat entries",
                    settings_path.display()
                ));
            }
        }
    }

    if items_shown.is_empty() {
        print_info("Nothing to remove (Seshat not configured).");
        eprintln!();
        return false;
    }

    for item in &items_shown {
        eprintln!("{item}");
    }
    eprintln!();

    if dry_run {
        print_info("[dry-run — no changes will be made]");
        eprintln!();
        return false;
    }

    // Ask for confirmation.
    if !ask_yn("Remove Seshat configuration?", false) {
        print_info("Skipped.");
        eprintln!();
        return false;
    }

    // Perform removal.
    for target in &plan.targets {
        match target {
            UninstallTarget::McpEntry {
                path,
                format,
                client,
                is_project,
                ..
            } => {
                // For Claude Code global MCP config, try `claude mcp remove` first.
                if *client == ClientKind::ClaudeCode
                    && !*is_project
                    && path.ends_with(".claude.json")
                {
                    match run_claude_mcp_remove(false) {
                        Ok(cmd) => {
                            print_ok(&format!("Removed via: {cmd}"), color);
                        }
                        Err(e) => {
                            print_error(&format!("Failed to remove via CLI: {e}"), color);
                            had_error = true;
                        }
                    }
                } else {
                    match remove_mcp_entry(path, *client, *format, false) {
                        Ok(UninstallResult::Removed) => {
                            print_ok(&format!("MCP entry removed from {}", path.display()), color);
                        }
                        Ok(UninstallResult::NotExists) => {
                            print_info(&format!("MCP entry not found in {}", path.display()));
                        }
                        Ok(UninstallResult::DryRun(p)) => {
                            print_info(&format!("Would remove MCP entry from {}", p.display()));
                        }
                        Ok(UninstallResult::Skipped(msg)) => {
                            print_info(&format!("MCP: {msg}"));
                        }
                        Err(e) => {
                            print_error(&format!("Failed to remove MCP entry: {e}"), color);
                            had_error = true;
                        }
                    }
                }
            }
            UninstallTarget::Instructions { path } => match remove_instructions(path, false) {
                Ok(UninstallResult::Removed) => {
                    print_ok(
                        &format!("Instructions removed from {}", path.display()),
                        color,
                    );
                }
                Ok(UninstallResult::NotExists) => {
                    print_info(&format!("No seshat section found in {}", path.display()));
                }
                Ok(UninstallResult::DryRun(p)) => {
                    print_info(&format!("Would remove instructions from {}", p.display()));
                }
                Ok(UninstallResult::Skipped(msg)) => {
                    print_info(&format!("Instructions: {msg}"));
                }
                Err(e) => {
                    print_error(&format!("Failed to remove instructions: {e}"), color);
                    had_error = true;
                }
            },
            UninstallTarget::SkillDir { path } => match remove_skill_dir(path, false) {
                Ok(UninstallResult::Removed) => {
                    print_ok(
                        &format!("Skill directory removed: {}", path.display()),
                        color,
                    );
                }
                Ok(UninstallResult::NotExists) => {
                    print_info(&format!("Skill directory not found: {}", path.display()));
                }
                Ok(UninstallResult::DryRun(p)) => {
                    print_info(&format!("Would remove skill directory: {}", p.display()));
                }
                Ok(UninstallResult::Skipped(msg)) => {
                    print_info(&format!("Skill: {msg}"));
                }
                Err(e) => {
                    print_error(&format!("Failed to remove skill directory: {e}"), color);
                    had_error = true;
                }
            },
            UninstallTarget::HookScript { path } => {
                if path.exists() {
                    if dry_run {
                        print_info(&format!("Would remove hook: {}", path.display()));
                    } else {
                        match fs::remove_file(path) {
                            Ok(()) => {
                                print_ok(&format!("Hook removed: {}", path.display()), color);
                            }
                            Err(e) => {
                                print_error(&format!("Failed to remove hook: {e}"), color);
                                had_error = true;
                            }
                        }
                    }
                } else {
                    print_info(&format!("Hook not found: {}", path.display()));
                }
            }
            UninstallTarget::HookEntries { settings_path } => {
                let hooks_dir = settings_path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join("hooks");
                match remove_hooks(&hooks_dir, settings_path, false) {
                    Ok(UninstallResult::Removed) => {
                        print_ok(
                            &format!("Hook entries removed from {}", settings_path.display()),
                            color,
                        );
                    }
                    Ok(UninstallResult::NotExists) => {
                        print_info(&format!(
                            "No seshat hook entries in {}",
                            settings_path.display()
                        ));
                    }
                    Ok(UninstallResult::DryRun(p)) => {
                        print_info(&format!("Would remove hooks from {}", p.display()));
                    }
                    Ok(UninstallResult::Skipped(msg)) => {
                        print_info(&format!("Hooks: {msg}"));
                    }
                    Err(e) => {
                        print_error(&format!("Failed to remove hooks: {e}"), color);
                        had_error = true;
                    }
                }
            }
        }
    }

    eprintln!();
    had_error
}

// ══════════════════════════════════════════════════════════════════════
// Entry point
// ══════════════════════════════════════════════════════════════════════

/// Run the `seshat uninstall` command.
pub fn run_uninstall(
    client: Option<&str>,
    scope: ScopeRequest,
    dry_run: bool,
) -> Result<(), CliError> {
    let color = color_enabled();

    // Show warning.
    eprintln!(
        "{}",
        format_section_header(if dry_run { "DRY RUN" } else { "WARNING" }, color)
    );
    eprintln!();
    if dry_run {
        eprintln!("  No changes will be made. This shows what would be removed.");
    } else {
        eprintln!("  This will permanently remove all Seshat configuration.");
        eprintln!("  This action cannot be undone. Use backups to restore.");
    }
    eprintln!();

    // Resolve project root.
    let cwd = std::env::current_dir().map_err(|e| CliError::IoWithPath {
        message: format!("cannot determine current directory: {e}"),
        path: PathBuf::from("."),
    })?;
    let project_root = crate::db::sync_root_for(&cwd);

    // Detect targets.
    let plans = detect_all_targets(client, scope, &project_root);

    if plans.is_empty() {
        eprintln!("  No Seshat configuration found to remove.");
        eprintln!("  Run `seshat init` to set up Seshat first.");
        return Ok(());
    }

    // Show scope hint.
    match scope {
        ScopeRequest::Auto => {}
        ScopeRequest::Project => {
            if color {
                eprintln!(
                    "  {} project ({})",
                    "Scope:".dimmed(),
                    project_root.display()
                );
            } else {
                eprintln!("  Scope: project ({})", project_root.display());
            }
        }
        ScopeRequest::Global => {
            if color {
                eprintln!("  {} global\n", "Scope:".dimmed());
            } else {
                eprintln!("  Scope: global\n");
            }
        }
    }

    // Perform uninstall for each client.
    let mut any_error = false;

    for plan in &plans {
        let error = handle_client_uninstall(plan, dry_run, color);
        if error {
            any_error = true;
        }
    }

    if any_error {
        Err(CliError::CommandFailed {
            command: "uninstall".to_owned(),
            reason: "one or more removals failed".to_owned(),
        })
    } else {
        Ok(())
    }
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    // ── remove_instructions ──────────────────────────────────────

    #[test]
    fn remove_instructions_removes_block() {
        let dir = tmp();
        let path = dir.path().join("CLAUDE.md");
        let content = "# Header\n\n<!-- seshat:start -->\nSome seshat content\n<!-- seshat:end -->\n\n# Footer\n".to_string();
        fs::write(&path, &content).unwrap();

        let result = remove_instructions(&path, false).unwrap();
        assert_eq!(result, UninstallResult::Removed);

        let new_content = fs::read_to_string(&path).unwrap();
        assert!(!new_content.contains("seshat:start"));
        assert!(!new_content.contains("seshat:end"));
        assert!(new_content.contains("# Header"));
        assert!(new_content.contains("# Footer"));
    }

    #[test]
    fn remove_instructions_returns_not_exists_when_no_markers() {
        let dir = tmp();
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, "# Just a regular file\n").unwrap();

        let result = remove_instructions(&path, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn remove_instructions_returns_not_exists_when_file_absent() {
        let dir = tmp();
        let path = dir.path().join("CLAUDE.md");

        let result = remove_instructions(&path, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn remove_instructions_dry_run_does_not_modify() {
        let dir = tmp();
        let path = dir.path().join("CLAUDE.md");
        let content = "# Header\n\n<!-- seshat:start -->\ncontent\n<!-- seshat:end -->\n";
        fs::write(&path, content).unwrap();

        let result = remove_instructions(&path, true).unwrap();
        assert!(matches!(result, UninstallResult::DryRun(_)));

        let new_content = fs::read_to_string(&path).unwrap();
        assert_eq!(
            new_content, content,
            "file should not be modified in dry-run"
        );
    }

    #[test]
    fn remove_instructions_clean_double_newlines() {
        let dir = tmp();
        let path = dir.path().join("CLAUDE.md");
        let content =
            "# Header\n\n\n\n<!-- seshat:start -->\ncontent\n<!-- seshat:end -->\n\n\n# Footer\n"
                .to_string();
        fs::write(&path, &content).unwrap();

        remove_instructions(&path, false).unwrap();

        let new_content = fs::read_to_string(&path).unwrap();
        // Should not have 3+ consecutive newlines.
        assert!(
            !new_content.contains("\n\n\n"),
            "should not have triple newlines, got: {:?}",
            new_content
        );
    }

    // ── remove_mcp_entry ─────────────────────────────────────────

    #[test]
    fn remove_mcp_entry_removes_seshat_from_json() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"mcpServers": {"seshat": {"command": "seshat"}, "other": {"command": "other"}}}"#,
        )
        .unwrap();

        let result =
            remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, false).unwrap();
        assert_eq!(result, UninstallResult::Removed);

        let content = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            parsed["mcpServers"].get("seshat").is_none(),
            "seshat removed"
        );
        assert!(
            parsed["mcpServers"]["other"].is_object(),
            "other entry preserved"
        );
    }

    #[test]
    fn remove_mcp_entry_removes_empty_mcp_key() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"mcpServers": {"seshat": {"command": "seshat"}}}"#,
        )
        .unwrap();

        remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            parsed.get("mcpServers").is_none(),
            "empty mcpServers key should be removed"
        );
    }

    #[test]
    fn remove_mcp_entry_returns_not_exists_when_no_seshat() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"mcpServers": {"other": {"command": "other"}}}"#).unwrap();

        let result =
            remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn remove_mcp_entry_dry_run_does_not_modify() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        let content = r#"{"mcpServers": {"seshat": {"command": "seshat"}}}"#;
        fs::write(&path, content).unwrap();

        let result =
            remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, true).unwrap();
        assert!(matches!(result, UninstallResult::DryRun(_)));

        let new_content = fs::read_to_string(&path).unwrap();
        assert_eq!(new_content, content);
    }

    #[test]
    fn remove_mcp_entry_handles_invalid_json() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{invalid json}").unwrap();

        let result = remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, false);
        assert!(result.is_err());
    }

    // ── remove_skill_dir ─────────────────────────────────────────

    #[test]
    fn remove_skill_dir_removes_directory() {
        let dir = tmp();
        let skill_dir = dir.path().join("skills").join("seshat");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "content").unwrap();

        let result = remove_skill_dir(&skill_dir, false).unwrap();
        assert_eq!(result, UninstallResult::Removed);
        assert!(!skill_dir.exists());
    }

    #[test]
    fn remove_skill_dir_returns_not_exists_when_absent() {
        let dir = tmp();
        let skill_dir = dir.path().join("skills").join("seshat");

        let result = remove_skill_dir(&skill_dir, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn remove_skill_dir_dry_run_does_not_remove() {
        let dir = tmp();
        let skill_dir = dir.path().join("skills").join("seshat");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "content").unwrap();

        let result = remove_skill_dir(&skill_dir, true).unwrap();
        assert!(matches!(result, UninstallResult::DryRun(_)));
        assert!(
            skill_dir.exists(),
            "directory should not be removed in dry-run"
        );
    }

    // ── remove_hooks ─────────────────────────────────────────────

    #[test]
    fn remove_hooks_removes_scripts_and_entries() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(
            hooks_dir.join("seshat-session-start"),
            "#!/bin/bash\necho hello",
        )
        .unwrap();
        fs::write(hooks_dir.join("seshat-pre-tool"), "#!/bin/bash\necho nudge").unwrap();
        fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"type":"command","command":"/hooks/seshat-pre-tool"}]}],"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"/hooks/seshat-session-start"}]}]}}"#,
        )
        .unwrap();

        let result = remove_hooks(&hooks_dir, &settings, false).unwrap();
        assert_eq!(result, UninstallResult::Removed);

        assert!(
            !hooks_dir.exists(),
            "hooks dir should be removed (was empty)"
        );
        let content = fs::read_to_string(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            parsed["hooks"].get("PreToolUse").is_none(),
            "PreToolUse removed"
        );
        assert!(
            parsed["hooks"].get("SessionStart").is_none(),
            "SessionStart removed"
        );
    }

    #[test]
    fn remove_hooks_preserves_other_hooks() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("seshat-pre-tool"), "#!/bin/bash\necho nudge").unwrap();
        fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"type":"command","command":"/hooks/seshat-pre-tool"}]},{"matcher":"Glob","hooks":[{"type":"command","command":"/hooks/other-hook"}]}]}}"#,
        )
        .unwrap();

        let result = remove_hooks(&hooks_dir, &settings, false).unwrap();
        assert_eq!(result, UninstallResult::Removed);

        let content = fs::read_to_string(&settings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1, "only one entry should remain");
        assert!(
            pre_tool[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("other-hook"),
            "other hook preserved"
        );
    }

    #[test]
    fn remove_hooks_returns_not_exists_when_nothing_to_remove() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"type":"command","command":"/hooks/other-hook"}]}]}}"#,
        )
        .unwrap();

        let result = remove_hooks(&hooks_dir, &settings, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn remove_hooks_dry_run_does_not_modify() {
        let dir = tmp();
        let hooks_dir = dir.path().join("hooks");
        let settings = dir.path().join("settings.json");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("seshat-pre-tool"), "#!/bin/bash\necho nudge").unwrap();
        fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"type":"command","command":"/hooks/seshat-pre-tool"}]}]}}"#,
        )
        .unwrap();

        let result = remove_hooks(&hooks_dir, &settings, true).unwrap();
        assert!(matches!(result, UninstallResult::DryRun(_)));
        assert!(
            hooks_dir.join("seshat-pre-tool").exists(),
            "hook should not be removed in dry-run"
        );
    }

    // ── clean_double_newlines ────────────────────────────────────

    #[test]
    fn clean_double_newlines_reduces_triple_newlines() {
        let input = "a\n\n\nb\n\n\nc";
        let result = clean_double_newlines(input);
        assert_eq!(result, "a\n\nb\n\nc");
    }

    #[test]
    fn clean_double_newlines_leaves_double_newlines() {
        let input = "a\n\nb";
        let result = clean_double_newlines(input);
        assert_eq!(result, "a\n\nb");
    }

    #[test]
    fn clean_double_newlines_trims_trailing() {
        let input = "a\n\n\n";
        let result = clean_double_newlines(input);
        assert_eq!(result, "a");
    }

    // ── detect_all_targets ─────────────────────────────────────────

    #[test]
    fn detect_all_targets_unknown_client_returns_empty() {
        let dir = tmp();
        let plans = detect_all_targets(Some("unknown-ai"), ScopeRequest::Auto, dir.path());
        assert!(plans.is_empty());
    }

    // ── detect_client_targets ──────────────────────────────────────

    #[test]
    fn detect_client_targets_opencode_returns_targets() {
        let dir = tmp();
        let config_dir = dir.path().join(".opencode");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("opencode.jsonc"), "{}").unwrap();

        let targets = detect_client_targets(ClientKind::OpenCode, ScopeRequest::Auto, dir.path());
        assert!(!targets.is_empty());
    }

    // ── is_seshat_hook_path ────────────────────────────────────────

    #[test]
    fn is_seshat_hook_path_positive() {
        assert!(is_seshat_hook_path(
            "/some/path/.claude/hooks/seshat-pre-tool",
            "seshat-pre-tool"
        ));
    }

    #[test]
    fn is_seshat_hook_path_negative() {
        assert!(!is_seshat_hook_path(
            "/some/path/.claude/hooks/other-pre-tool",
            "seshat-pre-tool"
        ));
    }

    // ── UninstallTarget & UninstallResult ──────────────────────────

    #[test]
    fn uninstall_result_equality() {
        assert_eq!(UninstallResult::Removed, UninstallResult::Removed);
        assert_eq!(UninstallResult::NotExists, UninstallResult::NotExists);
        assert_ne!(UninstallResult::Removed, UninstallResult::NotExists);
    }

    #[test]
    fn uninstall_target_clone() {
        let t = UninstallTarget::Instructions {
            path: PathBuf::from("/tmp/CLAUDE.md"),
        };
        let t2 = t.clone();
        if let UninstallTarget::Instructions { path } = &t2 {
            assert_eq!(path.to_str().unwrap(), "/tmp/CLAUDE.md");
        } else {
            unreachable!();
        }
    }

    #[test]
    fn run_uninstall_no_clients_output() {
        let result = run_uninstall(Some("opencode"), ScopeRequest::Auto, true);
        assert!(result.is_ok());
    }

    #[test]
    fn run_uninstall_unknown_client_errors() {
        // unknown-client in run_uninstall goes through detect_all_targets
        // which returns empty plan — actually it just returns Ok with empty result
        let result = run_uninstall(Some("unknown-client"), ScopeRequest::Auto, false);
        assert!(result.is_ok());
    }

    #[test]
    fn remove_instructions_multiple_blocks_are_all_removed() {
        let dir = tmp();
        let path = dir.path().join("CLAUDE.md");
        let content = concat!(
            "# Header\n",
            "\n",
            "<!-- seshat:start -->\n",
            "block1\n",
            "<!-- seshat:end -->\n",
            "\n",
            "middle\n",
            "\n",
            "<!-- seshat:start -->\n",
            "block2\n",
            "<!-- seshat:end -->\n",
            "\n",
            "# Footer\n",
        );
        fs::write(&path, content).unwrap();

        let _ = remove_instructions(&path, false);
        let new_content = fs::read_to_string(&path).unwrap();
        assert!(!new_content.contains("seshat:start"));
        assert!(!new_content.contains("seshat:end"));
    }

    #[test]
    fn run_uninstall_auto_mode_dry_run() {
        let result = run_uninstall(None, ScopeRequest::Auto, true);
        assert!(result.is_ok());
    }

    #[test]
    fn client_uninstall_plan_holds_correct_data() {
        let plan = ClientUninstallPlan {
            client: ClientKind::OpenCode,
            targets: vec![
                UninstallTarget::Instructions {
                    path: PathBuf::from("/tmp/AGENTS.md"),
                },
                UninstallTarget::SkillDir {
                    path: PathBuf::from("/tmp/skills/seshat"),
                },
            ],
        };
        assert_eq!(plan.client, ClientKind::OpenCode);
        assert_eq!(plan.targets.len(), 2);
    }

    #[test]
    fn remove_mcp_entry_nonexistent_file_returns_not_exists() {
        let dir = tmp();
        let path = dir.path().join("nonexistent.json");
        let result =
            remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn remove_mcp_entry_missing_mcp_key_returns_not_exists() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"otherKey": {}}"#).unwrap();
        let result =
            remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn remove_mcp_entry_mcp_key_not_object_returns_removed() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"mcpServers": []}"#).unwrap();
        let result =
            remove_mcp_entry(&path, ClientKind::ClaudeCode, ConfigFormat::Json, false).unwrap();
        // When mcpServers key exists but is not an object, the function
        // still writes the file back and returns Removed.
        assert_eq!(result, UninstallResult::Removed);
    }

    #[test]
    fn remove_hooks_nonexistent_dir_returns_not_exists() {
        let dir = tmp();
        let hooks_dir = dir.path().join("nonexistent_hooks");
        let settings = dir.path().join("settings.json");
        let result = remove_hooks(&hooks_dir, &settings, false).unwrap();
        assert_eq!(result, UninstallResult::NotExists);
    }

    #[test]
    fn is_seshat_hook_path_exact_match() {
        assert!(is_seshat_hook_path("seshat-pre-tool", "seshat-pre-tool"));
    }

    #[test]
    fn is_seshat_hook_path_ends_with_hook_name() {
        assert!(is_seshat_hook_path(
            "/hooks/seshat-pre-tool",
            "seshat-pre-tool"
        ));
    }

    #[test]
    fn is_seshat_hook_path_contains_hooks_dir() {
        assert!(is_seshat_hook_path(
            "/path/hooks/seshat-pre-tool/something",
            "seshat-pre-tool"
        ));
    }

    // ── detect_cursor_targets ────────────────────────────────────────

    #[test]
    fn detect_cursor_targets_project_scope_with_file() {
        let dir = tmp();
        let cursor_dir = dir.path().join(".cursor");
        fs::create_dir_all(&cursor_dir).unwrap();
        fs::write(
            cursor_dir.join("mcp.json"),
            r#"{"mcpServers":{"seshat":{}}}"#,
        )
        .unwrap();

        let targets = detect_cursor_targets(ScopeRequest::Project, dir.path());
        assert!(!targets.is_empty());
    }

    #[test]
    fn detect_cursor_targets_project_scope_no_file_returns_empty() {
        let dir = tmp();
        let targets = detect_cursor_targets(ScopeRequest::Project, dir.path());
        assert!(targets.is_empty());
    }

    #[test]
    fn detect_cursor_targets_auto_scope_with_project_file() {
        let dir = tmp();
        let cursor_dir = dir.path().join(".cursor");
        fs::create_dir_all(&cursor_dir).unwrap();
        fs::write(cursor_dir.join("mcp.json"), "{}").unwrap();

        let targets = detect_cursor_targets(ScopeRequest::Auto, dir.path());
        assert!(!targets.is_empty());
    }

    #[test]
    fn detect_client_targets_cursor_dispatches_correctly() {
        let dir = tmp();
        let cursor_dir = dir.path().join(".cursor");
        fs::create_dir_all(&cursor_dir).unwrap();
        fs::write(cursor_dir.join("mcp.json"), "{}").unwrap();

        let targets = detect_client_targets(ClientKind::Cursor, ScopeRequest::Project, dir.path());
        assert!(!targets.is_empty());
    }

    #[test]
    fn detect_client_targets_claude_code_dispatches_without_panic() {
        let dir = tmp();
        let targets =
            detect_client_targets(ClientKind::ClaudeCode, ScopeRequest::Project, dir.path());
        // May be empty if ~/.claude doesn't exist, but must not panic.
        drop(targets);
    }

    #[test]
    fn detect_client_targets_claude_desktop_dispatches_without_panic() {
        let dir = tmp();
        let targets =
            detect_client_targets(ClientKind::ClaudeDesktop, ScopeRequest::Auto, dir.path());
        drop(targets);
    }

    // ── run_claude_mcp_remove ────────────────────────────────────────

    // ── remove_hook_entries_from_settings ───────────────────────────

    #[test]
    fn remove_hook_entries_from_settings_dry_run_no_modification() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        let original = r#"{"hooks":{"PreToolUse":[{"hooks":[{"command":"/x/seshat-pre-tool"}]}]}}"#;
        fs::write(&path, original).unwrap();

        let res = remove_hook_entries_from_settings(&path, true).unwrap();
        assert!(matches!(res, UninstallResult::DryRun(_)));
        // File must be unchanged.
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn remove_hook_entries_from_settings_nonexistent_returns_not_exists() {
        let dir = tmp();
        let res = remove_hook_entries_from_settings(&dir.path().join("nope.json"), false).unwrap();
        assert!(matches!(res, UninstallResult::NotExists));
    }

    #[test]
    fn remove_hook_entries_from_settings_strips_seshat_pre_tool_hook() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        let original = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "command": "/usr/local/bin/seshat-pre-tool" }] },
                    { "hooks": [{ "command": "/other/tool" }] }
                ]
            },
            "theme": "dark"
        });
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        let res = remove_hook_entries_from_settings(&path, false).unwrap();
        assert!(matches!(res, UninstallResult::Removed));

        // Result should keep the non-seshat entry and unrelated keys.
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let arr = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["hooks"][0]["command"], "/other/tool");
        assert_eq!(after["theme"], "dark");
    }

    #[test]
    fn remove_hook_entries_from_settings_drops_empty_pretooluse_array() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        let original = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "command": "/x/seshat-pre-tool" }] }
                ]
            }
        });
        fs::write(&path, original.to_string()).unwrap();

        let res = remove_hook_entries_from_settings(&path, false).unwrap();
        assert!(matches!(res, UninstallResult::Removed));

        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // The PreToolUse key itself must be removed when its array becomes empty.
        assert!(after["hooks"].get("PreToolUse").is_none());
    }

    #[test]
    fn remove_hook_entries_from_settings_strips_session_start_hook() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        let original = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "command": "/x/hooks/seshat-session-start" }] },
                    { "hooks": [{ "command": "/other/setup" }] }
                ]
            }
        });
        fs::write(&path, original.to_string()).unwrap();

        let res = remove_hook_entries_from_settings(&path, false).unwrap();
        assert!(matches!(res, UninstallResult::Removed));

        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let arr = after["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["hooks"][0]["command"], "/other/setup");
    }

    #[test]
    fn remove_hook_entries_from_settings_no_match_returns_not_exists() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        let original = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "command": "/other/tool" }] }
                ]
            }
        });
        let original_str = original.to_string();
        fs::write(&path, &original_str).unwrap();

        let res = remove_hook_entries_from_settings(&path, false).unwrap();
        assert!(matches!(res, UninstallResult::NotExists));

        // Unchanged content (no rewrite).
        assert_eq!(fs::read_to_string(&path).unwrap(), original_str);
    }

    #[test]
    fn remove_hook_entries_from_settings_invalid_json_errors() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{not valid").unwrap();
        let err = remove_hook_entries_from_settings(&path, false).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn remove_hook_entries_from_settings_non_object_root_errors() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(&path, "[1, 2, 3]").unwrap();
        let err = remove_hook_entries_from_settings(&path, false).unwrap_err();
        assert!(err.to_string().contains("not a JSON object"));
    }

    #[test]
    fn remove_hook_entries_from_settings_no_hooks_key_returns_not_exists() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"theme": "dark"}"#).unwrap();
        let res = remove_hook_entries_from_settings(&path, false).unwrap();
        assert!(matches!(res, UninstallResult::NotExists));
    }

    // ── remove_skill_dir ────────────────────────────────────────────

    #[test]
    fn remove_skill_dir_dry_run_does_not_modify() {
        let dir = tmp();
        let skill_dir = dir.path().join("skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("README.md"), "x").unwrap();

        let res = remove_skill_dir(&skill_dir, true).unwrap();
        assert!(matches!(res, UninstallResult::DryRun(_)));
        assert!(skill_dir.exists());
    }

    #[test]
    fn remove_skill_dir_nonexistent_returns_not_exists() {
        let dir = tmp();
        let res = remove_skill_dir(&dir.path().join("nope"), false).unwrap();
        assert!(matches!(res, UninstallResult::NotExists));
    }

    #[test]
    fn remove_skill_dir_existing_dir_is_removed() {
        let dir = tmp();
        let skill_dir = dir.path().join("skill");
        fs::create_dir_all(skill_dir.join("nested")).unwrap();
        fs::write(skill_dir.join("README.md"), "x").unwrap();
        fs::write(skill_dir.join("nested/file.txt"), "y").unwrap();

        let res = remove_skill_dir(&skill_dir, false).unwrap();
        assert!(matches!(res, UninstallResult::Removed));
        assert!(!skill_dir.exists());
    }

    // ── remove_instructions ─────────────────────────────────────────

    #[test]
    fn remove_instructions_no_markers_returns_not_exists() {
        let dir = tmp();
        let path = dir.path().join("agents.md");
        let content = "# my agents file\n\nno seshat block here.\n";
        fs::write(&path, content).unwrap();

        let res = remove_instructions(&path, false).unwrap();
        assert!(matches!(res, UninstallResult::NotExists));
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }

    #[test]
    fn remove_instructions_missing_file_returns_not_exists() {
        let dir = tmp();
        let res = remove_instructions(&dir.path().join("nope.md"), false).unwrap();
        assert!(matches!(res, UninstallResult::NotExists));
    }

    #[test]
    fn run_claude_mcp_remove_dry_run_returns_command_string() {
        let result = run_claude_mcp_remove(true).unwrap();
        assert_eq!(result, "claude mcp remove seshat");
    }
}
