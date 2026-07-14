//! Ex-line (`:`) coverage — goto-line, write/quit variants, unknown-command
//! reporting, backspace-closes — plus `:substitute` and the interactive
//! `/`/`?` search-command prompts.

use super::*;

#[test]
fn ex_goto_line() {
    let (mut app, path) = vim_app("a\nb\nc\nd");
    keys(&mut app, ":3");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app)),
        2
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_substitute_global() {
    let (mut app, path) = vim_app("a a a\nb a");
    keys(&mut app, ":%s/a/X/g");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(text(&app), "X X X\nb X");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_write_saves() {
    let (mut app, path) = vim_app("data");
    keys(&mut app, "x"); // dirty
    keys(&mut app, ":w");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.editor.active_document().unwrap().dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ata");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_quit_closes_tab() {
    let (mut app, path) = vim_app("bye");
    keys(&mut app, ":q");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.editor.workspace.tabs.len(), 0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_force_quit_discards() {
    let (mut app, path) = vim_app("bye");
    keys(&mut app, "x"); // dirty it
    keys(&mut app, ":q!");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.editor.workspace.tabs.len(), 0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_quit_all_sets_quit() {
    let (mut app, path) = vim_app("x");
    keys(&mut app, ":qa");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.quit);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_unknown_command_reports() {
    let (mut app, path) = vim_app("x");
    keys(&mut app, ":frobnicate");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app
        .editor
        .status_message
        .as_deref()
        .unwrap_or_default()
        .contains("Not an editor command"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_substitute_current_line() {
    let (mut app, path) = vim_app("a a a\nb b");
    keys(&mut app, ":s/a/Z/g"); // current line only
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(text(&app), "Z Z Z\nb b");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_command_backspace_closes_when_empty() {
    let (mut app, path) = vim_app("x");
    keys(&mut app, ":");
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    // Command line closed; a subsequent motion works in Normal mode again.
    keys(&mut app, "l");
    assert_eq!(mode(&app), Mode::Normal);
    std::fs::remove_file(&path).ok();
}

#[test]
fn search_moves_to_match() {
    let (mut app, path) = vim_app("one two three two");
    keys(&mut app, "/two");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(head(&app), 4); // first 'two'
    keys(&mut app, "n"); // next match
    assert_eq!(head(&app), 14); // second 'two'
    std::fs::remove_file(&path).ok();
}

#[test]
fn search_backward_and_prev() {
    let (mut app, path) = vim_app("x y x y x");
    keys(&mut app, "$"); // on last 'x'
    keys(&mut app, "?x"); // search backward for 'x'
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(head(&app), 4); // middle 'x'
    keys(&mut app, "N"); // reverse -> forward -> last 'x'
    assert_eq!(head(&app), 8);
    std::fs::remove_file(&path).ok();
}
