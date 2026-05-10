//! Integration tests for the `seshat completions` command.
//!
//! Verifies that completion script generation works end-to-end for every
//! shell `clap_complete::Shell` supports, that auto-detection from
//! `$SHELL` produces the right script, and that explicit + detected
//! results agree.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Build a `Command` with a clean environment so tests don't accidentally
/// inherit the developer's shell-related env vars.
///
/// Returns both the command and the [`TempDir`] guard. Keep the guard
/// alive for the test's lifetime — when it drops, the tempdir is
/// removed. The previous `home.keep()` made each test leak a permanent
/// `/tmp/seshat-*` directory, which accumulated forever in CI.
fn seshat() -> (Command, TempDir) {
    let home = tempfile::tempdir().expect("create tempdir for HOME");
    let mut cmd = Command::cargo_bin("seshat").expect("binary exists");
    cmd.env("HOME", home.path());
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env_remove("SHELL");
    // Silence tracing-subscriber so future warn/info logs in the
    // startup path can't pollute stderr-substring assertions.
    cmd.env("SESHAT_LOG", "off");
    (cmd, home)
}

#[test]
fn explicit_bash_emits_seshat_function() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_seshat()"))
        .stdout(predicate::str::contains("COMPREPLY"))
        // Lock the "clean stdout for `eval`-pipes" guarantee: the
        // completions step must not bleed any byte to stderr.
        .stderr(predicate::str::is_empty());
}

#[test]
fn explicit_zsh_emits_compdef() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef seshat"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn explicit_fish_emits_complete_directives() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c seshat"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn explicit_powershell_emits_register_argument_completer() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "powershell"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Register-ArgumentCompleter"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn explicit_elvish_emits_edit_completion_arg() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "elvish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("edit:completion:arg-completer"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn auto_detect_from_shell_env_picks_correct_shell() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
        .env("SHELL", "/usr/local/bin/zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef seshat"));
}

#[test]
fn auto_detect_parses_windows_style_path() {
    // The basename parser is platform-agnostic — it only operates on
    // string content — so we exercise it on every OS regardless of
    // whether the host actually has a PowerShell at that path.
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
        .env("SHELL", r"C:\Program Files\PowerShell\7\pwsh.exe")
        .assert()
        .success()
        .stdout(predicate::str::contains("Register-ArgumentCompleter"));
}

#[test]
fn auto_detect_strips_uppercase_exe_suffix() {
    // Windows filesystems are case-insensitive: `PWSH.EXE` must
    // round-trip through the strip just like `pwsh.exe`.
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
        .env("SHELL", r"C:\WINDOWS\System32\PWSH.EXE")
        .assert()
        .success()
        .stdout(predicate::str::contains("Register-ArgumentCompleter"));
}

#[test]
fn auto_detect_strips_trailing_carriage_return() {
    // CRLF env files leave a trailing `\r` on POSIX runners.
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
        .env("SHELL", "/bin/zsh\r")
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef seshat"));
}

#[test]
fn auto_detect_handles_wrapper_invocation() {
    // Some logins record the shell as a wrapper: take the last
    // whitespace-separated token before path-splitting.
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
        .env("SHELL", "/usr/bin/script /bin/fish")
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c seshat"));
}

#[test]
fn auto_detect_unknown_shell_basename_errors_helpfully() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
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
fn auto_detect_rejects_sh_without_assumption() {
    // /bin/sh is dash on Debian, ash on Alpine — emitting bash
    // completion would silently break when sourced. Verify we no
    // longer assume `sh == bash`.
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
        .env("SHELL", "/bin/sh")
        .assert()
        .failure()
        .stderr(predicate::str::contains("could not auto-detect shell"));
}

#[cfg(unix)]
#[test]
fn auto_detect_without_shell_env_errors_helpfully() {
    let (mut cmd, _home) = seshat();
    cmd.args(["completions"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("$SHELL is unset"));
}

#[test]
fn explicit_overrides_auto_detect() {
    // Even with $SHELL=zsh, asking for bash explicitly must yield bash.
    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "bash"])
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
    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"))
        .stdout(predicate::str::contains("serve"))
        .stdout(predicate::str::contains("review"))
        .stdout(predicate::str::contains("decisions"))
        .stdout(predicate::str::contains("completions"));
}

#[test]
fn help_advertises_completions_subcommand() {
    // Discoverability: a future refactor that accidentally hides the
    // subcommand or mangles its docstring should fail this test.
    let (mut cmd, _home) = seshat();
    cmd.args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("completions"));

    let (mut cmd, _home) = seshat();
    cmd.args(["completions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("auto-detected"));
}
