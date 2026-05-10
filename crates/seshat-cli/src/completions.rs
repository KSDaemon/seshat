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
/// basename to a [`Shell`] variant. Treats an empty / whitespace-only
/// `$SHELL` as if it were unset. On Windows, falls back to
/// [`Shell::PowerShell`] only when `$SHELL` is genuinely unset — a
/// `$SHELL` that's set but unparseable is an error worth surfacing
/// instead of masking with the platform default.
fn detect_shell() -> Result<Shell, CliError> {
    let raw_set = std::env::var("SHELL").ok();
    // Trim whitespace and CR (CRLF env files leave a trailing `\r` on
    // POSIX systems). Normalise empty-after-trim to "unset" semantics.
    let raw = raw_set
        .as_deref()
        .map(|s| s.trim().trim_end_matches('\r'))
        .filter(|s| !s.is_empty());

    if let Some(raw) = raw {
        if let Some(name) = shell_basename(raw) {
            if let Some(shell) = map_shell_name(name) {
                return Ok(shell);
            }
            return Err(CliError::InvalidArgument(format!(
                "could not auto-detect shell from $SHELL={raw:?} (basename {name:?}). \
                 Pass one explicitly: bash | zsh | fish | powershell | elvish"
            )));
        }
        // `$SHELL` is set but parsing failed (no basename). Surface the
        // raw value so the user can see what we choked on.
        return Err(CliError::InvalidArgument(format!(
            "could not auto-detect shell from $SHELL={raw:?} (no basename). \
             Pass one explicitly: bash | zsh | fish | powershell | elvish"
        )));
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
/// to `pwsh`. Some login shells are recorded as a wrapper invocation
/// (`/usr/bin/script /bin/zsh ...`) — fall back to the last
/// whitespace-separated token before path-splitting.
fn shell_basename(path: &str) -> Option<&str> {
    // Wrapper invocations: take the last whitespace-separated token.
    let last_token = path
        .split_ascii_whitespace()
        .next_back()
        .filter(|s| !s.is_empty())?;
    let name = last_token
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())?;
    // Case-insensitive `.exe` strip — Windows filesystems are
    // case-insensitive so `PWSH.EXE` / `pwsh.Exe` must round-trip.
    let trimmed = if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    };
    Some(trimmed)
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
    fn shell_basename_strips_uppercase_exe() {
        // Windows filesystems are case-insensitive; .EXE / .Exe must
        // round-trip through the strip, otherwise the lowercased name
        // ("pwsh.exe") never matches map_shell_name's keys.
        assert_eq!(
            shell_basename(r"C:\WINDOWS\System32\PWSH.EXE"),
            Some("PWSH"),
        );
        assert_eq!(shell_basename(r"C:\bin\Bash.Exe"), Some("Bash"));
    }

    #[test]
    fn shell_basename_handles_bare_name() {
        assert_eq!(shell_basename("zsh"), Some("zsh"));
    }

    #[test]
    fn shell_basename_handles_wrapper_invocation() {
        // Some logins record the shell as a wrapper invocation
        // (`script(1)` capturing a session, `env`, `nice`, etc.).
        // Take the last whitespace-separated token before path-splitting.
        assert_eq!(shell_basename("/usr/bin/script /bin/zsh"), Some("zsh"));
        assert_eq!(shell_basename("nice -n 19 /usr/bin/fish"), Some("fish"));
    }

    #[test]
    fn shell_basename_rejects_empty_and_trailing_separator() {
        assert_eq!(shell_basename(""), None);
        assert_eq!(shell_basename("/bin/"), None);
        assert_eq!(shell_basename("   "), None);
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
