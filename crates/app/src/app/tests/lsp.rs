use super::*;

#[test]
fn workspace_edit_applies_rename_across_occurrences() {
    let path = temp_file("let foo = foo + 1;");
    let mut app = app_with(&path);
    let uri = crate::lsp::uri_for(&path);
    let edit = editor_lsp::WorkspaceEdit {
        changes: vec![(
            uri,
            vec![
                editor_lsp::TextEdit {
                    start_line: 0,
                    start_char16: 4,
                    end_line: 0,
                    end_char16: 7,
                    new_text: "bar".into(),
                },
                editor_lsp::TextEdit {
                    start_line: 0,
                    start_char16: 10,
                    end_line: 0,
                    end_char16: 13,
                    new_text: "bar".into(),
                },
            ],
        )],
    };
    app.apply_workspace_edit(edit);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "let bar = bar + 1;"
    );
}

#[test]
fn completion_replaces_typed_prefix() {
    let path = temp_file("pri");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.insert_completion("println!");
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "println!"
    );
}

#[test]
fn completion_popup_navigates_and_accepts() {
    let path = temp_file("pr");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.open_completion(vec![ci("print", 3), ci("println", 3), ci("procedure", 3)]);
    assert!(app.editor.completion.is_some());
    // Down selects the 2nd item ("println"); Enter accepts and replaces the typed "pr".
    app.on_key(KeyEvent::from(KeyCode::Down));
    app.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(app.editor.completion.is_none());
    assert_eq!(app.editor.active_document().unwrap().to_string(), "println");
    std::fs::remove_file(&path).ok();
}

#[test]
fn completion_filters_as_you_type_then_dismisses() {
    let path = temp_file("p");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.open_completion(vec![ci("print", 3), ci("procedure", 3), ci("foo", 3)]);
    assert_eq!(app.editor.completion.as_ref().unwrap().filtered.len(), 2); // print, procedure
    app.on_key(KeyEvent::from(KeyCode::Char('r'))); // "pr"
    assert_eq!(app.editor.active_document().unwrap().to_string(), "pr");
    assert_eq!(app.editor.completion.as_ref().unwrap().filtered.len(), 2);
    app.on_key(KeyEvent::from(KeyCode::Char('i'))); // "pri" → only print
    assert_eq!(app.editor.completion.as_ref().unwrap().filtered.len(), 1);
    // A non-identifier char leaves the word and dismisses the popup.
    app.on_key(KeyEvent::from(KeyCode::Char(' ')));
    assert!(app.editor.completion.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn hunk_navigation_cycles_over_change_starts() {
    let path = temp_file("a\nb\nc\nd\ne\nf\ng\n");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    let mut hunks = crate::git::LineStatuses::new();
    hunks.insert(1, crate::git::LineStatus::Modified);
    hunks.insert(2, crate::git::LineStatus::Modified); // same hunk as line 1
    hunks.insert(5, crate::git::LineStatus::Added); // separate hunk
    app.editor.git_hunks.insert(id, hunks);
    let line = |a: &App| {
        let d = a.editor.active_document().unwrap();
        d.char_to_line(d.selections.primary().head)
    };
    app.dispatch(Command::NextHunk);
    assert_eq!(line(&app), 1);
    app.dispatch(Command::NextHunk);
    assert_eq!(line(&app), 5);
    app.dispatch(Command::NextHunk); // wraps to the first hunk
    assert_eq!(line(&app), 1);
    app.dispatch(Command::PrevHunk); // wraps to the last hunk
    assert_eq!(line(&app), 5);
    std::fs::remove_file(&path).ok();
}

#[test]
fn git_status_message_stored_per_doc() {
    let path = temp_file("a\nb\n");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    let mut statuses = crate::git::LineStatuses::new();
    statuses.insert(1, crate::git::LineStatus::Modified);
    app.worker_tx
        .send(crate::worker::WorkerMsg::GitStatus {
            path: path.clone(),
            statuses,
        })
        .unwrap();
    app.drain_workers();
    assert_eq!(
        app.editor.git_hunks.get(&id).and_then(|m| m.get(&1)),
        Some(&crate::git::LineStatus::Modified)
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn references_open_picker_and_jump() {
    let path = temp_file("aaa\nbbb\nccc\n");
    let mut app = app_with(&path);
    let uri = crate::lsp::uri_for(&path);
    let loc = editor_lsp::Location {
        uri,
        line: 2,
        character: 0,
        end_line: 2,
        end_character: 1,
    };
    app.handle_lsp_event(crate::lsp::LspEvent::References(vec![loc]));
    assert!(matches!(
        app.editor.picker.as_ref().map(|p| p.kind),
        Some(crate::picker::PickerKind::Locations)
    ));
    assert_eq!(app.editor.nav_locations.len(), 1);
    // Accepting the row jumps the caret to line 3 (offset 8).
    app.picker_key(KeyEvent::from(KeyCode::Enter));
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.char_to_line(doc.selections.primary().head), 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn diagnostic_nav_and_caret_message() {
    let path = temp_file("aaa\nbbb\nccc\n");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    app.editor.diagnostics.insert(
        id,
        vec![diag(0, 0, 0, 1, "first"), diag(2, 0, 2, 1, "third")],
    );
    // Caret at 0 covers the first diagnostic; its message renders at the caret.
    assert_eq!(app.diagnostic_at_caret().map(|(_, m)| m), Some("first"));
    // Next jumps to the line-3 diagnostic (offset 8).
    app.dispatch(Command::NextDiagnostic);
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head,
        8
    );
    assert_eq!(app.diagnostic_at_caret().map(|(_, m)| m), Some("third"));
    // Next past the last wraps to the first; Prev from there wraps to the last.
    app.dispatch(Command::NextDiagnostic);
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head,
        0
    );
    app.dispatch(Command::PrevDiagnostic);
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head,
        8
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn completion_esc_dismisses_without_editing() {
    let path = temp_file("pr");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.open_completion(vec![ci("print", 3)]);
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(app.editor.completion.is_none());
    assert_eq!(app.editor.active_document().unwrap().to_string(), "pr");
    std::fs::remove_file(&path).ok();
}

#[test]
fn lsp_manager_is_inert_without_a_configured_server() {
    // With no server configured, every request resolves to `false`, notifications are
    // no-ops, and the event queue stays empty — the manager is dormant (plan §10).
    use std::collections::HashMap;
    let mut mgr = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), HashMap::new());
    assert!(!mgr.is_enabled());
    let p = std::path::Path::new("/tmp/x.rs");
    assert!(!mgr.request_hover(p, "rust", 0, 0));
    assert!(!mgr.request_definition(p, "rust", 0, 0));
    assert!(!mgr.request_completion(p, "rust", 0, 0));
    assert!(!mgr.request_references(p, "rust", 0, 0));
    assert!(!mgr.request_rename(p, "rust", 0, 0, "new"));
    assert!(!mgr.request_document_symbols(p, "rust"));
    mgr.did_open(p, "rust", "text"); // no server → no-op
    mgr.did_change(p, "rust", "text"); // no open doc → no-op
    assert!(mgr.poll().is_empty());
}
