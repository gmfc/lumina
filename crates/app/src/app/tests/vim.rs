//! Integration coverage for the Vim modal layer, driven through the real `on_key`
//! path (the same seam the terminal loop uses).
//!
//! This module root holds the shared harness (`vim_app`/`keys`/`esc`/`ctrl` +
//! the `text`/`head`/`mode` accessors) and the basic mode-transition / toggle /
//! status-line smoke tests. The bulk of the coverage lives in the submodules,
//! grouped by concern; each `use super::*` to reach this harness, which in turn
//! `use super::*` for the parent `tests` helpers (`app_with`, `temp_file`,
//! `render_to_string`).

use super::*;
use editor_plugin::VimMode as Mode;

mod ex;
mod motions;
mod operators;
mod registers;
mod visual;

/// An app with the Vim layer enabled on a scratch file.
fn vim_app(contents: &str) -> (App, PathBuf) {
    let path = temp_file(contents);
    let mut app = app_with(&path);
    app.exec_id("vim.enable");
    app.editor.active_document_mut().unwrap().set_caret(0);
    (app, path)
}

/// Feed a run of plain character keys (Normal/Insert mode text).
fn keys(app: &mut App, s: &str) {
    for c in s.chars() {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }
}

fn esc(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

fn ctrl(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

fn text(app: &App) -> String {
    app.editor.active_document().unwrap().to_string()
}

fn head(app: &App) -> usize {
    app.editor
        .active_document()
        .unwrap()
        .selections
        .primary()
        .head
}

fn mode(app: &App) -> Mode {
    app.editor.vim_view.as_ref().unwrap().mode
}

#[test]
fn starts_in_normal_mode() {
    let (app, path) = vim_app("hello");
    assert_eq!(mode(&app), Mode::Normal);
    std::fs::remove_file(&path).ok();
}

#[test]
fn i_inserts_and_esc_returns_to_normal() {
    let (mut app, path) = vim_app("world");
    keys(&mut app, "i");
    assert_eq!(mode(&app), Mode::Insert);
    keys(&mut app, "hello ");
    esc(&mut app);
    assert_eq!(mode(&app), Mode::Normal);
    assert_eq!(text(&app), "hello world");
    std::fs::remove_file(&path).ok();
}

#[test]
fn global_ctrl_s_still_saves_in_normal_mode() {
    let (mut app, path) = vim_app("data");
    keys(&mut app, "x"); // dirty the buffer
    assert!(app.editor.active_document().unwrap().dirty);
    ctrl(&mut app, 's'); // Ctrl+S falls through to the global keymap
    assert!(!app.editor.active_document().unwrap().dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ata");
    std::fs::remove_file(&path).ok();
}

#[test]
fn disabling_vim_restores_plain_typing() {
    let (mut app, path) = vim_app("x");
    app.exec_id("vim.disable");
    assert!(app.editor.vim_view.is_none());
    // With Vim off, a plain char inserts again.
    keys(&mut app, "y");
    assert!(text(&app).contains('y'));
    std::fs::remove_file(&path).ok();
}

#[test]
fn toggle_commands_enable_and_disable() {
    let path = temp_file("hello");
    let mut app = app_with(&path); // Vim off by default
    assert!(app.editor.vim_view.is_none());
    app.exec_id("vim.enable");
    assert!(app.editor.vim_view.is_some());
    app.exec_id("vim.toggle"); // toggles off
    assert!(app.editor.vim_view.is_none());
    app.exec_id("vim.toggle"); // toggles on
    assert!(app.editor.vim_view.is_some());
    app.exec_id("vim.disable");
    assert!(app.editor.vim_view.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn status_line_shows_pending_operator_and_count() {
    let (mut app, path) = vim_app("hello world");
    keys(&mut app, "2d"); // pending: count 2 + operator d
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("2d"),
        "expected pending hint 2d in {screen:?}"
    );
    esc(&mut app);
    keys(&mut app, ":wq"); // command line hint
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains(":wq"),
        "expected command hint in {screen:?}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn status_line_shows_mode_badge() {
    let (mut app, path) = vim_app("hello world");
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("NORMAL"),
        "expected NORMAL badge in {screen:?}"
    );
    keys(&mut app, "v");
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("VISUAL"),
        "expected VISUAL badge in {screen:?}"
    );
    keys(&mut app, "i"); // enters insert? no — in visual, 'i' starts a text object prefix
    esc(&mut app);
    keys(&mut app, "i"); // now Normal -> Insert
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("INSERT"),
        "expected INSERT badge in {screen:?}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn renders_without_panic_in_all_modes() {
    let (mut app, path) = vim_app("alpha beta\ngamma delta");
    keys(&mut app, "v$"); // charwise visual to end of line
    let _ = render_to_string(&mut app, 40, 6);
    esc(&mut app);
    keys(&mut app, "Vj"); // linewise visual across two lines
    let _ = render_to_string(&mut app, 40, 6);
    esc(&mut app);
    keys(&mut app, ":"); // command line open
    let _ = render_to_string(&mut app, 40, 6);
    esc(&mut app);
    keys(&mut app, "/beta"); // search line open
    let _ = render_to_string(&mut app, 40, 6);
    std::fs::remove_file(&path).ok();
}
