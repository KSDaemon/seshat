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
//! ## JSONC handling
//!
//! OpenCode supports both `.json` and `.jsonc` config files. When a `.jsonc`
//! file is detected (or a `.json` file that fails to parse as JSON), we only
//! show a snippet — we never auto-patch JSONC because round-tripping through a
//! parser would silently destroy comments.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;

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

    /// Generate the JSON snippet fragment to insert (the value for "seshat" key).
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

    /// Format the snippet lines for display in a copy block.
    ///
    /// Returns lines representing just the `"seshat": { ... }` fragment,
    /// without outer braces — suitable for pasting into an existing config.
    pub fn snippet_lines(self) -> Vec<String> {
        let entry = self.seshat_entry_json();
        let formatted = serde_json::to_string_pretty(&entry).unwrap_or_else(|_| "{}".to_string());
        // Wrap with the key name.
        let mut lines = vec![format!(
            "\"seshat\": {}",
            formatted.lines().next().unwrap_or("{")
        )];
        for line in formatted.lines().skip(1) {
            lines.push(line.to_string());
        }
        lines
    }

    /// Format full file content lines for when the config does not exist yet.
    pub fn full_file_lines(self) -> Vec<String> {
        let entry = self.seshat_entry_json();
        let root = serde_json::json!({
            self.mcp_key(): {
                "seshat": entry
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

/// Scope for config targeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// Global user-level config (`~/.claude/settings.json`, etc.).
    Global,
    /// Project-level config (`.claude/settings.local.json`, `./opencode.json`, etc.).
    Project,
}

/// A resolved config target for a specific client.
#[derive(Debug)]
pub struct ConfigTarget {
    pub client: ClientKind,
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub exists: bool,
}

// ══════════════════════════════════════════════════════════════════════
// Detection
// ══════════════════════════════════════════════════════════════════════

/// Detect all installed AI coding clients and resolve their config targets.
///
/// `scope` controls whether global or project-level configs are targeted.
/// `cwd` is used when `scope == Project`.
pub fn detect_clients(scope: ConfigScope, cwd: &Path) -> Vec<ConfigTarget> {
    let mut targets = Vec::new();

    // Claude Code: `claude` in PATH
    if which::which("claude").is_ok() {
        if let Some(t) = resolve_claude_code_config(scope, cwd) {
            targets.push(t);
        }
    }

    // Claude Desktop: app directory exists (macOS only)
    #[cfg(target_os = "macos")]
    if let Some(t) = resolve_claude_desktop_config(scope) {
        targets.push(t);
    }

    // OpenCode: `opencode` in PATH
    if which::which("opencode").is_ok() {
        if let Some(t) = resolve_opencode_config(scope, cwd) {
            targets.push(t);
        }
    }

    // Cursor: `cursor` in PATH
    if which::which("cursor").is_ok() {
        if let Some(t) = resolve_cursor_config(scope, cwd) {
            targets.push(t);
        }
    }

    targets
}

/// Resolve config target for a single explicitly-named client.
pub fn resolve_single_client(
    client: ClientKind,
    scope: ConfigScope,
    cwd: &Path,
) -> Option<ConfigTarget> {
    match client {
        ClientKind::ClaudeCode => resolve_claude_code_config(scope, cwd),
        ClientKind::ClaudeDesktop => {
            #[cfg(target_os = "macos")]
            {
                resolve_claude_desktop_config(scope)
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        }
        ClientKind::OpenCode => resolve_opencode_config(scope, cwd),
        ClientKind::Cursor => resolve_cursor_config(scope, cwd),
    }
}

fn resolve_claude_code_config(scope: ConfigScope, cwd: &Path) -> Option<ConfigTarget> {
    let path = match scope {
        ConfigScope::Global => dirs::home_dir()?.join(".claude").join("settings.json"),
        ConfigScope::Project => cwd.join(".claude").join("settings.local.json"),
    };
    Some(config_target(ClientKind::ClaudeCode, path))
}

#[cfg(target_os = "macos")]
fn resolve_claude_desktop_config(_scope: ConfigScope) -> Option<ConfigTarget> {
    // Claude Desktop only has a global config; no project-level equivalent.
    let path = dirs::home_dir()?
        .join("Library")
        .join("Application Support")
        .join("Claude")
        .join("claude_desktop_config.json");
    // Only report if the app directory exists (app is installed).
    let app_dir = dirs::home_dir()?
        .join("Library")
        .join("Application Support")
        .join("Claude");
    if !app_dir.is_dir() {
        return None;
    }
    Some(config_target(ClientKind::ClaudeDesktop, path))
}

fn resolve_opencode_config(scope: ConfigScope, cwd: &Path) -> Option<ConfigTarget> {
    let dir = match scope {
        ConfigScope::Global => dirs::config_dir()?.join("opencode"),
        ConfigScope::Project => cwd.to_path_buf(),
    };
    Some(find_opencode_config_in_dir(&dir, ClientKind::OpenCode))
}

fn resolve_cursor_config(scope: ConfigScope, cwd: &Path) -> Option<ConfigTarget> {
    let path = match scope {
        ConfigScope::Global => dirs::home_dir()?.join(".cursor").join("mcp.json"),
        ConfigScope::Project => cwd.join(".cursor").join("mcp.json"),
    };
    Some(config_target(ClientKind::Cursor, path))
}

/// Find the opencode config in a directory, preferring `.jsonc` over `.json`.
///
/// If both exist, `.jsonc` takes precedence (matches opencode's own priority).
/// If neither exists, returns a target pointing at `opencode.json` (will be
/// created on patch).
pub fn find_opencode_config_in_dir(dir: &Path, client: ClientKind) -> ConfigTarget {
    let jsonc_path = dir.join("opencode.jsonc");
    let json_path = dir.join("opencode.json");

    if jsonc_path.exists() {
        ConfigTarget {
            client,
            path: jsonc_path,
            format: ConfigFormat::Jsonc,
            exists: true,
        }
    } else if json_path.exists() {
        // Even a .json file may contain comments — try parsing to confirm.
        let format = if is_valid_json(&json_path) {
            ConfigFormat::Json
        } else {
            ConfigFormat::Jsonc
        };
        ConfigTarget {
            client,
            path: json_path,
            format,
            exists: true,
        }
    } else {
        // Neither exists; default to creating opencode.json.
        ConfigTarget {
            client,
            path: json_path,
            format: ConfigFormat::Json,
            exists: false,
        }
    }
}

/// Build a `ConfigTarget` for a path that is always JSON (not opencode).
fn config_target(client: ClientKind, path: PathBuf) -> ConfigTarget {
    let exists = path.exists();
    ConfigTarget {
        client,
        path,
        format: ConfigFormat::Json,
        exists,
    }
}

/// Return `true` if the file at `path` parses as valid JSON.
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
/// For JSON files: parse and check the appropriate key.
/// For JSONC files: simple text search for `"seshat"` (fast, no parsing).
/// For non-existent files: returns `false`.
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
        ConfigFormat::Jsonc => {
            // Simple text search — conservative: if "seshat" appears anywhere
            // in the mcp section, assume configured.
            content.contains("\"seshat\"")
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// Patching
// ══════════════════════════════════════════════════════════════════════

/// Write a timestamped backup of `path` next to the original.
///
/// Backup name: `{filename}.seshat-backup.{unix_timestamp}`
/// Returns the backup path on success.
pub fn write_backup(path: &Path) -> Result<PathBuf, CliError> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
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
/// Overwrites any existing `seshat` entry (idempotent).
pub fn merge_seshat_entry(value: &mut serde_json::Value, client: ClientKind) {
    let mcp_key = client.mcp_key();
    if value.get(mcp_key).is_none() {
        value[mcp_key] = serde_json::json!({});
    }
    value[mcp_key]["seshat"] = client.seshat_entry_json();
}

/// Patch a JSON config file: backup → merge → write.
///
/// Returns the backup path.
pub fn patch_json_config(target: &ConfigTarget) -> Result<PathBuf, CliError> {
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

    merge_seshat_entry(&mut value, target.client);

    let updated = serde_json::to_string_pretty(&value)
        .map_err(|e| CliError::InvalidArgument(format!("failed to serialize config: {e}")))?;

    // Backup only if the file already exists.
    let backup_path = if target.exists {
        write_backup(&target.path)?
    } else {
        // Create parent directory if needed.
        if let Some(parent) = target.path.parent() {
            fs::create_dir_all(parent).map_err(|e| CliError::IoWithPath {
                message: format!("failed to create directory: {e}"),
                path: parent.to_path_buf(),
            })?;
        }
        // Return a placeholder path (no backup for new file).
        target.path.with_extension("seshat-new")
    };

    fs::write(&target.path, updated.as_bytes()).map_err(|e| CliError::IoWithPath {
        message: format!("failed to write config: {e}"),
        path: target.path.clone(),
    })?;

    Ok(backup_path)
}

// ══════════════════════════════════════════════════════════════════════
// Output helpers
// ══════════════════════════════════════════════════════════════════════

/// Print a `✓ message` line.
fn print_ok(message: &str, color: bool) {
    if color {
        eprintln!("  {} {message}", "✓".green().bold());
    } else {
        eprintln!("  ✓ {message}");
    }
}

/// Print an info line with 2-space indent.
fn print_info(message: &str) {
    eprintln!("  {message}");
}

/// Ask the user a yes/no question. Returns `true` for "y"/"Y".
/// In dry-run mode, prints a note and returns `false`.
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
// Per-client output
// ══════════════════════════════════════════════════════════════════════

/// Handle output and optional patching for a single config target.
fn handle_target(target: &ConfigTarget, dry_run: bool, color: bool) {
    // Section header.
    eprintln!(
        "{}",
        format_section_header(target.client.display_name(), color)
    );
    eprintln!();

    let path_display = target.path.display().to_string();

    // Already configured?
    if is_already_configured(target) {
        if target.format == ConfigFormat::Jsonc {
            print_info(&format!("Config: {path_display}"));
            print_ok(
                "Already configured (detected in JSONC — verify manually).",
                color,
            );
        } else {
            print_info(&format!("Config: {path_display}"));
            print_ok("Already configured.", color);
        }
        eprintln!();
        return;
    }

    // JSONC — snippet only.
    if target.format == ConfigFormat::Jsonc {
        print_info(&format!("Config: {path_display}"));
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
        eprintln!();
        return;
    }

    // JSON — auto-patch flow.
    if target.exists {
        print_info(&format!("Config: {path_display}"));
        print_info(&format!(
            "Seshat is not configured. Add to \"{}\":",
            target.client.mcp_key()
        ));
    } else {
        print_info(&format!("Config not found: {path_display}"));
        print_info("Will create new file with:");
    }
    eprintln!();

    if target.exists {
        let owned = target.client.snippet_lines();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        eprint!("{}", format_copy_block(&refs, color));
    } else {
        let owned = target.client.full_file_lines();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        eprint!("{}", format_copy_block(&refs, color));
    }
    eprintln!();

    let prompt = if target.exists {
        "Auto-add?"
    } else {
        "Create file?"
    };

    if ask_yn(prompt, dry_run) {
        match patch_json_config(target) {
            Ok(backup_path) => {
                if target.exists {
                    print_ok(&format!("Backup saved: {}", backup_path.display()), color);
                }
                print_ok(&format!("Updated {path_display}"), color);
            }
            Err(e) => {
                if color {
                    eprintln!("  {} {}", "error:".red().bold(), e);
                } else {
                    eprintln!("  error: {e}");
                }
            }
        }
    } else if !dry_run {
        print_info("Skipped. Add the snippet above manually.");
    }

    eprintln!();
}

// ══════════════════════════════════════════════════════════════════════
// Entry point
// ══════════════════════════════════════════════════════════════════════

/// Run the `seshat init` command.
pub fn run_init(client: Option<&str>, project: bool, dry_run: bool) -> Result<(), CliError> {
    let color = color_enabled();
    let scope = if project {
        ConfigScope::Project
    } else {
        ConfigScope::Global
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Scope header.
    if project {
        if color {
            eprintln!("  {} project ({})", "Scope:".dimmed(), cwd.display());
        } else {
            eprintln!("  Scope: project ({})", cwd.display());
        }
        eprintln!();
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

    // Explicit client mode.
    if let Some(name) = client {
        let kind = ClientKind::from_cli_name(name).ok_or_else(|| {
            CliError::InvalidArgument(format!(
                "Unknown client: {name}\n\nhint: Supported clients: claude-code, claude-desktop, opencode, cursor\nhint: Run `seshat init --help` for usage."
            ))
        })?;
        let target = resolve_single_client(kind, scope, &cwd).ok_or_else(|| {
            CliError::InvalidArgument(format!(
                "{} is not available on this platform",
                kind.display_name(),
            ))
        })?;
        handle_target(&target, dry_run, color);
        return Ok(());
    }

    // Auto-detect mode.
    let targets = detect_clients(scope, &cwd);

    if targets.is_empty() {
        eprintln!("  No AI coding clients detected in PATH.");
        eprintln!();
        eprintln!("  Supported clients: claude-code, claude-desktop, opencode, cursor");
        eprintln!("  Run `seshat init <client>` to generate config for a specific client.");
        return Ok(());
    }

    // Print detection header.
    eprintln!("  Detected AI coding clients:");
    eprintln!();
    for t in &targets {
        if color {
            eprintln!(
                "    {} {} — {}",
                "✓".green().bold(),
                t.client.cli_name(),
                t.client.display_name()
            );
        } else {
            eprintln!(
                "    ✓ {} — {}",
                t.client.cli_name(),
                t.client.display_name()
            );
        }
    }
    eprintln!();

    // Handle each target.
    for target in &targets {
        handle_target(target, dry_run, color);
    }

    Ok(())
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
    fn snippet_lines_claude_code_contains_command() {
        let lines = ClientKind::ClaudeCode.snippet_lines();
        let joined = lines.join("\n");
        assert!(joined.contains("\"seshat\""));
        assert!(joined.contains("\"command\""));
        assert!(joined.contains("\"seshat\""));
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
        // Should parse as valid JSON.
        let _: serde_json::Value = serde_json::from_str(&joined).expect("full file is valid JSON");
    }

    // ── find_opencode_config_in_dir ──────────────────────────────────

    #[test]
    fn detect_opencode_config_prefers_jsonc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("opencode.json");
        let jsonc_path = dir.path().join("opencode.jsonc");
        fs::write(&json_path, r#"{"mcp": {}}"#).unwrap();
        fs::write(
            &jsonc_path,
            r#"// comment
{"mcp": {}}"#,
        )
        .unwrap();

        let target = find_opencode_config_in_dir(dir.path(), ClientKind::OpenCode);
        assert_eq!(target.path, jsonc_path);
        assert_eq!(target.format, ConfigFormat::Jsonc);
    }

    #[test]
    fn detect_opencode_config_json_when_no_jsonc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("opencode.json");
        fs::write(&json_path, r#"{"mcp": {}}"#).unwrap();

        let target = find_opencode_config_in_dir(dir.path(), ClientKind::OpenCode);
        assert_eq!(target.path, json_path);
        assert_eq!(target.format, ConfigFormat::Json);
    }

    #[test]
    fn detect_opencode_config_misnamed_json_with_comments_is_jsonc() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("opencode.json");
        // File has extension .json but contains comments → invalid JSON.
        fs::write(
            &json_path,
            r#"// this is a comment
{
  "mcp": {}
}"#,
        )
        .unwrap();

        let target = find_opencode_config_in_dir(dir.path(), ClientKind::OpenCode);
        assert_eq!(target.format, ConfigFormat::Jsonc);
    }

    #[test]
    fn detect_opencode_config_not_found_defaults_to_json() {
        let dir = tempdir().unwrap();
        let target = find_opencode_config_in_dir(dir.path(), ClientKind::OpenCode);
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
        };
        assert!(!is_already_configured(&target));
    }

    #[test]
    fn already_configured_jsonc_text_search() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        fs::write(
            &path,
            r#"// comment
{
  "mcp": {
    "seshat": { "type": "local" }
  }
}"#,
        )
        .unwrap();
        let target = ConfigTarget {
            client: ClientKind::OpenCode,
            path,
            format: ConfigFormat::Jsonc,
            exists: true,
        };
        assert!(is_already_configured(&target));
    }

    #[test]
    fn already_configured_not_exists() {
        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path: PathBuf::from("/nonexistent/settings.json"),
            format: ConfigFormat::Json,
            exists: false,
        };
        assert!(!is_already_configured(&target));
    }

    // ── merge_seshat_entry ───────────────────────────────────────────

    #[test]
    fn merge_mcp_servers_entry_adds_seshat() {
        let mut value = serde_json::json!({"mcpServers": {"other": {}}});
        merge_seshat_entry(&mut value, ClientKind::ClaudeCode);
        assert!(value["mcpServers"]["seshat"].is_object());
        assert_eq!(value["mcpServers"]["seshat"]["command"], "seshat");
        // Existing keys preserved.
        assert!(value["mcpServers"]["other"].is_object());
    }

    #[test]
    fn merge_mcp_servers_creates_key_if_missing() {
        let mut value = serde_json::json!({"model": "gpt-4"});
        merge_seshat_entry(&mut value, ClientKind::ClaudeCode);
        assert!(value["mcpServers"]["seshat"].is_object());
        // Existing keys preserved.
        assert_eq!(value["model"], "gpt-4");
    }

    #[test]
    fn merge_mcp_entry_opencode_uses_mcp_key() {
        let mut value = serde_json::json!({});
        merge_seshat_entry(&mut value, ClientKind::OpenCode);
        assert!(value["mcp"]["seshat"].is_object());
        assert_eq!(value["mcp"]["seshat"]["type"], "local");
        assert!(value["mcp"]["seshat"]["enabled"].as_bool().unwrap_or(false));
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
        // Timestamp part should be numeric.
        let ts_part = name.split('.').next_back().unwrap_or("");
        assert!(
            ts_part.parse::<u64>().is_ok(),
            "timestamp must be numeric: {ts_part}"
        );
        assert!(backup.exists());
    }

    // ── patch_json_config ────────────────────────────────────────────

    #[test]
    fn patch_json_config_adds_entry_to_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"globalShortcut": ""}"#).unwrap();

        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path: path.clone(),
            format: ConfigFormat::Json,
            exists: true,
        };

        patch_json_config(&target).expect("patch should succeed");

        let updated: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(updated["mcpServers"]["seshat"].is_object());
        // Existing key preserved.
        assert_eq!(updated["globalShortcut"], "");
    }

    #[test]
    fn patch_json_config_creates_new_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("new_settings.json");

        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path: path.clone(),
            format: ConfigFormat::Json,
            exists: false,
        };

        patch_json_config(&target).expect("patch should succeed");

        assert!(path.exists());
        let created: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(created["mcpServers"]["seshat"].is_object());
    }

    #[test]
    fn patch_json_config_creates_backup_for_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{}").unwrap();

        let target = ConfigTarget {
            client: ClientKind::ClaudeCode,
            path: path.clone(),
            format: ConfigFormat::Json,
            exists: true,
        };

        let backup = patch_json_config(&target).expect("patch should succeed");
        assert!(backup.exists());
        // Backup content is the original.
        let backup_content = fs::read_to_string(&backup).unwrap();
        assert_eq!(backup_content, "{}");
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
