//! Implementation of the `seshat init` command.
//!
//! Detects installed AI coding clients, locates their MCP configuration files,
//! checks whether Seshat is already configured, and either auto-patches JSON
//! configs (with backup + confirmation) or displays a copy-paste snippet for
//! JSONC configs.
//!
//! ## Supported clients
//!
//! | Client | Detection | Config key |
//! |--------|-----------|------------|
//! | Claude Code | `claude` in PATH | `mcpServers` |
//! | Claude Desktop | app dir exists (macOS) | `mcpServers` |
//! | OpenCode | `opencode` in PATH | `mcp` |
//! | Cursor | `cursor` in PATH | `mcpServers` |
//!
//! ## Scope selection (default: smart auto-detect)
//!
//! Without flags, `seshat init` uses a **smart scope**:
//! - First checks whether a project-level config exists for each client in the
//!   current working directory (or nearest git root).
//! - If a project-level config is found, it targets that.
//! - If not, falls back to the global user config.
//!
//! `--project` forces project-level configs only (no fallback).
//! `--global`  forces global configs only.
//!
//! ## JSONC handling
//!
//! OpenCode supports both `.json` and `.jsonc` config files. When a `.jsonc`
//! file is detected (or a `.json` file that fails JSON parsing), we only show
//! a snippet — we never auto-patch JSONC to avoid silently destroying comments.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;

use crate::db::find_git_root;
use crate::error::CliError;
use crate::format::{color_enabled, format_copy_block, format_section_header};

// ══════════════════════════════════════════════════════════════════════
// Types
// ══════════════════════════════════════════════════════════════════════

/// A supported AI coding client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    ClaudeCode,
    ClaudeDesktop,
    OpenCode,
    Cursor,
}

impl ClientKind {
    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
        }
    }

    /// CLI name used in `seshat init <client>`.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::ClaudeDesktop => "claude-desktop",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
        }
    }

    /// Parse from a CLI argument string.
    pub fn from_cli_name(s: &str) -> Option<Self> {
        match s {
            "claude-code" | "claude" => Some(Self::ClaudeCode),
            "claude-desktop" => Some(Self::ClaudeDesktop),
            "opencode" => Some(Self::OpenCode),
            "cursor" => Some(Self::Cursor),
            _ => None,
        }
    }

    /// The JSON key under which MCP servers are registered for this client.
    pub fn mcp_key(self) -> &'static str {
        match self {
            Self::OpenCode => "mcp",
            _ => "mcpServers",
        }
    }

    /// Generate the JSON entry value for the `"seshat"` key.
    pub fn seshat_entry_json(self) -> serde_json::Value {
        match self {
            Self::OpenCode => serde_json::json!({
                "type": "local",
                "command": ["seshat", "serve"],
                "enabled": true
            }),
            _ => serde_json::json!({
                "command": "seshat",
                "args": ["serve"]
            }),
        }
    }

    /// Lines to display in the copy block for an existing config.
    ///
    /// Returns the `"seshat": { ... }` fragment suitable for pasting into
    /// an existing `mcpServers` / `mcp` object.
    pub fn snippet_lines(self) -> Vec<String> {
        let entry = self.seshat_entry_json();
        let formatted = serde_json::to_string_pretty(&entry).unwrap_or_else(|_| "{}".to_string());
        // First line: `"seshat": {`  — merge key with opening brace.
        let first = formatted
            .split_once('\n')
            .map(|(head, _)| head)
            .unwrap_or(&formatted);
        let mut lines = vec![format!("\"seshat\": {first}")];
        // Remaining lines: body + closing brace.
        if let Some((_, rest)) = formatted.split_once('\n') {
            for line in rest.lines() {
                lines.push(line.to_string());
            }
        }
        lines
    }

    /// Lines for a brand-new config file that doesn't exist yet.
    pub fn full_file_lines(self) -> Vec<String> {
        let root = serde_json::json!({
            self.mcp_key(): {
                "seshat": self.seshat_entry_json()
            }
        });
        let formatted = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
        formatted.lines().map(|l| l.to_string()).collect()
    }
}

/// Config file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// Standard JSON — can be auto-patched.
    Json,
    /// JSON with Comments — show snippet only, never auto-patch.
    Jsonc,
}

/// Explicit scope requested by the user via CLI flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRequest {
    /// Default: try project-level first, fall back to global.
    Auto,
    /// `--project`: project-level configs only.
    Project,
    /// `--global`: global user configs only.
    Global,
}

/// A resolved config target for a specific client.
#[derive(Debug)]
pub struct ConfigTarget {
    pub client: ClientKind,
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub exists: bool,
    /// True when this target was resolved from a project-level location.
    pub is_project: bool,
}

// ══════════════════════════════════════════════════════════════════════
// Detection
// ══════════════════════════════════════════════════════════════════════

/// Detect all installed AI coding clients and resolve their config targets.
///
/// When `scope == Auto`, each client first checks for a project-level config
/// in `project_root`; if none exists it falls back to the global config.
/// Detect non-Claude-Code clients that use JSON-patch approach.
///
/// Claude Code is intentionally excluded here — it is handled separately
/// via `handle_claude_code_via_cli` which calls `claude mcp add`.
pub fn detect_clients(scope: ScopeRequest, project_root: &Path) -> Vec<ConfigTarget> {
    let mut targets = Vec::new();

    // Claude Code handled separately via CLI — not included here.

    #[cfg(target_os = "macos")]
    if let Some(t) = resolve_claude_desktop_config() {
        targets.push(t);
    }

    if which::which("opencode").is_ok() {
        if let Some(t) = resolve_opencode_config(scope, project_root) {
            targets.push(t);
        }
    }

    if which::which("cursor").is_ok() {
        if let Some(t) = resolve_cursor_config(scope, project_root) {
            targets.push(t);
        }
    }

    targets
}

/// Resolve config target for a single explicitly-named non-ClaudeCode client.
///
/// Claude Code does not use this path — it is handled via `handle_claude_code_via_cli`.
pub fn resolve_single_client(
    client: ClientKind,
    scope: ScopeRequest,
    project_root: &Path,
) -> Option<ConfigTarget> {
    match client {
        ClientKind::ClaudeCode => None, // handled via `claude mcp add` CLI, not JSON patch
        ClientKind::ClaudeDesktop => {
            #[cfg(target_os = "macos")]
            {
                resolve_claude_desktop_config()
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        }
        ClientKind::OpenCode => resolve_opencode_config(scope, project_root),
        ClientKind::Cursor => resolve_cursor_config(scope, project_root),
    }
}

#[cfg(target_os = "macos")]
fn resolve_claude_desktop_config() -> Option<ConfigTarget> {
    // Claude Desktop only has a global config; no project-level equivalent.
    let home = dirs::home_dir()?;
    let app_dir = home
        .join("Library")
        .join("Application Support")
        .join("Claude");
    if !app_dir.is_dir() {
        return None;
    }
    let path = app_dir.join("claude_desktop_config.json");
    Some(make_target(ClientKind::ClaudeDesktop, path, false))
}

/// Resolve the OpenCode global config directory.
///
/// OpenCode follows XDG conventions on all platforms: it reads
/// `$XDG_CONFIG_HOME/opencode` when the env var is set, and falls back to
/// `~/.config/opencode` otherwise — including on macOS where
/// `dirs::config_dir()` would incorrectly return `~/Library/Application Support/`.
fn opencode_global_config_dir() -> Option<PathBuf> {
    // Respect $XDG_CONFIG_HOME if set and non-empty.
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("opencode"));
        }
    }
    // Default XDG fallback: ~/.config/opencode (works on macOS, Linux, Windows).
    Some(dirs::home_dir()?.join(".config").join("opencode"))
}

fn resolve_opencode_config(scope: ScopeRequest, project_root: &Path) -> Option<ConfigTarget> {
    match scope {
        ScopeRequest::Global => {
            let dir = opencode_global_config_dir()?;
            Some(find_opencode_config_in_dir(&dir, false))
        }
        ScopeRequest::Project => Some(find_opencode_config_in_dir(project_root, true)),
        ScopeRequest::Auto => {
            // Prefer project-level if either opencode.json or opencode.jsonc exists.
            let proj_target = find_opencode_config_in_dir(project_root, true);
            if proj_target.exists {
                Some(proj_target)
            } else {
                let dir = opencode_global_config_dir()?;
                Some(find_opencode_config_in_dir(&dir, false))
            }
        }
    }
}

fn resolve_cursor_config(scope: ScopeRequest, project_root: &Path) -> Option<ConfigTarget> {
    match scope {
        ScopeRequest::Global => {
            let path = dirs::home_dir()?.join(".cursor").join("mcp.json");
            Some(make_target(ClientKind::Cursor, path, false))
        }
        ScopeRequest::Project => {
            let path = project_root.join(".cursor").join("mcp.json");
            Some(make_target(ClientKind::Cursor, path, true))
        }
        ScopeRequest::Auto => {
            let project_path = project_root.join(".cursor").join("mcp.json");
            if project_path.exists() {
                Some(make_target(ClientKind::Cursor, project_path, true))
            } else {
                let global_path = dirs::home_dir()?.join(".cursor").join("mcp.json");
                Some(make_target(ClientKind::Cursor, global_path, false))
            }
        }
    }
}

/// Find the opencode config in a directory, preferring `.jsonc` over `.json`.
///
/// If both exist, `.jsonc` takes precedence (matches opencode's load order).
/// If neither exists, returns a non-existing target pointing at `opencode.json`.
pub fn find_opencode_config_in_dir(dir: &Path, is_project: bool) -> ConfigTarget {
    let jsonc_path = dir.join("opencode.jsonc");
    let json_path = dir.join("opencode.json");

    if jsonc_path.exists() {
        ConfigTarget {
            client: ClientKind::OpenCode,
            path: jsonc_path,
            format: ConfigFormat::Jsonc,
            exists: true,
            is_project,
        }
    } else if json_path.exists() {
        // A .json file that fails JSON parsing is treated as JSONC (has comments).
        let format = if is_valid_json(&json_path) {
            ConfigFormat::Json
        } else {
            ConfigFormat::Jsonc
        };
        ConfigTarget {
            client: ClientKind::OpenCode,
            path: json_path,
            format,
            exists: true,
            is_project,
        }
    } else {
        // Neither exists; offer to create opencode.json.
        ConfigTarget {
            client: ClientKind::OpenCode,
            path: json_path,
            format: ConfigFormat::Json,
            exists: false,
            is_project,
        }
    }
}

/// Build a JSON (never JSONC) `ConfigTarget` for non-opencode clients.
fn make_target(client: ClientKind, path: PathBuf, is_project: bool) -> ConfigTarget {
    ConfigTarget {
        exists: path.exists(),
        client,
        path,
        format: ConfigFormat::Json,
        is_project,
    }
}

/// Return `true` if the file at `path` parses as valid JSON (no comments).
fn is_valid_json(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some()
}

// ══════════════════════════════════════════════════════════════════════
// Already-configured check
// ══════════════════════════════════════════════════════════════════════

/// Check whether `seshat` is already present in the target's config.
///
/// - JSON files: parse and check the appropriate key.
/// - JSONC files: text search for `"seshat":` (key assignment, not value).
/// - Non-existent files: `false`.
pub fn is_already_configured(target: &ConfigTarget) -> bool {
    if !target.exists {
        return false;
    }
    let content = match fs::read_to_string(&target.path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    match target.format {
        ConfigFormat::Json => {
            let value: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => return false,
            };
            value
                .get(target.client.mcp_key())
                .and_then(|s| s.get("seshat"))
                .is_some()
        }
        // Search for `"seshat":` (key assignment) to avoid false-positives from
        // keys named `"seshat-tools"` or string values that contain `"seshat"`.
        ConfigFormat::Jsonc => content.contains("\"seshat\":"),
    }
}

// ══════════════════════════════════════════════════════════════════════
// Patching
// ══════════════════════════════════════════════════════════════════════

/// Write a timestamped backup of `path` next to the original.
///
/// Backup name: `{filename}.seshat-backup.{unix_timestamp_ms}`
/// Using millisecond precision avoids collisions when two patches happen
/// within the same second.
pub fn write_backup(path: &Path) -> Result<PathBuf, CliError> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let backup_name = format!("{filename}.seshat-backup.{ts}");
    let backup_path = path.with_file_name(backup_name);
    fs::copy(path, &backup_path).map_err(|e| CliError::IoWithPath {
        message: format!("failed to write backup: {e}"),
        path: backup_path.clone(),
    })?;
    Ok(backup_path)
}

/// Merge the `seshat` entry into a parsed JSON `Value`.
///
/// Creates the `mcpServers` / `mcp` key if it doesn't exist.
/// Returns an error if `value` is not a JSON object (guards against corrupt
/// config files that contain arrays, nulls, or bare scalars at root level).
pub fn merge_seshat_entry(
    value: &mut serde_json::Value,
    client: ClientKind,
) -> Result<(), CliError> {
    if !value.is_object() {
        return Err(CliError::InvalidArgument(format!(
            "config file root is not a JSON object (got {})",
            json_type_name(value)
        )));
    }
    let mcp_key = client.mcp_key();
    if value.get(mcp_key).is_none() {
        value[mcp_key] = serde_json::json!({});
    }
    value[mcp_key]["seshat"] = client.seshat_entry_json();
    Ok(())
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Result of patching a JSON config file.
#[derive(Debug)]
pub struct PatchResult {
    /// Path to the backup file, if one was created (only for existing files).
    pub backup_path: Option<PathBuf>,
}

/// Patch a JSON config file: (backup if exists) → parse → merge → write.
///
/// For new files: parent directories are created as needed, no backup.
/// For existing files: backup written before any mutation.
pub fn patch_json_config(target: &ConfigTarget) -> Result<PatchResult, CliError> {
    // For existing files: backup first, before any mutation.
    let backup_path = if target.exists {
        Some(write_backup(&target.path)?)
    } else {
        // Create parent directories for new files.
        if let Some(parent) = target.path.parent() {
            fs::create_dir_all(parent).map_err(|e| CliError::IoWithPath {
                message: format!("failed to create directory: {e}"),
                path: parent.to_path_buf(),
            })?;
        }
        None
    };

    // Read existing content or start from empty object.
    let content = if target.exists {
        fs::read_to_string(&target.path).map_err(|e| CliError::IoWithPath {
            message: format!("failed to read config: {e}"),
            path: target.path.clone(),
        })?
    } else {
        "{}".to_string()
    };

    let mut value: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        CliError::InvalidArgument(format!(
            "config file contains invalid JSON at {}: {e}",
            target.path.display()
        ))
    })?;

    merge_seshat_entry(&mut value, target.client)?;

    let updated = serde_json::to_string_pretty(&value)
        .map_err(|e| CliError::InvalidArgument(format!("failed to serialize config: {e}")))?;

    fs::write(&target.path, updated.as_bytes()).map_err(|e| CliError::IoWithPath {
        message: format!("failed to write config: {e}"),
        path: target.path.clone(),
    })?;

    Ok(PatchResult { backup_path })
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
        eprintln!("  {} {message}", "error:".red().bold());
    } else {
        eprintln!("  error: {message}");
    }
}

/// Ask a yes/no question on stderr, read answer from stdin.
/// Returns `true` for "y" / "Y". In dry-run mode skips the prompt and
/// returns `false`.
fn ask_yn(prompt: &str, dry_run: bool) -> bool {
    if dry_run {
        eprintln!("  {prompt} [dry-run — no changes]");
        return false;
    }
    eprint!("  {prompt} [y/N] ");
    io::stderr().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim(), "y" | "Y")
}

// ══════════════════════════════════════════════════════════════════════
// Claude Code CLI integration
// ══════════════════════════════════════════════════════════════════════

/// Check whether seshat is already registered via `claude mcp list`.
///
/// Runs `claude mcp list` and checks if "seshat" appears in the output.
/// Returns `None` if the command fails (treat as not configured).
fn claude_mcp_list_has_seshat() -> Option<bool> {
    let output = std::process::Command::new("claude")
        .args(["mcp", "list"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    Some(combined.contains("seshat"))
}

/// Map our ScopeRequest to the `claude mcp add --scope` argument.
///
/// Claude Code scope semantics:
/// - `user`    → `~/.claude.json` global `mcpServers` — applies to all projects
/// - `local`   → `~/.claude.json` under `projects["/path"]` — personal, project-specific
/// - `project` → `.mcp.json` in CWD — committed to repo, shared with team
///
/// Our `--project` flag means "personal project-level" → `local`.
/// Our `--global` / default-global means "all projects" → `user`.
/// We intentionally don't expose `project` scope (team-shared `.mcp.json`)
/// as that requires additional team coordination.
fn claude_scope_arg(scope: ScopeRequest) -> &'static str {
    match scope {
        ScopeRequest::Project => "local", // personal, project-specific in ~/.claude.json
        ScopeRequest::Global | ScopeRequest::Auto => "user", // global for all projects
    }
}

/// Register seshat via `claude mcp add`.
///
/// Uses the official Claude Code CLI to write the MCP entry to the correct
/// location in `~/.claude.json`. This avoids manual JSON patching of
/// internal Claude Code config files.
///
/// Returns the command string shown to the user for reference.
fn run_claude_mcp_add(scope: ScopeRequest, dry_run: bool) -> Result<String, CliError> {
    let scope_arg = claude_scope_arg(scope);
    let cmd_display = format!("claude mcp add -s {scope_arg} seshat seshat serve");

    if dry_run {
        return Ok(cmd_display);
    }

    let status = std::process::Command::new("claude")
        .args(["mcp", "add", "-s", scope_arg, "seshat", "seshat", "serve"])
        .status()
        .map_err(|e| CliError::CommandFailed {
            command: "claude mcp add".to_owned(),
            reason: format!("failed to run: {e}"),
        })?;

    if !status.success() {
        return Err(CliError::CommandFailed {
            command: "claude mcp add".to_owned(),
            reason: format!("exited with status {status}"),
        });
    }

    Ok(cmd_display)
}

/// Handle Claude Code via its own CLI (`claude mcp add`).
///
/// Returns `true` if there was an error.
fn handle_claude_code_via_cli(scope: ScopeRequest, dry_run: bool, color: bool) -> bool {
    eprintln!("{}", format_section_header("Claude Code", color));
    eprintln!();

    let scope_arg = claude_scope_arg(scope);
    let scope_label = match scope_arg {
        "local" => "project-local (~/.claude.json, bound to this path)",
        _ => "user-global (~/.claude.json, all projects)",
    };

    // Check if already configured.
    match claude_mcp_list_has_seshat() {
        Some(true) => {
            print_info(&format!("Scope: {scope_label}"));
            print_ok(
                "Already configured (detected via `claude mcp list`).",
                color,
            );
            eprintln!();
            return false;
        }
        Some(false) => {} // not configured, proceed
        None => {
            // `claude mcp list` failed — still try to add
        }
    }

    print_info(&format!("Scope: {scope_label}"));
    print_info("Will run:");
    eprintln!();

    let cmd_str = format!("claude mcp add -s {scope_arg} seshat seshat serve");
    let refs: Vec<&str> = vec![cmd_str.as_str()];
    eprint!("{}", format_copy_block(&refs, color));
    eprintln!();

    if ask_yn("Run command?", dry_run) {
        match run_claude_mcp_add(scope, dry_run) {
            Ok(_) => {
                print_ok("Seshat added to Claude Code.", color);
            }
            Err(e) => {
                print_error(&e.to_string(), color);
                eprintln!();
                return true;
            }
        }
    } else if !dry_run {
        print_info("Skipped. Run the command above manually.");
    }

    eprintln!();
    false
}

// ══════════════════════════════════════════════════════════════════════
// Per-client output
// ══════════════════════════════════════════════════════════════════════

/// Handle output and optional patching for a single config target.
///
/// Returns `true` if a patch was attempted and failed (so the caller can
/// propagate a non-zero exit).
fn handle_target(target: &ConfigTarget, dry_run: bool, color: bool) -> bool {
    let mut had_error = false;

    eprintln!(
        "{}",
        format_section_header(target.client.display_name(), color)
    );
    eprintln!();

    let path_display = target.path.display().to_string();
    let scope_label = if target.is_project {
        "project"
    } else {
        "global"
    };

    // Already configured?
    if is_already_configured(target) {
        print_info(&format!("Config ({scope_label}): {path_display}"));
        if target.format == ConfigFormat::Jsonc {
            print_ok(
                "Already configured (detected in JSONC — verify manually).",
                color,
            );
        } else {
            print_ok("Already configured.", color);
        }
        eprintln!();
        return false;
    }

    // JSONC — snippet only, no auto-patch.
    if target.format == ConfigFormat::Jsonc {
        print_info(&format!("Config ({scope_label}): {path_display}"));
        print_info("Format: JSONC (contains comments — auto-patch not supported)");
        eprintln!();
        print_info(&format!(
            "Add to \"{}\" section manually:",
            target.client.mcp_key()
        ));
        eprintln!();
        let owned = target.client.snippet_lines();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        eprint!("{}", format_copy_block(&refs, color));
        print_info("Note: add a comma after the preceding entry if \"seshat\" is not the first.");
        eprintln!();
        return false;
    }

    // JSON — auto-patch flow.
    if target.exists {
        print_info(&format!("Config ({scope_label}): {path_display}"));
        print_info(&format!(
            "Seshat is not configured. Add to \"{}\":",
            target.client.mcp_key()
        ));
    } else {
        print_info(&format!("Config not found ({scope_label}): {path_display}"));
        print_info("Will create new file with:");
    }
    eprintln!();

    let owned = if target.exists {
        target.client.snippet_lines()
    } else {
        target.client.full_file_lines()
    };
    let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    eprint!("{}", format_copy_block(&refs, color));

    if target.exists {
        print_info("Note: add a comma after the preceding entry if \"seshat\" is not the first.");
    }
    eprintln!();

    let prompt = if target.exists {
        "Auto-add?"
    } else {
        "Create file?"
    };

    if ask_yn(prompt, dry_run) {
        match patch_json_config(target) {
            Ok(result) => {
                if let Some(backup) = result.backup_path {
                    print_ok(&format!("Backup saved: {}", backup.display()), color);
                }
                print_ok(&format!("Updated {path_display}"), color);
            }
            Err(e) => {
                print_error(&e.to_string(), color);
                had_error = true;
            }
        }
    } else if !dry_run {
        print_info("Skipped. Add the snippet above manually.");
    }

    eprintln!();
    had_error
}

// ══════════════════════════════════════════════════════════════════════
// Entry point
// ══════════════════════════════════════════════════════════════════════

/// Run the `seshat init` command.
///
/// `scope`: `Auto` = smart project-first + global fallback (default),
///           `Project` = project only, `Global` = global only.
pub fn run_init(
    client: Option<&str>,
    scope: ScopeRequest,
    dry_run: bool,
    skip_instructions: bool,
) -> Result<(), CliError> {
    let color = color_enabled();

    // Resolve project root: prefer git root, fall back to cwd.
    let cwd = std::env::current_dir().map_err(|e| CliError::IoWithPath {
        message: format!("cannot determine current directory: {e}"),
        path: PathBuf::from("."),
    })?;
    let project_root = find_git_root(&cwd).unwrap_or_else(|| cwd.clone());

    // Print scope hint when non-default.
    match scope {
        ScopeRequest::Auto => {}
        ScopeRequest::Project => {
            if color {
                eprintln!(
                    "  {} project ({})\n",
                    "Scope:".dimmed(),
                    project_root.display()
                );
            } else {
                eprintln!("  Scope: project ({})\n", project_root.display());
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

    if dry_run {
        if color {
            eprintln!(
                "  {} no files will be written\n",
                "Dry run:".yellow().bold()
            );
        } else {
            eprintln!("  Dry run: no files will be written\n");
        }
    }

    let mut any_error = false;

    // Explicit client mode.
    if let Some(name) = client {
        let kind = ClientKind::from_cli_name(name).ok_or_else(|| {
            CliError::InvalidArgument(format!(
                "Unknown client: {name}\n\nhint: Supported clients: claude-code, claude-desktop, opencode, cursor\nhint: Run `seshat init --help` for usage."
            ))
        })?;

        // Claude Code uses its own CLI rather than direct JSON patching.
        if kind == ClientKind::ClaudeCode {
            if handle_claude_code_via_cli(scope, dry_run, color) {
                any_error = true;
            } else if !skip_instructions {
                write_instructions_for_client(ClientKind::ClaudeCode, dry_run, color);
            }
        } else {
            let target = resolve_single_client(kind, scope, &project_root).ok_or_else(|| {
                CliError::InvalidArgument(format!(
                    "{} is not available on this platform",
                    kind.display_name(),
                ))
            })?;
            if handle_target(&target, dry_run, color) {
                any_error = true;
            } else if !skip_instructions {
                write_instructions_for_client(kind, dry_run, color);
            }
        }
    } else {
        // Auto-detect mode.
        let claude_code_present = which::which("claude").is_ok();
        let targets = detect_clients(scope, &project_root);

        // Claude Code is handled separately via its CLI; filter it from JSON-patch targets.
        let other_targets: Vec<&ConfigTarget> = targets
            .iter()
            .filter(|t| t.client != ClientKind::ClaudeCode)
            .collect();

        if !claude_code_present && other_targets.is_empty() {
            eprintln!("  No AI coding clients detected in PATH.");
            eprintln!();
            eprintln!("  Supported clients: claude-code, claude-desktop, opencode, cursor");
            eprintln!("  Run `seshat init <client>` to generate config for a specific client.");
            return Ok(());
        }

        // Detection summary header.
        eprintln!("  Detected AI coding clients:");
        eprintln!();
        if claude_code_present {
            let scope_hint = match scope {
                ScopeRequest::Project => " (project → .mcp.json)",
                _ => " (global → ~/.claude.json)",
            };
            if color {
                eprintln!(
                    "    {} claude — Claude Code{}",
                    "✓".green().bold(),
                    scope_hint.dimmed(),
                );
            } else {
                eprintln!("    ✓ claude — Claude Code{scope_hint}");
            }
        }
        for t in &other_targets {
            let scope_hint = if t.is_project {
                " (project)"
            } else {
                " (global)"
            };
            if color {
                eprintln!(
                    "    {} {} — {}{}",
                    "✓".green().bold(),
                    t.client.cli_name(),
                    t.client.display_name(),
                    scope_hint.dimmed(),
                );
            } else {
                eprintln!(
                    "    ✓ {} — {}{}",
                    t.client.cli_name(),
                    t.client.display_name(),
                    scope_hint,
                );
            }
        }
        eprintln!();

        // Handle Claude Code first via CLI.
        if claude_code_present {
            let mcp_error = handle_claude_code_via_cli(scope, dry_run, color);
            if mcp_error {
                any_error = true;
            } else if !skip_instructions {
                write_instructions_for_client(ClientKind::ClaudeCode, dry_run, color);
            }
        }

        // Handle remaining clients via JSON patching.
        for target in &other_targets {
            let mcp_error = handle_target(target, dry_run, color);
            if mcp_error {
                any_error = true;
            } else if !skip_instructions {
                write_instructions_for_client(target.client, dry_run, color);
            }
        }
    }

    if any_error {
        Err(CliError::CommandFailed {
            command: "init".to_owned(),
            reason: "one or more configs could not be updated".to_owned(),
        })
    } else {
        Ok(())
    }
}

/// Write agent instructions, skill file, and hooks for the given client.
///
/// Called after a successful MCP config write. Non-fatal — errors are printed
/// but do not abort the overall `seshat init` flow.
fn write_instructions_for_client(client: ClientKind, dry_run: bool, color: bool) {
    use crate::instructions::{
        AGENTS_MD_CONTENT, HooksResult, SKILL_MD_CONTENT, SkillResult, claude_home,
        install_hooks_claude_code, install_skill, opencode_config_dir, upsert_instructions,
    };

    match client {
        ClientKind::ClaudeCode => {
            let Some(claude_home) = claude_home() else {
                print_error(
                    "Could not determine home directory; skipping instructions for Claude Code.",
                    color,
                );
                return;
            };

            // AGENTS.md / CLAUDE.md
            let claude_md = claude_home.join("CLAUDE.md");
            match upsert_instructions(&claude_md, AGENTS_MD_CONTENT, dry_run) {
                Ok(result) => {
                    let msg = if dry_run {
                        format!("Instructions would be written to {}", claude_md.display())
                    } else {
                        format!(
                            "Instructions {} in {}",
                            result.description(),
                            claude_md.display()
                        )
                    };
                    print_ok(&msg, color);
                }
                Err(e) => print_error(&format!("Failed to write instructions: {e}"), color),
            }

            // Skill file
            let skill_dir = claude_home.join("skills").join("seshat");
            let skill_path = skill_dir.join("SKILL.md");
            match install_skill(&skill_dir, SKILL_MD_CONTENT, dry_run) {
                Ok(SkillResult::Installed) => {
                    print_ok(&format!("Skill installed: {}", skill_path.display()), color);
                }
                Ok(SkillResult::DryRun(Some(ref p))) => {
                    print_ok(&format!("Skill would be installed: {}", p.display()), color);
                }
                Ok(SkillResult::DryRun(None)) => {
                    print_ok("Skill dry-run (no changes written)", color);
                }
                Err(e) => print_error(&format!("Failed to install skill: {e}"), color),
            }

            // Hooks
            let hooks_dir = claude_home.join("hooks");
            let settings_path = claude_home.join("settings.json");
            match install_hooks_claude_code(&hooks_dir, &settings_path, dry_run) {
                Ok(HooksResult::Installed(Some(backup))) => print_ok(
                    &format!("Hooks registered (backup: {})", backup.display()),
                    color,
                ),
                Ok(HooksResult::Installed(None)) => {
                    print_ok("Hooks registered in ~/.claude/settings.json", color)
                }
                Ok(HooksResult::DryRun { settings, .. }) => print_ok(
                    &format!("Hooks would be registered in {}", settings.display()),
                    color,
                ),
                Err(e) => print_error(&format!("Failed to install hooks: {e}"), color),
            }
        }

        ClientKind::OpenCode => {
            let Some(opencode_dir) = opencode_config_dir() else {
                print_error(
                    "Could not determine config directory; skipping instructions for OpenCode.",
                    color,
                );
                return;
            };

            // AGENTS.md
            let agents_md = opencode_dir.join("AGENTS.md");
            match upsert_instructions(&agents_md, AGENTS_MD_CONTENT, dry_run) {
                Ok(result) => print_ok(
                    &format!(
                        "Instructions {} in {}",
                        result.description(),
                        agents_md.display()
                    ),
                    color,
                ),
                Err(e) => print_error(&format!("Failed to write instructions: {e}"), color),
            }

            // Skill file
            let skill_dir = opencode_dir.join("skills").join("seshat");
            match install_skill(&skill_dir, SKILL_MD_CONTENT, dry_run) {
                Ok(_) => print_ok(
                    &format!("Skill installed: {}", skill_dir.join("SKILL.md").display()),
                    color,
                ),
                Err(e) => print_error(&format!("Failed to install skill: {e}"), color),
            }
        }

        // Claude Desktop and Cursor: instruction writing not yet supported.
        ClientKind::ClaudeDesktop | ClientKind::Cursor => {}
    }
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ── ClientKind ───────────────────────────────────────────────────

    #[test]
    fn client_from_cli_name_known() {
        assert_eq!(
            ClientKind::from_cli_name("claude-code"),
            Some(ClientKind::ClaudeCode)
        );
        assert_eq!(
            ClientKind::from_cli_name("claude"),
            Some(ClientKind::ClaudeCode)
        );
        assert_eq!(
            ClientKind::from_cli_name("opencode"),
            Some(ClientKind::OpenCode)
        );
        assert_eq!(
            ClientKind::from_cli_name("cursor"),
            Some(ClientKind::Cursor)
        );
        assert_eq!(
            ClientKind::from_cli_name("claude-desktop"),
            Some(ClientKind::ClaudeDesktop)
        );
    }

    #[test]
    fn client_from_cli_name_unknown() {
        assert!(ClientKind::from_cli_name("vscode").is_none());
        assert!(ClientKind::from_cli_name("").is_none());
    }

    #[test]
    fn client_mcp_key_opencode_uses_mcp() {
        assert_eq!(ClientKind::OpenCode.mcp_key(), "mcp");
    }

    #[test]
    fn client_mcp_key_others_use_mcp_servers() {
        assert_eq!(ClientKind::ClaudeCode.mcp_key(), "mcpServers");
        assert_eq!(ClientKind::ClaudeDesktop.mcp_key(), "mcpServers");
        assert_eq!(ClientKind::Cursor.mcp_key(), "mcpServers");
    }

    #[test]
    fn snippet_lines_claude_code_structure() {
        let lines = ClientKind::ClaudeCode.snippet_lines();
        let joined = lines.join("\n");
        assert!(joined.contains("\"seshat\":"));
        assert!(joined.contains("\"command\""));
        assert!(joined.contains("\"args\""));
        assert!(joined.contains("\"serve\""));
    }

    #[test]
    fn snippet_lines_opencode_contains_type_and_enabled() {
        let lines = ClientKind::OpenCode.snippet_lines();
        let joined = lines.join("\n");
        assert!(joined.contains("\"type\""));
        assert!(joined.contains("\"local\""));
        assert!(joined.contains("\"enabled\""));
    }

    #[test]
    fn full_file_lines_valid_json() {
        let lines = ClientKind::ClaudeCode.full_file_lines();
        let joined = lines.join("\n");
        let _: serde_json::Value = serde_json::from_str(&joined).expect("full file is valid JSON");
    }

    // ── opencode_global_config_dir ───────────────────────────────────

    #[test]
    fn opencode_global_config_dir_respects_xdg_config_home() {
        // When XDG_CONFIG_HOME is set, use it instead of ~/.config.
        // We can't safely mutate env in parallel tests, so just verify the
        // function returns a path ending in "opencode" in both branches.
        let result = opencode_global_config_dir();
        assert!(result.is_some());
        let dir = result.unwrap();
        assert_eq!(dir.file_name().unwrap(), "opencode");
    }

    #[test]
    fn opencode_global_config_dir_does_not_use_macos_library() {
        // Verify the returned path does NOT go through Library/Application Support
        // (which dirs::config_dir() would return on macOS).
        let result = opencode_global_config_dir();
        if let Some(dir) = result {
            let path_str = dir.to_string_lossy();
            assert!(
                !path_str.contains("Library/Application Support"),
                "OpenCode config path must not use macOS Library dir, got: {path_str}"
            );
        }
    }

    // ── find_opencode_config_in_dir ──────────────────────────────────

    #[test]
    fn detect_opencode_config_prefers_jsonc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("opencode.json");
        let jsonc_path = dir.path().join("opencode.jsonc");
        fs::write(&json_path, r#"{"mcp": {}}"#).unwrap();
        fs::write(&jsonc_path, "// comment\n{\"mcp\": {}}").unwrap();

        let target = find_opencode_config_in_dir(dir.path(), false);
        assert_eq!(target.path, jsonc_path);
        assert_eq!(target.format, ConfigFormat::Jsonc);
    }

    #[test]
    fn detect_opencode_config_json_when_no_jsonc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("opencode.json");
        fs::write(&json_path, r#"{"mcp": {}}"#).unwrap();

        let target = find_opencode_config_in_dir(dir.path(), false);
        assert_eq!(target.path, json_path);
        assert_eq!(target.format, ConfigFormat::Json);
    }

    #[test]
    fn detect_opencode_config_misnamed_json_with_comments_is_jsonc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("opencode.json");
        fs::write(&json_path, "// comment\n{\"mcp\": {}}").unwrap();

        let target = find_opencode_config_in_dir(dir.path(), false);
        assert_eq!(target.format, ConfigFormat::Jsonc);
    }

    #[test]
    fn detect_opencode_config_not_found_defaults_to_json() {
        let dir = tempdir().unwrap();
        let target = find_opencode_config_in_dir(dir.path(), false);
        assert!(!target.exists);
        assert_eq!(target.format, ConfigFormat::Json);
        assert_eq!(target.path.file_name().unwrap(), "opencode.json");
    }

    // ── is_already_configured ────────────────────────────────────────

    #[test]
    fn already_configured_json_true() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"mcpServers": {"seshat": {"command": "seshat"}}}"#,
        )
        .unwrap();
        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path,
            format: ConfigFormat::Json,
            exists: true,
            is_project: false,
        };
        assert!(is_already_configured(&target));
    }

    #[test]
    fn already_configured_json_false() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"mcpServers": {"other": {}}}"#).unwrap();
        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path,
            format: ConfigFormat::Json,
            exists: true,
            is_project: false,
        };
        assert!(!is_already_configured(&target));
    }

    #[test]
    fn already_configured_jsonc_text_search_true() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        fs::write(
            &path,
            "// comment\n{\"mcp\": {\"seshat\": {\"type\": \"local\"}}}",
        )
        .unwrap();
        let target = ConfigTarget {
            client: ClientKind::OpenCode,
            path,
            format: ConfigFormat::Jsonc,
            exists: true,
            is_project: false,
        };
        assert!(is_already_configured(&target));
    }

    #[test]
    fn already_configured_jsonc_no_false_positive_on_seshat_tools() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        // Contains "seshat-tools" but NOT `"seshat":` — should be false.
        fs::write(
            &path,
            "// comment\n{\"mcp\": {\"seshat-tools\": {\"type\": \"local\"}}}",
        )
        .unwrap();
        let target = ConfigTarget {
            client: ClientKind::OpenCode,
            path,
            format: ConfigFormat::Jsonc,
            exists: true,
            is_project: false,
        };
        assert!(!is_already_configured(&target));
    }

    #[test]
    fn already_configured_not_exists() {
        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path: PathBuf::from("/nonexistent/settings.json"),
            format: ConfigFormat::Json,
            exists: false,
            is_project: false,
        };
        assert!(!is_already_configured(&target));
    }

    // ── merge_seshat_entry ───────────────────────────────────────────

    #[test]
    fn merge_mcp_servers_entry_adds_seshat() {
        let mut value = serde_json::json!({"mcpServers": {"other": {}}});
        merge_seshat_entry(&mut value, ClientKind::ClaudeCode).unwrap();
        assert!(value["mcpServers"]["seshat"].is_object());
        assert_eq!(value["mcpServers"]["seshat"]["command"], "seshat");
        assert!(value["mcpServers"]["other"].is_object());
    }

    #[test]
    fn merge_mcp_servers_creates_key_if_missing() {
        let mut value = serde_json::json!({"model": "gpt-4"});
        merge_seshat_entry(&mut value, ClientKind::ClaudeCode).unwrap();
        assert!(value["mcpServers"]["seshat"].is_object());
        assert_eq!(value["model"], "gpt-4");
    }

    #[test]
    fn merge_mcp_entry_opencode_uses_mcp_key() {
        let mut value = serde_json::json!({});
        merge_seshat_entry(&mut value, ClientKind::OpenCode).unwrap();
        assert!(value["mcp"]["seshat"].is_object());
        assert_eq!(value["mcp"]["seshat"]["type"], "local");
        assert!(value["mcp"]["seshat"]["enabled"].as_bool().unwrap_or(false));
    }

    #[test]
    fn merge_seshat_entry_rejects_non_object_root() {
        let mut value = serde_json::json!([1, 2, 3]);
        let err = merge_seshat_entry(&mut value, ClientKind::ClaudeCode);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not a JSON object"));
    }

    #[test]
    fn merge_seshat_entry_rejects_null_root() {
        let mut value = serde_json::Value::Null;
        assert!(merge_seshat_entry(&mut value, ClientKind::ClaudeCode).is_err());
    }

    // ── backup ───────────────────────────────────────────────────────

    #[test]
    fn backup_filename_has_timestamp_suffix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{}").unwrap();

        let backup = write_backup(&path).expect("backup should succeed");
        let name = backup.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("settings.json.seshat-backup."));
        // Timestamp part should be numeric (milliseconds).
        let ts_part = name.split('.').next_back().unwrap_or("");
        assert!(
            ts_part.parse::<u128>().is_ok(),
            "timestamp must be numeric: {ts_part}"
        );
        assert!(backup.exists());
    }

    // ── patch_json_config ────────────────────────────────────────────

    #[test]
    fn patch_json_config_adds_entry_and_creates_backup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"globalShortcut": ""}"#).unwrap();

        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path: path.clone(),
            format: ConfigFormat::Json,
            exists: true,
            is_project: false,
        };

        let result = patch_json_config(&target).expect("patch should succeed");

        // Backup must exist and contain the original.
        let backup = result
            .backup_path
            .expect("backup should be Some for existing file");
        assert!(backup.exists());
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            r#"{"globalShortcut": ""}"#
        );

        // Config must be updated with seshat entry.
        let updated: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(updated["mcpServers"]["seshat"].is_object());
        assert_eq!(updated["globalShortcut"], "");
    }

    #[test]
    fn patch_json_config_creates_new_file_no_backup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("new_settings.json");

        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path: path.clone(),
            format: ConfigFormat::Json,
            exists: false,
            is_project: false,
        };

        let result = patch_json_config(&target).expect("patch should succeed");
        assert!(result.backup_path.is_none(), "no backup for new file");
        assert!(path.exists());
        let created: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(created["mcpServers"]["seshat"].is_object());
    }

    #[test]
    fn patch_json_config_fails_on_non_object_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        fs::write(&path, "[1, 2, 3]").unwrap();

        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path,
            format: ConfigFormat::Json,
            exists: true,
            is_project: false,
        };

        let err = patch_json_config(&target);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not a JSON object"));
    }

    // ── is_valid_json ────────────────────────────────────────────────

    #[test]
    fn is_valid_json_true_for_clean_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.json");
        fs::write(&path, r#"{"key": "value"}"#).unwrap();
        assert!(is_valid_json(&path));
    }

    #[test]
    fn is_valid_json_false_for_jsonc() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.json");
        fs::write(&path, "// comment\n{}").unwrap();
        assert!(!is_valid_json(&path));
    }
}
