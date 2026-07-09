//! Coverage for the VS Code-parity commands: line ops, multi-cursor helpers, matching-bracket
//! motion, and the save-all / close-all / reopen-closed tab lifecycle.

use super::*;
use crossterm::event::KeyModifiers;

/// Feed a single key chord through the real keymap path.
fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
}

#[test]
fn delete_line_via_dispatch() {
    let path = temp_file("a\nb\nc");
    let mut app = app_with(&path);
    app.editor
        .active_document_mut()
        .unwrap()
        .set_caret("a\n".len());
    app.dispatch(Command::DeleteLine);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "a\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn delete_line_chord_ctrl_k_ctrl_k() {
    let path = temp_file("one\ntwo\nthree");
    let mut app = app_with(&path);
    // Ctrl+K arms the chord; Ctrl+K completes Delete Line.
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "two\nthree"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn insert_line_below_and_above_from_keys() {
    let path = temp_file("    x");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(5); // end of "    x"
    key(&mut app, KeyCode::Enter, KeyModifiers::CONTROL); // insert below, copies indent
    key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "    x\n    y"
    );
    key(
        &mut app,
        KeyCode::Enter,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ); // insert above
    key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "    x\n    z\n    y"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn copy_line_up_from_keys() {
    let path = temp_file("a\nb");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(0); // line "a"
    key(
        &mut app,
        KeyCode::Up,
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    );
    assert_eq!(app.editor.active_document().unwrap().to_string(), "a\na\nb");
    std::fs::remove_file(&path).ok();
}

#[test]
fn select_all_matches_then_edit_rewrites_all() {
    let path = temp_file("foo foo bar foo");
    let mut app = app_with(&path);
    // Bare caret inside the first "foo" selects the word, then all occurrences.
    app.editor.active_document_mut().unwrap().set_caret(1);
    app.dispatch(Command::SelectAllMatches);
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 3);
    app.dispatch(Command::InsertText("X".into()));
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "X X bar X"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn cursors_to_line_ends_from_keys() {
    let path = temp_file("aa\nbbb");
    let mut app = app_with(&path);
    app.dispatch(Command::SelectAll);
    key(
        &mut app,
        KeyCode::Char('i'),
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    );
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.selections.len(), 2);
    assert!(doc.selections.ranges().iter().all(|s| s.is_empty()));
    // Typing appends at each line end.
    app.dispatch(Command::InsertText("!".into()));
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "aa!\nbbb!"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn jump_to_matching_bracket_motion() {
    let path = temp_file("x(abc)y");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(1); // on '('
    key(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL);
    // Caret jumps to the matching ')' at offset 5.
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head,
        5
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn trim_trailing_whitespace_command() {
    let path = temp_file("a   \nb\t\nc");
    let mut app = app_with(&path);
    app.dispatch(Command::TrimTrailingWhitespace);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "a\nb\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_all_writes_every_dirty_tab() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b);
    // Dirty both buffers.
    app.editor.workspace.focus_tab(0);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('1'));
    app.editor.workspace.focus_tab(1);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('2'));
    let active_before = app.editor.workspace.active_tab;

    app.dispatch(Command::SaveAll);

    assert_eq!(app.editor.workspace.active_tab, active_before);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "aaa1");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "bbb2");
    assert!(app.editor.workspace.documents.values().all(|d| !d.dirty));
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}

#[test]
fn reopen_closed_tab_restores_last_closed() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b); // b is active (tab 1)
    assert_eq!(app.editor.workspace.tabs.len(), 2);

    // Close the active (clean) tab b, then reopen it.
    app.dispatch(Command::CloseTab);
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    app.dispatch(Command::ReopenClosedTab);
    assert_eq!(app.editor.workspace.tabs.len(), 2);
    assert_eq!(
        app.editor.active_document().unwrap().path.as_deref(),
        Some(b.as_path())
    );
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}

#[test]
fn close_all_closes_clean_tabs() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b);
    app.dispatch(Command::CloseAllTabs);
    assert_eq!(app.editor.workspace.tabs.len(), 0);
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}

#[test]
fn close_all_stops_at_dirty_tab_with_prompt() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b);
    // Make the first tab dirty; the last (clean) closes, the dirty one prompts.
    app.editor.workspace.focus_tab(0);
    app.dispatch(Command::InsertChar('!'));
    app.dispatch(Command::CloseAllTabs);
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    assert!(matches!(
        app.editor.overlay,
        Some(crate::editor::Overlay::ConfirmClose { .. })
    ));
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}
