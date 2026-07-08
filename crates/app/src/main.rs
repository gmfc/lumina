//! `lmn` — the lumina binary. Owns the panic-safe terminal lifecycle, the input event loop,
//! and wiring of the kernel + plugins. Rendering is a pure function of state (plan §4).
#![forbid(unsafe_code)]

mod app;
mod clipboard;
mod commands;
mod completion;
mod config;
mod editor;
mod files;
mod find;
mod git;
mod input;
mod keymap;
mod lsp;
mod picker;
mod search;
mod session;
mod sync;
mod terminal;
mod theme;
mod ui;
mod worker;

use std::io;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::supports_keyboard_enhancement;

use app::App;

fn main() -> Result<()> {
    // Parse a single optional argument. Recognised subcommands/flags are handled before we
    // touch the terminal; anything else is treated as a path (a file or directory) to open.
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--version" | "-V") => {
            println!("lmn {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            print_usage();
            return Ok(());
        }
        Some("update" | "upgrade" | "--update") => {
            return self_update();
        }
        _ => {}
    }

    let mut terminal = ratatui::init();
    // Best-effort enable of mouse capture + bracketed paste; ignore on unsupported terms.
    let _ = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    // The kitty keyboard protocol lets us disambiguate Ctrl+I/Tab, Ctrl+M/Enter, and detect
    // key-release — richer chords for the VS Code-style keymap (plan §5). Only push it where
    // the terminal advertises support, and remember whether we did so we can pop it cleanly.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        );
    }

    let result = App::new(arg).and_then(|mut app| app.run(&mut terminal));

    if enhanced {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();

    result
}

/// Print CLI usage. Kept deliberately small — lumina is a TUI, not a flag-heavy CLI.
fn print_usage() {
    println!(
        "lmn {} — the lumina terminal code editor\n\n\
         USAGE:\n    \
         lmn [PATH]       open PATH (a file or directory); omit for the start screen\n    \
         lmn update       download and install the latest release, in place\n    \
         lmn --version    print the version\n    \
         lmn --help       print this help\n\n\
         EXAMPLES:\n    \
         lmn .            open the current directory\n    \
         lmn src/main.rs  open a file",
        env!("CARGO_PKG_VERSION")
    );
}

/// Update in place by re-running the official installer for this platform, pointed at the
/// directory the running binary lives in so it upgrades *this* install (not a default one).
/// Delegating to the install script keeps a single source of truth and avoids baking an
/// HTTP/TLS/archive stack into the editor. The installers replace the binary atomically, so
/// this is safe to run while another lumina instance is open.
fn self_update() -> Result<()> {
    use std::process::Command;

    // The URL is the raw install script on the default branch — the same entry point the
    // README documents for a fresh install.
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));

    println!("lmn: fetching and installing the latest release…");

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

    let status = cmd
        .status()
        .context("failed to launch the installer (is curl/wget or PowerShell available?)")?;
    if !status.success() {
        anyhow::bail!("update failed: installer exited with {status}");
    }
    Ok(())
}
