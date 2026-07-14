use super::*;

#[test]
fn switching_tabs_dismisses_the_completion_popup() {
    // Regression: DidChangeActive is now emitted on a tab switch (it was declared but never
    // fired), so a stale completion popup clears when the focused document changes.
    let path_a = temp_file("pr");
    let path_b = temp_file("xy");
    let mut app = app_with(&path_a);
    app.dispatch(Command::Move(Motion::DocEnd));
    feed_completion(&mut app, vec![ci("print", 3)]);
    assert!(app.editor.popup.is_some());
    app.open_path(&path_b); // focus a different document
    app.drain_workers(); // emits DidChangeActive → completion plugin dismisses
    assert!(
        app.editor.popup.is_none(),
        "popup should clear on tab switch"
    );
    std::fs::remove_file(&path_a).ok();
    std::fs::remove_file(&path_b).ok();
}

#[test]
fn snippet_completion_expands_with_tabstop_cursor() {
    // Accepting a snippet item expands its grammar ($1/$0 stripped) and lands the caret on the
    // first tabstop instead of leaving a literal `$1` in the buffer.
    let path = temp_file("pri");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    let snippet_item = editor_plugin::LspCompletionItem {
        label: "println!".into(),
        detail: None,
        insert_text: "println!($1)$0".into(),
        kind: Some(3),
        additional_edits: Vec::new(),
        is_snippet: true,
        data: None,
        command: None,
    };
    feed_completion(&mut app, vec![snippet_item]);
    app.on_key(KeyEvent::from(KeyCode::Enter)); // accept
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "println!()");
    assert_eq!(doc.selections.primary().head, 9); // between the parens ($1)
    std::fs::remove_file(&path).ok();
}

#[test]
fn completion_accept_replaces_typed_prefix() {
    // Feed one item, accept it, and confirm it replaces the identifier prefix under the caret —
    // the `completion` plugin's accept path (apply_transaction over the real edit).
    let path = temp_file("pri");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    feed_completion(&mut app, vec![ci("println!", 3)]);
    assert!(app.editor.popup.is_some());
    app.on_key(KeyEvent::from(KeyCode::Enter)); // accept
    assert!(app.editor.popup.is_none());
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "println!"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn completion_popup_navigates_and_accepts() {
    let path = temp_file("pr");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    feed_completion(
        &mut app,
        vec![ci("print", 3), ci("println", 3), ci("procedure", 3)],
    );
    assert!(app.editor.popup.is_some());
    // Down selects the 2nd row ("println"); Enter accepts and replaces the typed "pr".
    app.on_key(KeyEvent::from(KeyCode::Down));
    app.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(app.editor.popup.is_none());
    assert_eq!(app.editor.active_document().unwrap().to_string(), "println");
    std::fs::remove_file(&path).ok();
}

#[test]
fn completion_filters_as_you_type_then_dismisses() {
    let path = temp_file("p");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    feed_completion(
        &mut app,
        vec![ci("print", 3), ci("procedure", 3), ci("foo", 3)],
    );
    assert_eq!(popup_rows(&app), 2); // print, procedure (foo doesn't match "p")
    app.on_key(KeyEvent::from(KeyCode::Char('r'))); // "pr" → a char falls through to editing
    assert_eq!(app.editor.active_document().unwrap().to_string(), "pr");
    assert_eq!(popup_rows(&app), 2);
    app.on_key(KeyEvent::from(KeyCode::Char('i'))); // "pri" → only print
    assert_eq!(popup_rows(&app), 1);
    // A non-identifier char leaves the word and dismisses the popup.
    app.on_key(KeyEvent::from(KeyCode::Char(' ')));
    assert!(app.editor.popup.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn completion_esc_dismisses_without_editing() {
    let path = temp_file("pr");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    feed_completion(&mut app, vec![ci("print", 3)]);
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(app.editor.popup.is_none());
    assert_eq!(app.editor.active_document().unwrap().to_string(), "pr");
    std::fs::remove_file(&path).ok();
}

#[test]
fn completion_resolved_edits_apply_to_the_document() {
    // handle_lsp_event(CompletionResolvedEdits) → on_completion_resolved_edits applies the late
    // auto-import edits to the doc they were resolved for.
    let path = temp_rs_file("use x;\n");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::CompletionResolvedEdits {
        uri: crate::lsp::uri_for(&path),
        edits: vec![editor_lsp::TextEdit {
            start_line: 0,
            start_char16: 0,
            end_line: 0,
            end_char16: 0,
            new_text: "// import\n".into(),
        }],
    });
    app.drain_workers();
    assert!(
        app.editor
            .active_document()
            .unwrap()
            .to_string()
            .starts_with("// import"),
        "the resolved edit should be inserted"
    );
    std::fs::remove_file(&path).ok();
}
