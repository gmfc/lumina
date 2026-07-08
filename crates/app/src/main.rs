//! `lumina` — the binary. Owns the panic-safe terminal lifecycle, the input event loop,
//! and wiring of the kernel + plugins. Rendering is a pure function of state (plan §4).
#![forbid(unsafe_code)]

mod app;
mod commands;
mod config;
mod editor;
mod files;
mod find;
mod input;
mod keymap;
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
};
use crossterm::execute;

use app::App;

fn main() -> Result<()> {
    // Parse a single optional path argument (a file or directory to open).
    let arg = std::env::args().nth(1);

    let mut terminal = ratatui::init();
    // Best-effort enable of mouse capture + bracketed paste; ignore on unsupported terms.
    let _ = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);

    let result = App::new(arg).and_then(|mut app| app.run(&mut terminal));

    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();

    result
}
