//! The `lmn` command-line surface: argument classification, the `--help`/`--version` text,
//! and construction of the self-update command. This is the *pure*, unit-testable half of
//! the binary — the terminal lifecycle and the actual process spawn live in `main.rs`.

use std::path::PathBuf;
use std::process::Command;

/// A classified top-level invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum Cli {
    /// `--version` / `-V`
    Version,
    /// `--help` / `-h`
    Help,
    /// `update` / `upgrade` / `--update`
    Update,
    /// Open an optional path (a file or directory); `None` shows the start screen.
    Open(Option<String>),
}

/// Classify the first CLI argument. Anything that isn't a recognised flag or subcommand is
/// treated as a path to open — so `lmn .` opens the current directory, just like `vim .`.
pub fn parse_cli(arg: Option<&str>) -> Cli {
    match arg {
        Some("--version" | "-V") => Cli::Version,
        Some("--help" | "-h") => Cli::Help,
        Some("update" | "upgrade" | "--update") => Cli::Update,
        other => Cli::Open(other.map(str::to_owned)),
    }
}

/// The `lmn --version` line.
pub fn version_line() -> String {
    format!("lmn {}", env!("CARGO_PKG_VERSION"))
}

/// The `lmn --help` text. Deliberately small — lumina is a TUI, not a flag-heavy CLI.
pub fn usage() -> String {
    format!(
        "{version} — the lumina terminal code editor\n\n\
         USAGE:\n    \
         lmn [PATH]       open PATH (a file or directory); omit for the start screen\n    \
         lmn update       download and install the latest release, in place\n    \
         lmn --version    print the version\n    \
         lmn --help       print this help\n\n\
         EXAMPLES:\n    \
         lmn .            open the current directory\n    \
         lmn src/main.rs  open a file",
        version = version_line()
    )
}

/// Build the platform installer command used by `lmn update`, pointed at `install_dir` (the
/// directory the running binary lives in) so it upgrades *this* install rather than a default
/// location. Kept separate from the spawn so its shape can be asserted in tests without ever
/// running the installer. Delegating to the install script keeps a single source of truth and
/// avoids baking an HTTP/TLS/archive stack into the editor.
pub fn build_update_command(install_dir: Option<PathBuf>) -> Command {
    #[cfg(windows)]
    let mut cmd = {
        const URL: &str = "https://raw.githubusercontent.com/gmfc/lumina/main/install.ps1";
        let mut c = Command::new("powershell");
        c.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!("irm {URL} | iex"),
        ]);
        c
    };

    #[cfg(not(windows))]
    let mut cmd = {
        const URL: &str = "https://raw.githubusercontent.com/gmfc/lumina/main/install.sh";
        let script = format!(
            "if command -v curl >/dev/null 2>&1; then curl -fsSL {URL} | sh; \
             else wget -qO- {URL} | sh; fi"
        );
        let mut c = Command::new("sh");
        c.arg("-c").arg(script);
        c
    };

    if let Some(dir) = install_dir {
        cmd.env("LMN_INSTALL_DIR", dir);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn flags_and_subcommands_are_recognised() {
        assert_eq!(parse_cli(Some("--version")), Cli::Version);
        assert_eq!(parse_cli(Some("-V")), Cli::Version);
        assert_eq!(parse_cli(Some("--help")), Cli::Help);
        assert_eq!(parse_cli(Some("-h")), Cli::Help);
        assert_eq!(parse_cli(Some("update")), Cli::Update);
        assert_eq!(parse_cli(Some("upgrade")), Cli::Update);
        assert_eq!(parse_cli(Some("--update")), Cli::Update);
    }

    #[test]
    fn a_path_argument_is_opened_not_mistaken_for_a_flag() {
        assert_eq!(parse_cli(Some(".")), Cli::Open(Some(".".to_owned())));
        assert_eq!(
            parse_cli(Some("src/main.rs")),
            Cli::Open(Some("src/main.rs".to_owned()))
        );
        assert_eq!(parse_cli(None), Cli::Open(None));
    }

    #[test]
    fn version_and_usage_describe_the_binary_and_commands() {
        assert!(version_line().starts_with("lmn "));
        assert!(version_line().contains(env!("CARGO_PKG_VERSION")));
        let usage = usage();
        assert!(usage.contains("lmn ."));
        assert!(usage.contains("update"));
        assert!(usage.contains("--version"));
    }

    #[test]
    fn update_command_carries_install_dir_and_targets_the_installer() {
        let dir = PathBuf::from("/opt/lmn/bin");
        let cmd = build_update_command(Some(dir.clone()));

        let carries_dir = cmd
            .get_envs()
            .any(|(k, v)| k == OsStr::new("LMN_INSTALL_DIR") && v == Some(dir.as_os_str()));
        assert!(carries_dir, "update must tell the installer where to write");

        let program = cmd.get_program().to_string_lossy().into_owned();
        let joined = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");

        if cfg!(windows) {
            assert_eq!(program, "powershell");
            assert!(joined.contains("install.ps1"));
            assert!(joined.contains("iex"));
        } else {
            assert_eq!(program, "sh");
            assert!(joined.contains("install.sh"));
            assert!(joined.contains("curl") || joined.contains("wget"));
        }
    }

    #[test]
    fn update_command_without_install_dir_sets_no_env() {
        let cmd = build_update_command(None);
        assert!(cmd.get_envs().next().is_none());
    }
}
