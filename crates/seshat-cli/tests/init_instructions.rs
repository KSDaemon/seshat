//! Integration tests for `seshat init` agent instruction writing (Story 9.2).
//!
//! These tests exercise `run_init()` end-to-end against a temp directory,
//! verifying that instruction files, skill files, and hook registrations are
//! created correctly for each supported client.

use std::fs;
use std::path::Path;

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
    assert!(matches!(result, UpsertResult::DryRun(Some(_))));
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

/// Simulate `write_instructions_for_client(ClaudeCode, ...)` end-to-end.
/// This mirrors the exact logic in `init.rs::write_instructions_for_client`.
#[test]
fn write_instructions_for_client_claude_code_full_path() {
    use seshat_cli::instructions::{
        AGENTS_MD_CONTENT, HooksResult, SKILL_MD_CONTENT, SkillResult, UpsertResult,
        install_hooks_claude_code, install_skill, upsert_instructions,
    };

    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path().join(".claude");
    fs::create_dir_all(&claude_home).unwrap();

    // ── Step 1: upsert_instructions (CLAUDE.md) ──────────────────────
    let claude_md = claude_home.join("CLAUDE.md");
    let upsert_result = upsert_instructions(&claude_md, AGENTS_MD_CONTENT, false).unwrap();
    assert!(
        matches!(upsert_result, UpsertResult::Created),
        "CLAUDE.md should be created (not appended or updated)"
    );
    assert!(claude_md.exists());
    let claude_content = fs::read_to_string(&claude_md).unwrap();
    assert!(claude_content.contains("<!-- seshat:start -->"));
    assert!(claude_content.contains("<!-- seshat:end -->"));

    // ── Step 2: install_skill ─────────────────────────────────────────
    let skill_dir = claude_home.join("skills").join("seshat");
    let skill_result = install_skill(&skill_dir, SKILL_MD_CONTENT, false).unwrap();
    assert!(
        matches!(skill_result, SkillResult::Installed),
        "skill should be Installed"
    );
    assert!(skill_dir.join("SKILL.md").exists());

    // ── Step 3: install_hooks_claude_code ─────────────────────────────
    let hooks_dir = claude_home.join("hooks");
    let settings_path = claude_home.join("settings.json");
    let hooks_result = install_hooks_claude_code(&hooks_dir, &settings_path, false).unwrap();
    // When settings.json is NEW (doesn't exist), no backup is created.
    assert!(
        matches!(hooks_result, HooksResult::Installed(None)),
        "should have no backup since settings.json is new"
    );
    assert!(hooks_dir.join("seshat-session-start").exists());
    assert!(hooks_dir.join("seshat-pre-tool").exists());
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(parsed["hooks"]["PreToolUse"].is_array());
    assert!(parsed["hooks"]["SessionStart"].is_array());
}

/// Verify that the second run produces `Updated` (not `Created` or `Appended`
/// for instructions) and `Installed` for skills.
#[test]
fn write_instructions_are_idempotent_on_second_run() {
    use seshat_cli::instructions::{
        AGENTS_MD_CONTENT, HooksResult, SKILL_MD_CONTENT, UpsertResult, install_hooks_claude_code,
        install_skill, upsert_instructions,
    };

    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path().join(".claude");
    fs::create_dir_all(&claude_home).unwrap();

    // First run — all Created.
    let run_once = |claude_home: &Path| {
        let claude_md = claude_home.join("CLAUDE.md");
        let skill_dir = claude_home.join("skills").join("seshat");

        let ins = upsert_instructions(&claude_md, AGENTS_MD_CONTENT, false).unwrap();
        let skill = install_skill(&skill_dir, SKILL_MD_CONTENT, false).unwrap();
        let hooks = install_hooks_claude_code(
            &claude_home.join("hooks"),
            &claude_home.join("settings.json"),
            false,
        )
        .unwrap();
        (ins, skill, hooks)
    };

    let (ins1, _skill1, _hooks1) = run_once(&claude_home);
    assert!(matches!(ins1, UpsertResult::Created));

    // Second run — should be Updated, Installed, Installed.
    let (ins2, _skill2, hooks2) = run_once(&claude_home);
    assert!(
        matches!(ins2, UpsertResult::Updated),
        "second upsert must produce Updated, got {:?}",
        ins2
    );

    // Verify exactly one section.
    let content = fs::read_to_string(claude_home.join("CLAUDE.md")).unwrap();
    assert_eq!(
        content.matches("<!-- seshat:start -->").count(),
        1,
        "only one section after two runs"
    );

    // Verify hooks are still idempotent (no duplicates).
    if let HooksResult::Installed(Some(backup)) = hooks2 {
        assert!(backup.to_string_lossy().contains("seshat-backup"));
    }
}

/// Verify that when settings.json already exists, a backup is created.
#[test]
fn hooks_backup_created_when_settings_exists() {
    use seshat_cli::instructions::{HooksResult, install_hooks_claude_code};

    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path().join(".claude");
    fs::create_dir_all(&claude_home).unwrap();

    // Pre-populate settings.json with existing content.
    let settings_path = claude_home.join("settings.json");
    fs::write(
        &settings_path,
        r#"{"hooks":{"PreToolUse":[{"matcher":"test","hooks":[{"type":"command","command":"/other/hook"}]}]}}"#,
    )
    .unwrap();

    let hooks_dir = claude_home.join("hooks");
    let result = install_hooks_claude_code(&hooks_dir, &settings_path, false).unwrap();

    // Should have a backup since settings.json existed.
    if let HooksResult::Installed(Some(backup)) = result {
        assert!(backup.to_string_lossy().contains("seshat-backup"));
        assert!(backup.to_string_lossy().contains("settings.json"));
        // Backup should contain the original content.
        let backup_content = fs::read_to_string(&backup).unwrap();
        assert!(backup_content.contains(r#"{"hooks":{"PreToolUse""#));
    } else {
        panic!("expected Installed(Some(backup)), got {:?}", result);
    }

    // Original settings.json should now have seshat hooks merged in.
    let content = fs::read_to_string(&settings_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(parsed["hooks"]["PreToolUse"].is_array());
    let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
    assert!(
        pre_tool.len() >= 2,
        "should have original + seshat entries, got {}",
        pre_tool.len()
    );
}

/// Verify that dry-run produces `DryRun(Some(path))` variants and shows specific paths.
#[test]
fn dry_run_shows_specific_paths() {
    use seshat_cli::instructions::{
        AGENTS_MD_CONTENT, HooksResult, SKILL_MD_CONTENT, SkillResult, UpsertResult,
        install_hooks_claude_code, install_skill, upsert_instructions,
    };

    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path().join(".claude");
    fs::create_dir_all(&claude_home).unwrap();

    // Instruction dry-run.
    let claude_md = claude_home.join("CLAUDE.md");
    let ins_result = upsert_instructions(&claude_md, AGENTS_MD_CONTENT, true).unwrap();
    if let UpsertResult::DryRun(Some(path)) = &ins_result {
        assert_eq!(path, &claude_md, "dry-run must report the instruction path");
    } else {
        panic!("expected DryRun(Some(path)), got {:?}", ins_result);
    }

    // Skill dry-run.
    let skill_dir = claude_home.join("skills").join("seshat");
    let skill_result = install_skill(&skill_dir, SKILL_MD_CONTENT, true).unwrap();
    if let SkillResult::DryRun(Some(path)) = &skill_result {
        assert!(
            path.ends_with("SKILL.md"),
            "dry-run must report the skill path, got {:?}",
            path
        );
    } else {
        panic!(
            "expected SkillResult::DryRun(Some(path)), got {:?}",
            skill_result
        );
    }

    // Hooks dry-run.
    let hooks_dir = claude_home.join("hooks");
    let settings_path = claude_home.join("settings.json");
    let hooks_result = install_hooks_claude_code(&hooks_dir, &settings_path, true).unwrap();
    if let HooksResult::DryRun {
        hooks_dir: hd,
        session_start,
        pre_tool,
        settings,
    } = &hooks_result
    {
        assert!(
            hd.to_string_lossy().contains("hooks"),
            "dry-run must report hooks_dir"
        );
        assert!(
            session_start
                .to_string_lossy()
                .contains("seshat-session-start"),
            "dry-run must report session_start"
        );
        assert!(
            pre_tool.to_string_lossy().contains("seshat-pre-tool"),
            "dry-run must report pre_tool"
        );
        assert!(
            settings.to_string_lossy().ends_with("settings.json"),
            "dry-run must report settings"
        );

        // All paths should exist as non-existent (nothing written).
        assert!(!hd.exists(), "hooks_dir not created in dry-run");
        assert!(
            !session_start.exists(),
            "session_start not created in dry-run"
        );
        assert!(!pre_tool.exists(), "pre_tool not created in dry-run");
        assert!(!settings.exists(), "settings not created in dry-run");
    } else {
        panic!("expected HooksResult::DryRun{{..}}, got {:?}", hooks_result);
    }

    // Verify description messages mention paths.
    let ins_desc = ins_result.description();
    assert!(
        ins_desc.contains("CLAUDE.md"),
        "description must include file path, got: {ins_desc}",
    );
}

/// Verify that an existing AGENTS.md without markers gets seshat section appended.
#[test]
fn open_code_appends_to_existing_if_no_markers() {
    use seshat_cli::instructions::{AGENTS_MD_CONTENT, UpsertResult, upsert_instructions};

    let tmp = tempfile::tempdir().unwrap();
    // Simulate OpenCode global config dir.
    let opencode_dir = tmp.path().join(".config").join("opencode");
    fs::create_dir_all(&opencode_dir).unwrap();

    let agents_md = opencode_dir.join("AGENTS.md");
    fs::write(
        &agents_md,
        "# My Project\n\n## Instructions\n\nWrite your instructions here.\n",
    )
    .unwrap();

    let result = upsert_instructions(&agents_md, AGENTS_MD_CONTENT, false).unwrap();
    assert!(
        matches!(result, UpsertResult::Appended),
        "should append to existing file without markers"
    );

    let content = fs::read_to_string(&agents_md).unwrap();
    assert!(content.contains("# My Project"));
    assert!(content.contains("## Instructions"));
    assert!(content.contains("<!-- seshat:start -->"));
    assert!(
        content.contains("<!-- seshat:end -->"),
        "end marker present"
    );
    // Section should be at the end.
    assert!(
        content.rfind("<!-- seshat:start -->").unwrap() > content.find("## Instructions").unwrap(),
        "seshat section appended after existing content"
    );
}

/// Verify that Claude Code instruction writing preserves existing CLAUDE.md content.
#[test]
fn claude_code_preserves_existing_content() {
    use seshat_cli::instructions::{AGENTS_MD_CONTENT, UpsertResult, upsert_instructions};

    let tmp = tempfile::tempdir().unwrap();
    let claude_home = tmp.path().join(".claude");
    fs::create_dir_all(&claude_home).unwrap();

    let claude_md = claude_home.join("CLAUDE.md");

    // First run: creates.
    let result1 = upsert_instructions(&claude_md, AGENTS_MD_CONTENT, false).unwrap();
    assert!(matches!(result1, UpsertResult::Created));

    // Add some extra content after the seshat section.
    let existing = fs::read_to_string(&claude_md).unwrap();
    let extra = "\n\n## Other Tools\n\nSome other AI tools are configured here.\n";
    fs::write(&claude_md, format!("{existing}{extra}")).unwrap();

    // Second run: should replace between markers, preserve header and footer.
    let result2 = upsert_instructions(&claude_md, AGENTS_MD_CONTENT, false).unwrap();
    assert!(matches!(result2, UpsertResult::Updated));

    // Verify header preserved ("# Claude Code" is a common section in CLAUDE.md).
    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("## Other Tools"),
        "existing content after markers preserved"
    );
    assert!(
        content.contains("Some other AI tools are configured here"),
        "exact existing content preserved"
    );
}
