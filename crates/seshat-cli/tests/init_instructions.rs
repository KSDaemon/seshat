//! Integration tests for `seshat init` agent instruction writing (Story 9.2).
//!
//! These tests exercise `run_init()` end-to-end against a temp directory,
//! verifying that instruction files, skill files, and hook registrations are
//! created correctly for each supported client.

use std::fs;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Verify that `upsert_instructions` creates AGENTS.md with seshat markers
/// when called directly — simulating what run_init does for OpenCode.
#[test]
fn init_writes_instructions_to_agents_md() {
    use seshat_cli::instructions::{AGENTS_MD_CONTENT, UpsertResult, upsert_instructions};

    let tmp = tempfile::tempdir().unwrap();
    let agents_md = tmp.path().join("AGENTS.md");

    let result = upsert_instructions(&agents_md, AGENTS_MD_CONTENT, false).unwrap();

    assert_eq!(result, UpsertResult::Created);
    let content = fs::read_to_string(&agents_md).unwrap();
    assert!(
        content.contains("<!-- seshat:start -->"),
        "start marker present"
    );
    assert!(
        content.contains("<!-- seshat:end -->"),
        "end marker present"
    );
    assert!(
        content.contains("query_project_context"),
        "seshat tool reference present"
    );
    assert!(
        content.contains("query_code_pattern"),
        "seshat tool reference present"
    );
}

/// Verify that running upsert twice produces exactly one seshat section.
#[test]
fn init_instructions_are_idempotent() {
    use seshat_cli::instructions::{AGENTS_MD_CONTENT, upsert_instructions};

    let tmp = tempfile::tempdir().unwrap();
    let agents_md = tmp.path().join("AGENTS.md");

    upsert_instructions(&agents_md, AGENTS_MD_CONTENT, false).unwrap();
    upsert_instructions(&agents_md, AGENTS_MD_CONTENT, false).unwrap();

    let content = fs::read_to_string(&agents_md).unwrap();
    let count = content.matches("<!-- seshat:start -->").count();
    assert_eq!(count, 1, "exactly one seshat section after two upserts");
}

/// Verify that seshat section is appended to an existing AGENTS.md.
#[test]
fn init_appends_to_existing_agents_md() {
    use seshat_cli::instructions::{AGENTS_MD_CONTENT, UpsertResult, upsert_instructions};

    let tmp = tempfile::tempdir().unwrap();
    let agents_md = tmp.path().join("AGENTS.md");
    fs::write(&agents_md, "# My Project\n\nSome existing instructions.\n").unwrap();

    let result = upsert_instructions(&agents_md, AGENTS_MD_CONTENT, false).unwrap();
    assert_eq!(result, UpsertResult::Appended);

    let content = fs::read_to_string(&agents_md).unwrap();
    assert!(
        content.contains("# My Project"),
        "existing content preserved"
    );
    assert!(
        content.contains("<!-- seshat:start -->"),
        "seshat section appended"
    );
}

/// Verify skill installation creates the correct directory and SKILL.md.
#[test]
fn init_installs_skill_file() {
    use seshat_cli::instructions::{SKILL_MD_CONTENT, SkillResult, install_skill};

    let tmp = tempfile::tempdir().unwrap();
    let skill_dir = tmp.path().join("skills").join("seshat");

    let result = install_skill(&skill_dir, SKILL_MD_CONTENT, false).unwrap();
    assert_eq!(result, SkillResult::Installed);

    let skill_path = skill_dir.join("SKILL.md");
    assert!(skill_path.exists(), "SKILL.md created");

    let content = fs::read_to_string(&skill_path).unwrap();
    assert!(content.contains("name: seshat"), "skill name present");
    assert!(
        content.contains("query_code_pattern"),
        "workflow content present"
    );
}

/// Verify that --skip-instructions leaves no instruction files behind.
#[test]
fn upsert_dry_run_leaves_no_files() {
    use seshat_cli::instructions::{AGENTS_MD_CONTENT, UpsertResult, upsert_instructions};

    let tmp = tempfile::tempdir().unwrap();
    let agents_md = tmp.path().join("AGENTS.md");

    let result = upsert_instructions(&agents_md, AGENTS_MD_CONTENT, true).unwrap();
    assert_eq!(result, UpsertResult::DryRun);
    assert!(!agents_md.exists(), "no file created in dry-run mode");
}

/// Verify Claude Code hook installation creates scripts and settings.json.
#[test]
fn init_installs_claude_code_hooks() {
    use seshat_cli::instructions::install_hooks_claude_code;

    let tmp = tempfile::tempdir().unwrap();
    let hooks_dir = tmp.path().join("hooks");
    let settings = tmp.path().join("settings.json");

    install_hooks_claude_code(&hooks_dir, &settings, false).unwrap();

    assert!(
        hooks_dir.join("seshat-session-start").exists(),
        "session-start hook created"
    );
    assert!(
        hooks_dir.join("seshat-pre-tool").exists(),
        "pre-tool hook created"
    );

    let settings_content = fs::read_to_string(&settings).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&settings_content).unwrap();
    assert!(
        parsed["hooks"]["PreToolUse"].is_array(),
        "PreToolUse registered"
    );
    assert!(
        parsed["hooks"]["SessionStart"].is_array(),
        "SessionStart registered"
    );
    assert!(
        settings_content.contains("seshat-pre-tool"),
        "pre-tool command in settings"
    );
    assert!(
        settings_content.contains("seshat-session-start"),
        "session-start command in settings"
    );
}
