//! Integration tests for the `seshat completions` command.
//!
//! Verifies that completion script generation works end-to-end for every
//! shell `clap_complete::Shell` supports, that auto-detection from
//! `$SHELL` produces the right script, and that explicit + detected
//! results agree.

use assert_cmd::Command;
use predicates::prelude::*;

/// Build a `Command` with a clean environment so tests don't accidentally
/// inherit the developer's shell-related env vars.
fn seshat() -> Command {
    let home = tempfile::tempdir().expect("create tempdir for HOME");
    let home_path = home.keep();
    let mut cmd = Command::cargo_bin("seshat").expect("binary exists");
    cmd.env("HOME", &home_path);
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env_remove("SHELL");
    cmd
}

#[test]
fn explicit_bash_emits_seshat_function() {
    seshat()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_seshat()"))
        .stdout(predicate::str::contains("COMPREPLY"));
}

#[test]
fn explicit_zsh_emits_compdef() {
    seshat()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef seshat"));
}

#[test]
fn explicit_fish_emits_complete_directives() {
    seshat()
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c seshat"));
}

#[test]
fn explicit_powershell_emits_register_argument_completer() {
    seshat()
        .args(["completions", "powershell"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Register-ArgumentCompleter"));
}

#[test]
fn explicit_elvish_emits_edit_completion_arg() {
    seshat()
        .args(["completions", "elvish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("edit:completion:arg-completer"));
}

#[test]
fn auto_detect_from_shell_env_picks_correct_shell() {
    seshat()
        .args(["completions"])
        .env("SHELL", "/usr/local/bin/zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef seshat"));
}

#[test]
fn auto_detect_strips_exe_suffix_on_windows_paths() {
    seshat()
        .args(["completions"])
        .env("SHELL", r"C:\Program Files\PowerShell\7\pwsh.exe")
        .assert()
        .success()
        .stdout(predicate::str::contains("Register-ArgumentCompleter"));
}

#[test]
fn auto_detect_unknown_shell_basename_errors_helpfully() {
    seshat()
        .args(["completions"])
        .env("SHELL", "/usr/bin/xonsh")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("could not auto-detect shell")
                .and(predicate::str::contains("xonsh"))
                .and(predicate::str::contains("bash | zsh | fish")),
        );
}

#[test]
fn auto_detect_without_shell_env_errors_helpfully_on_unix() {
    if cfg!(windows) {
        // On Windows we fall back to PowerShell; covered separately.
        return;
    }
    seshat()
        .args(["completions"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("$SHELL is unset"));
}

#[test]
fn explicit_overrides_auto_detect() {
    // Even with $SHELL=zsh, asking for bash explicitly must yield bash.
    seshat()
        .args(["completions", "bash"])
        .env("SHELL", "/bin/zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("_seshat()"))
        .stdout(predicate::str::contains("COMPREPLY"));
}

#[test]
fn completion_script_lists_subcommands() {
    // Sanity: the generated script should reference real subcommands so
    // users actually get suggestions for them.
    seshat()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("review"))
        .stdout(predicate::str::contains("decisions"))
        .stdout(predicate::str::contains("completions"));
}
