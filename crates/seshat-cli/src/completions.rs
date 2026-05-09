//! `seshat completions` — print a shell completion script to stdout.
//!
//! When invoked without an explicit `<shell>` argument, the target is
//! auto-detected from the `$SHELL` environment variable (basename of the
//! login shell). On Windows we fall back to PowerShell when `$SHELL` is
//! unset. If detection fails we return [`CliError::InvalidArgument`] with
//! a friendly hint listing the supported shells.

use std::io;

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::args::Cli;
use crate::error::CliError;

/// Print the completion script for `shell` (or the auto-detected current
/// shell) to stdout.
pub fn run_completions(shell: Option<Shell>) -> Result<(), CliError> {
    let shell = match shell {
        Some(s) => s,
        None => detect_shell()?,
    };

    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_owned();
    generate(shell, &mut cmd, bin_name, &mut io::stdout());
    Ok(())
}

/// Auto-detect the running shell from environment.
///
/// Checks `$SHELL` first (POSIX login-shell convention) and maps the
/// basename to a [`Shell`] variant. On Windows, falls back to
/// [`Shell::PowerShell`] when `$SHELL` is unset.
fn detect_shell() -> Result<Shell, CliError> {
    if let Ok(raw) = std::env::var("SHELL") {
        if let Some(name) = shell_basename(&raw) {
            if let Some(shell) = map_shell_name(name) {
                return Ok(shell);
            }
            return Err(CliError::InvalidArgument(format!(
                "could not auto-detect shell from $SHELL='{raw}' (basename '{name}'). \
                 Pass one explicitly: bash | zsh | fish | powershell | elvish"
            )));
        }
    }

    if cfg!(windows) {
        return Ok(Shell::PowerShell);
    }

    Err(CliError::InvalidArgument(
        "could not auto-detect shell ($SHELL is unset). \
         Pass one explicitly: bash | zsh | fish | powershell | elvish"
            .to_owned(),
    ))
}

/// Extract the executable basename from a shell path, stripping any
/// trailing `.exe` so that `C:\Program Files\PowerShell\7\pwsh.exe` maps
/// to `pwsh`.
fn shell_basename(path: &str) -> Option<&str> {
    let name = path.rsplit(['/', '\\']).next().filter(|s| !s.is_empty())?;
    Some(name.strip_suffix(".exe").unwrap_or(name))
}

/// Map a shell basename (`bash`, `zsh`, `pwsh`, ...) to the [`Shell`]
/// variant clap_complete understands.
fn map_shell_name(name: &str) -> Option<Shell> {
    match name.to_ascii_lowercase().as_str() {
        "bash" | "sh" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        "pwsh" | "powershell" => Some(Shell::PowerShell),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_basename_strips_unix_path() {
        assert_eq!(shell_basename("/bin/zsh"), Some("zsh"));
        assert_eq!(shell_basename("/usr/local/bin/fish"), Some("fish"));
    }

    #[test]
    fn shell_basename_strips_windows_path_and_exe() {
        assert_eq!(
            shell_basename(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            Some("pwsh"),
        );
    }

    #[test]
    fn shell_basename_handles_bare_name() {
        assert_eq!(shell_basename("zsh"), Some("zsh"));
    }

    #[test]
    fn shell_basename_rejects_empty_and_trailing_separator() {
        assert_eq!(shell_basename(""), None);
        assert_eq!(shell_basename("/bin/"), None);
    }

    #[test]
    fn map_shell_name_known_shells() {
        assert_eq!(map_shell_name("bash"), Some(Shell::Bash));
        assert_eq!(map_shell_name("zsh"), Some(Shell::Zsh));
        assert_eq!(map_shell_name("fish"), Some(Shell::Fish));
        assert_eq!(map_shell_name("elvish"), Some(Shell::Elvish));
        assert_eq!(map_shell_name("pwsh"), Some(Shell::PowerShell));
        assert_eq!(map_shell_name("powershell"), Some(Shell::PowerShell));
        assert_eq!(map_shell_name("PowerShell"), Some(Shell::PowerShell));
    }

    #[test]
    fn map_shell_name_unknown_shell() {
        assert_eq!(map_shell_name("nu"), None);
        assert_eq!(map_shell_name("xonsh"), None);
        assert_eq!(map_shell_name(""), None);
    }
}
