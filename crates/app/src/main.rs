//! `lumina` — the binary. Owns the panic-safe terminal lifecycle, the input event loop,
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
mod theme;
mod ui;
mod worker;

use std::io;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::supports_keyboard_enhancement;

use app::App;

fn main() -> Result<()> {
    // Parse a single optional path argument (a file or directory to open).
    let arg = std::env::args().nth(1);

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
