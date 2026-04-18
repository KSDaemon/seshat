//! Integration tests for `seshat uninstall` command.
//!
//! Tests the full uninstall flow: removing MCP entries, instruction sections,
//! skill directories, and hook entries from AI client configs.

use std::fs;

use seshat_cli::init::ConfigFormat;
use seshat_cli::uninstall::{
    UninstallResult, remove_hooks, remove_instructions, remove_mcp_entry, remove_skill_dir,
};

// ---------------------------------------------------------------------------
// remove_instructions integration tests
// ---------------------------------------------------------------------------

#[test]
fn uninstall_removes_instructions_block_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");
    let content = "# Claude Code\n\nSome setup.\n\n<!-- seshat:start -->\n## Seshat\nquery_project_context()\n<!-- seshat:end -->\n\n## Other\nMore content.\n";
    fs::write(&path, content).unwrap();

    let result = remove_instructions(&path, false).unwrap();
    assert_eq!(result, UninstallResult::Removed);

    let new_content = fs::read_to_string(&path).unwrap();
    assert!(!new_content.contains("seshat:start"));
    assert!(!new_content.contains("seshat:end"));
    assert!(new_content.contains("# Claude Code"));
    assert!(new_content.contains("## Other"));
    assert!(new_content.contains("Some setup."));
    assert!(new_content.contains("More content."));
}

#[test]
fn uninstall_instructions_not_found_returns_not_exists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");
    fs::write(&path, "# Just a regular file without any seshat markers\n").unwrap();

    let result = remove_instructions(&path, false).unwrap();
    assert_eq!(result, UninstallResult::NotExists);
}

#[test]
fn uninstall_instructions_dry_run_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");
    let original = "# Header\n\n<!-- seshat:start -->\ncontent\n<!-- seshat:end -->\n";
    fs::write(&path, original).unwrap();

    let result = remove_instructions(&path, true).unwrap();
    assert!(matches!(result, UninstallResult::DryRun(_)));

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, original, "file should not be modified in dry-run");
}

#[test]
fn uninstall_removes_multiple_seshat_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");
    let content = format!(
        "# Header\n\n\
         <!-- seshat:start -->\n\
         ## Seshat v1\n\
         query_project_context()\n\
         <!-- seshat:end -->\n\n\
         ## Middle Section\n\n\
         <!-- seshat:start -->\n\
         ## Seshat v2\n\
         query_code_pattern()\n\
         <!-- seshat:end -->\n\n\
         # Footer\n",
    );
    fs::write(&path, &content).unwrap();

    let result = remove_instructions(&path, false).unwrap();
    assert_eq!(result, UninstallResult::Removed);

    let new_content = fs::read_to_string(&path).unwrap();
    assert!(
        !new_content.contains("seshat:start"),
        "no start markers remaining"
    );
    assert!(
        !new_content.contains("seshat:end"),
        "no end markers remaining"
    );
    assert!(new_content.contains("# Header"), "header preserved");
    assert!(
        new_content.contains("## Middle Section"),
        "middle section preserved"
    );
    assert!(new_content.contains("# Footer"), "footer preserved");
    assert!(
        !new_content.contains("\n\n\n"),
        "no triple newlines, got: {:?}",
        new_content
    );
}

// ---------------------------------------------------------------------------
// remove_mcp_entry integration tests
// ---------------------------------------------------------------------------

#[test]
fn uninstall_removes_mcp_entry_from_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        r#"{
  "mcpServers": {
    "seshat": {
      "command": "seshat",
      "args": ["serve"]
    },
    "other-server": {
      "command": "/usr/local/bin/other"
    }
  }
}"#,
    )
    .unwrap();

    let result = remove_mcp_entry(
        &path,
        seshat_cli::init::ClientKind::ClaudeCode,
        ConfigFormat::Json,
        false,
    )
    .unwrap();
    assert_eq!(result, UninstallResult::Removed);

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(
        parsed["mcpServers"].get("seshat").is_none(),
        "seshat entry should be removed"
    );
    assert!(
        parsed["mcpServers"]["other-server"].is_object(),
        "other-server entry should be preserved"
    );
}

#[test]
fn uninstall_removes_empty_mcp_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        r#"{"mcpServers": {"seshat": {"command": "seshat", "args": ["serve"]}}}"#,
    )
    .unwrap();

    remove_mcp_entry(
        &path,
        seshat_cli::init::ClientKind::ClaudeCode,
        ConfigFormat::Json,
        false,
    )
    .unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(
        parsed.get("mcpServers").is_none(),
        "empty mcpServers key should be removed"
    );
}

#[test]
fn uninstall_preserves_other_keys_in_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        r#"{
  "theme": "dark",
  "fontSize": 14,
  "mcpServers": {
    "seshat": {"command": "seshat"},
    "other": {"command": "other"}
  },
  "plugins": {"enabled": true}
}"#,
    )
    .unwrap();

    remove_mcp_entry(
        &path,
        seshat_cli::init::ClientKind::ClaudeCode,
        ConfigFormat::Json,
        false,
    )
    .unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["theme"], "dark", "theme preserved");
    assert_eq!(parsed["fontSize"], 14, "fontSize preserved");
    assert_eq!(parsed["plugins"]["enabled"], true, "plugins preserved");
}

#[test]
fn uninstall_mcp_entry_dry_run_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let original = r#"{"mcpServers": {"seshat": {"command": "seshat"}}}"#;
    fs::write(&path, original).unwrap();

    let result = remove_mcp_entry(
        &path,
        seshat_cli::init::ClientKind::ClaudeCode,
        ConfigFormat::Json,
        true,
    )
    .unwrap();
    assert!(matches!(result, UninstallResult::DryRun(_)));

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, original);
}

#[test]
fn uninstall_mcp_entry_not_exists_when_no_seshat_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, r#"{"mcpServers": {"other": {"command": "other"}}}"#).unwrap();

    let result = remove_mcp_entry(
        &path,
        seshat_cli::init::ClientKind::ClaudeCode,
        ConfigFormat::Json,
        false,
    )
    .unwrap();
    assert_eq!(result, UninstallResult::NotExists);
}

// ---------------------------------------------------------------------------
// remove_skill_dir integration tests
// ---------------------------------------------------------------------------

#[test]
fn uninstall_removes_skill_directory() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("skills").join("seshat");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "---\nname: seshat\n---\n").unwrap();
    fs::create_dir_all(skill_dir.join("subdir")).unwrap();
    fs::write(skill_dir.join("subdir").join("extra.txt"), "data").unwrap();

    let result = remove_skill_dir(&skill_dir, false).unwrap();
    assert_eq!(result, UninstallResult::Removed);
    assert!(!skill_dir.exists(), "skill directory should be removed");
}

#[test]
fn uninstall_skill_dir_not_exists_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("skills").join("seshat");

    let result = remove_skill_dir(&skill_dir, false).unwrap();
    assert_eq!(result, UninstallResult::NotExists);
}

#[test]
fn uninstall_skill_dir_dry_run_no_removal() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("skills").join("seshat");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "content").unwrap();

    let result = remove_skill_dir(&skill_dir, true).unwrap();
    assert!(matches!(result, UninstallResult::DryRun(_)));
    assert!(
        skill_dir.exists(),
        "skill directory should not be removed in dry-run"
    );
}

// ---------------------------------------------------------------------------
// remove_hooks integration tests
// ---------------------------------------------------------------------------

#[test]
fn uninstall_removes_hooks_and_entries() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    let settings = dir.path().join("settings.json");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        hooks_dir.join("seshat-session-start"),
        "#!/bin/bash\ncat << 'EOF'\nSeshat reminder\nEOF\n",
    )
    .unwrap();
    fs::write(
        hooks_dir.join("seshat-pre-tool"),
        "#!/bin/bash\necho 'Seshat tip'\n",
    )
    .unwrap();
    fs::write(
        &settings,
        r#"{
  "theme": "dark",
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep|Glob|Read|Search",
        "hooks": [{"type": "command", "command": "/hooks/seshat-pre-tool"}]
      }
    ],
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [{"type": "command", "command": "/hooks/seshat-session-start"}]
      }
    ]
  }
}"#,
    )
    .unwrap();

    let result = remove_hooks(&hooks_dir, &settings, false).unwrap();
    assert_eq!(result, UninstallResult::Removed);

    // Hooks dir should be removed (was empty after script removal).
    assert!(!hooks_dir.exists(), "empty hooks dir should be removed");

    let content = fs::read_to_string(&settings).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["theme"], "dark", "non-hook keys preserved");
    assert!(
        parsed["hooks"].get("PreToolUse").is_none(),
        "PreToolUse should be removed (was empty)"
    );
    assert!(
        parsed["hooks"].get("SessionStart").is_none(),
        "SessionStart should be removed (was empty)"
    );
}

#[test]
fn uninstall_hooks_preserves_other_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    let settings = dir.path().join("settings.json");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        hooks_dir.join("seshat-pre-tool"),
        "#!/bin/bash\necho nudge\n",
    )
    .unwrap();
    fs::write(
        &settings,
        r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep",
        "hooks": [{"type": "command", "command": "/hooks/seshat-pre-tool"}]
      },
      {
        "matcher": "Glob",
        "hooks": [{"type": "command", "command": "/hooks/other-hook"}]
      }
    ]
  }
}"#,
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
fn uninstall_hooks_not_exists_when_no_seshat_entries() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    let settings = dir.path().join("settings.json");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        &settings,
        r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep",
        "hooks": [{"type": "command", "command": "/hooks/other-hook"}]
      }
    ]
  }
}"#,
    )
    .unwrap();

    let result = remove_hooks(&hooks_dir, &settings, false).unwrap();
    assert_eq!(result, UninstallResult::NotExists);
}

#[test]
fn uninstall_hooks_dry_run_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    let settings = dir.path().join("settings.json");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        hooks_dir.join("seshat-pre-tool"),
        "#!/bin/bash\necho nudge\n",
    )
    .unwrap();
    fs::write(
        &settings,
        r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep",
        "hooks": [{"type": "command", "command": "/hooks/seshat-pre-tool"}]
      }
    ]
  }
}"#,
    )
    .unwrap();

    let result = remove_hooks(&hooks_dir, &settings, true).unwrap();
    assert!(matches!(result, UninstallResult::DryRun(_)));

    assert!(
        hooks_dir.join("seshat-pre-tool").exists(),
        "hook should not be removed in dry-run"
    );
}

#[test]
fn uninstall_hooks_handles_missing_files_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    let settings = dir.path().join("settings.json");
    // Don't create any files.

    let result = remove_hooks(&hooks_dir, &settings, false).unwrap();
    assert_eq!(result, UninstallResult::NotExists);
}

// ---------------------------------------------------------------------------
// End-to-end: uninstall reverses init
// ---------------------------------------------------------------------------

#[test]
fn uninstall_reverses_init_full_flow_for_claude_code() {
    let dir = tempfile::tempdir().unwrap();
    let claude_home = dir.path().join(".claude");
    fs::create_dir_all(&claude_home).unwrap();

    // Simulate what `seshat init` creates for Claude Code.

    // 1. CLAUDE.md with seshat section.
    let claude_md = claude_home.join("CLAUDE.md");
    fs::write(
        &claude_md,
        "# Claude Code\n\n## Setup\n\nInitial setup.\n\n<!-- seshat:start -->\n## Seshat\nquery_project_context()\n<!-- seshat:end -->\n\n## Other\nMore content.\n",
    )
    .unwrap();

    // 2. Skill directory.
    let skill_dir = claude_home.join("skills").join("seshat");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "---\nname: seshat\n---\n").unwrap();

    // 3. Hook scripts.
    let hooks_dir = claude_home.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        hooks_dir.join("seshat-session-start"),
        "#!/bin/bash\necho 'Seshat reminder'\n",
    )
    .unwrap();
    fs::write(
        hooks_dir.join("seshat-pre-tool"),
        "#!/bin/bash\necho 'Seshat tip'\n",
    )
    .unwrap();

    // 4. settings.json with hook entries.
    let settings = claude_home.join("settings.json");
    fs::write(
        &settings,
        r#"{
  "theme": "dark",
  "fontSize": 14,
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep|Glob|Read|Search",
        "hooks": [{"type": "command", "command": "/hooks/seshat-pre-tool"}]
      }
    ],
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [{"type": "command", "command": "/hooks/seshat-session-start"}]
      }
    ]
  }
}"#,
    )
    .unwrap();

    // 5. ~/.claude.json with MCP entry.
    let claude_json = dir.path().join(".claude.json");
    fs::write(
        &claude_json,
        r#"{
  "mcpServers": {
    "seshat": {"command": "seshat", "args": ["serve"]},
    "other-server": {"command": "/usr/local/bin/other"}
  }
}"#,
    )
    .unwrap();

    // ── Perform uninstall ──────────────────────────────────────

    // Remove instructions.
    let ins_result = remove_instructions(&claude_md, false).unwrap();
    assert_eq!(ins_result, UninstallResult::Removed);

    // Remove skill dir.
    let skill_result = remove_skill_dir(&skill_dir, false).unwrap();
    assert_eq!(skill_result, UninstallResult::Removed);

    // Remove hooks.
    let hooks_result = remove_hooks(&hooks_dir, &settings, false).unwrap();
    assert_eq!(hooks_result, UninstallResult::Removed);

    // Remove MCP entry.
    let mcp_result = remove_mcp_entry(
        &claude_json,
        seshat_cli::init::ClientKind::ClaudeCode,
        ConfigFormat::Json,
        false,
    )
    .unwrap();
    assert_eq!(mcp_result, UninstallResult::Removed);

    // ── Verify results ─────────────────────────────────────────

    // CLAUDE.md should not contain seshat markers.
    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(!content.contains("seshat:start"));
    assert!(!content.contains("seshat:end"));
    assert!(content.contains("# Claude Code"));
    assert!(content.contains("## Other"));

    // Skill dir should be gone.
    assert!(!skill_dir.exists());

    // Hooks dir should be gone (was empty).
    assert!(!hooks_dir.exists());

    // settings.json should not have seshat hooks.
    let settings_content = fs::read_to_string(&settings).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&settings_content).unwrap();
    assert_eq!(parsed["theme"], "dark", "theme preserved");
    assert_eq!(parsed["fontSize"], 14, "fontSize preserved");
    assert!(
        parsed["hooks"].get("PreToolUse").is_none(),
        "PreToolUse removed"
    );
    assert!(
        parsed["hooks"].get("SessionStart").is_none(),
        "SessionStart removed"
    );

    // .claude.json should not have seshat MCP entry.
    let mcp_content = fs::read_to_string(&claude_json).unwrap();
    let mcp_parsed: serde_json::Value = serde_json::from_str(&mcp_content).unwrap();
    assert!(
        mcp_parsed["mcpServers"].get("seshat").is_none(),
        "seshat MCP entry removed"
    );
    assert!(
        mcp_parsed["mcpServers"]["other-server"].is_object(),
        "other-server MCP entry preserved"
    );
}

#[test]
fn uninstall_dry_run_no_changes_made() {
    let dir = tempfile::tempdir().unwrap();
    let claude_home = dir.path().join(".claude");
    fs::create_dir_all(&claude_home).unwrap();

    // Set up files.
    let claude_md = claude_home.join("CLAUDE.md");
    fs::write(
        &claude_md,
        "# Header\n\n<!-- seshat:start -->\nseshat content\n<!-- seshat:end -->\n",
    )
    .unwrap();

    let skill_dir = claude_home.join("skills").join("seshat");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "content").unwrap();

    let hooks_dir = claude_home.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(
        hooks_dir.join("seshat-pre-tool"),
        "#!/bin/bash\necho nudge\n",
    )
    .unwrap();

    let settings = claude_home.join("settings.json");
    fs::write(
        &settings,
        r#"{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"type":"command","command":"/hooks/seshat-pre-tool"}]}]}}"#,
    )
    .unwrap();

    let claude_json = dir.path().join(".claude.json");
    fs::write(
        &claude_json,
        r#"{"mcpServers":{"seshat":{"command":"seshat"}}}"#,
    )
    .unwrap();

    // ── Dry-run operations ─────────────────────────────────────

    let ins_result = remove_instructions(&claude_md, true).unwrap();
    assert!(matches!(ins_result, UninstallResult::DryRun(_)));

    let skill_result = remove_skill_dir(&skill_dir, true).unwrap();
    assert!(matches!(skill_result, UninstallResult::DryRun(_)));

    let hooks_result = remove_hooks(&hooks_dir, &settings, true).unwrap();
    assert!(matches!(hooks_result, UninstallResult::DryRun(_)));

    let mcp_result = remove_mcp_entry(
        &claude_json,
        seshat_cli::init::ClientKind::ClaudeCode,
        ConfigFormat::Json,
        true,
    )
    .unwrap();
    assert!(matches!(mcp_result, UninstallResult::DryRun(_)));

    // ── Verify nothing was changed ─────────────────────────────

    assert!(claude_md.exists(), "CLAUDE.md should still exist");
    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("seshat:start"),
        "seshat markers should still be present"
    );

    assert!(skill_dir.exists(), "skill dir should still exist");
    assert!(
        hooks_dir.join("seshat-pre-tool").exists(),
        "hook script should still exist"
    );

    let settings_content = fs::read_to_string(&settings).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&settings_content).unwrap();
    assert!(
        parsed["hooks"]["PreToolUse"].is_array(),
        "hooks should still be present"
    );

    let mcp_content = fs::read_to_string(&claude_json).unwrap();
    let mcp_parsed: serde_json::Value = serde_json::from_str(&mcp_content).unwrap();
    assert!(
        mcp_parsed["mcpServers"]["seshat"].is_object(),
        "MCP entry should still be present"
    );
}

#[test]
fn uninstall_handles_not_existing_files_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("CLAUDE.md");

    // All should return NotExists for non-existent files.
    assert_eq!(
        remove_instructions(&path, false).unwrap(),
        UninstallResult::NotExists
    );
    assert_eq!(
        remove_mcp_entry(
            &path,
            seshat_cli::init::ClientKind::ClaudeCode,
            ConfigFormat::Json,
            false
        )
        .unwrap(),
        UninstallResult::NotExists
    );

    let skill_dir = dir.path().join("skills").join("seshat");
    assert_eq!(
        remove_skill_dir(&skill_dir, false).unwrap(),
        UninstallResult::NotExists
    );

    let hooks_dir = dir.path().join("hooks");
    assert_eq!(
        remove_hooks(&hooks_dir, &path, false).unwrap(),
        UninstallResult::NotExists
    );
}
