use super::*;

#[test]
fn opens_file_into_a_tab() {
    let path = temp_file("hello\nworld\n");
    let app = app_with(&path);
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "hello\nworld\n"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn type_undo_redo_roundtrip() {
    let path = temp_file("");
    let mut app = app_with(&path);
    app.dispatch(Command::InsertChar('h'));
    app.dispatch(Command::InsertChar('i'));
    assert_eq!(app.editor.active_document().unwrap().to_string(), "hi");
    app.dispatch(Command::Undo);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "");
    app.dispatch(Command::Redo);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "hi");
    std::fs::remove_file(&path).ok();
}

#[test]
fn auto_pairs_multi_cursor_dispatch() {
    // plan §1.1 acceptance: three cursors, typing `(` yields three `()` with a caret
    // inside each, routed through the real Command dispatch (auto_pairs on by default).
    let path = temp_file("a\nb\nc\n");
    let mut app = app_with(&path);
    {
        let doc = app.editor.active_document_mut().unwrap();
        doc.selections = editor_core::Selections::from_iter([
            Selection::caret(0),
            Selection::caret(2),
            Selection::caret(4),
        ]);
    }
    app.dispatch(Command::InsertChar('('));
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "()a\n()b\n()c\n");
    assert_eq!(doc.selections.len(), 3);
    for s in doc.selections.ranges() {
        assert!(s.is_empty(), "caret stays a caret");
        assert_eq!(doc.rope().char(s.head), ')', "caret sits before the closer");
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn auto_indent_newline_dispatch() {
    // Auto-indent in isolation (auto-pairs off so the braces aren't auto-closed): a
    // newline after `{` indents the fresh line, and typing `}` on it dedents (plan §1.2).
    let path = temp_file("");
    let mut app = app_with(&path);
    app.config.auto_pairs = false;
    for c in "fn f() {".chars() {
        app.dispatch(Command::InsertChar(c));
    }
    app.dispatch(Command::InsertNewline);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "fn f() {\n    "
    );
    app.dispatch(Command::InsertChar('}'));
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "fn f() {\n}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn new_file_opens_clean_untitled_buffer() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    let before = app.editor.workspace.tabs.len();
    app.dispatch(Command::NewFile);
    assert_eq!(app.editor.workspace.tabs.len(), before + 1);
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "");
    assert!(!doc.dirty, "a fresh buffer is not dirty");
    assert!(doc.path.is_none(), "untitled has no path");
    std::fs::remove_file(&path).ok();
}

#[test]
fn bracket_match_precomputed_into_state() {
    let path = temp_file("a(b)c\n");
    let mut app = app_with(&path);
    // Caret on the '(' at offset 1 → highlight it and its partner ')'.
    app.editor.active_document_mut().unwrap().set_caret(1);
    app.editor.update_bracket_match();
    assert_eq!(app.editor.bracket_match, Some((1, 3)));
    // Caret just after the ')' (offset 4) → matches via the bracket before the caret.
    app.editor.active_document_mut().unwrap().set_caret(4);
    app.editor.update_bracket_match();
    assert_eq!(app.editor.bracket_match, Some((3, 1)));
    // Caret not adjacent to any bracket → None.
    app.editor.active_document_mut().unwrap().set_caret(0);
    app.editor.update_bracket_match();
    assert_eq!(app.editor.bracket_match, None);
    std::fs::remove_file(&path).ok();
}

#[test]
fn multi_cursor_typing_edits_all() {
    let path = temp_file("foo bar foo baz foo");
    let mut app = app_with(&path);
    for _ in 0..3 {
        app.exec_id("cursor.addNextMatch");
    }
    // Replace each selected "foo" by typing.
    app.dispatch(Command::InsertText("X".into()));
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "X bar X baz X"
    );
    std::fs::remove_file(&path).ok();
}
