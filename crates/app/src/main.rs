//! `lmn` — the lumina binary. Owns the panic-safe terminal lifecycle, the input event loop,
//! and wiring of the kernel + plugins. Rendering is a pure function of state (plan §4).

mod app;
mod cli;
mod clipboard;
mod commands;
mod config;
mod editor;
mod files;
mod git;
mod input;
mod keymap;
mod lsp;
mod picker;
mod session;
mod settings;
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
    match cli::parse_cli(arg.as_deref()) {
        cli::Cli::Version => {
            println!("{}", cli::version_line());
            return Ok(());
        }
        cli::Cli::Help => {
            println!("{}", cli::usage());
            return Ok(());
        }
        cli::Cli::Update => return self_update(),
        cli::Cli::Open(_) => {}
    }

    let mut terminal = ratatui::init();
    // ratatui's panic hook restores raw mode + the alternate screen, but not the extra input
    // modes we enable below. Chain a hook that also disables mouse capture / bracketed paste /
    // keyboard-enhancement flags, so a panic doesn't leave the user's shell echoing raw mouse
    // escapes and mangling pastes.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        prev_hook(info);
    }));
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

/// Update in place by spawning the platform installer (built by [`cli::build_update_command`])
/// pointed at this binary's own directory. This is I/O glue — the command's shape is unit-tested
/// in `cli`; here we only run it and surface a clear error. The installers replace the binary
/// atomically, so this is safe to run while another lumina instance is open.
fn self_update() -> Result<()> {
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));

    println!("lmn: fetching and installing the latest release…");

    let status = cli::build_update_command(install_dir)
        .status()
        .context("failed to launch the installer (is curl/wget or PowerShell available?)")?;
    if !status.success() {
        anyhow::bail!("update failed: installer exited with {status}");
    }
    Ok(())
}
