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
    app.handle_lsp_event(crate::lsp::LspEvent::Rename(edit));
    app.drain_workers(); // broadcast LspWorkspaceEdit → rename plugin → apply on drain
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "let bar = bar + 1;"
    );
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
    // Driven through the registry: git navigation is the `git-nav` builtin plugin.
    app.exec_id("git.nextHunk");
    assert_eq!(line(&app), 1);
    app.exec_id("git.nextHunk");
    assert_eq!(line(&app), 5);
    app.exec_id("git.nextHunk"); // wraps to the first hunk
    assert_eq!(line(&app), 1);
    app.exec_id("git.prevHunk"); // wraps to the last hunk
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
    // Drives the `lsp-nav` plugin: a References response becomes an `LspLocations` event; the
    // plugin opens its own picker and, on Enter, jumps via `Host::open_location` (resolved in the
    // drain). The picker is plugin-owned (owner "lsp-nav"), not an app-side `PickerKind::Locations`.
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
    app.drain_workers(); // broadcast LspLocations → lsp-nav opens the picker
    assert_eq!(
        app.editor.picker.as_ref().and_then(|p| p.owner.as_deref()),
        Some("lsp-nav"),
        "references open the lsp-nav plugin's picker"
    );
    // Accepting the row jumps the caret to line 3 (offset 8).
    app.picker_key(KeyEvent::from(KeyCode::Enter));
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.char_to_line(doc.selections.primary().head), 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn goto_definition_jumps_to_the_target() {
    // A Goto response becomes an `LspGoto` event; the `lsp-nav` plugin jumps via
    // `Host::open_location`, resolved (open + caret) in the same drain.
    let path = temp_file("aaa\nbbb\nccc\n");
    let mut app = app_with(&path);
    let uri = crate::lsp::uri_for(&path);
    let loc = editor_lsp::Location {
        uri,
        line: 1,
        character: 2,
        end_line: 1,
        end_character: 3,
    };
    app.handle_lsp_event(crate::lsp::LspEvent::Goto(loc));
    app.drain_workers(); // broadcast LspGoto → open_location → drain opens + jumps
    let doc = app.editor.active_document().unwrap();
    // Line 1 ("bbb") starts at offset 4; character 2 → offset 6.
    assert_eq!(doc.selections.primary().head, 6);
    std::fs::remove_file(&path).ok();
}

#[test]
fn hover_shows_info_overlay() {
    // A Hover response becomes an `LspHover` event; the `hover` plugin shows the text in a
    // dismissable info box (the app-owned `Overlay::Info`).
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::Hover("fn main()".into()));
    app.drain_workers(); // broadcast LspHover → hover plugin → show_info
    assert!(
        matches!(app.editor.overlay.as_ref(), Some(crate::editor::Overlay::Info(t)) if t == "fn main()"),
        "hover response should open an info overlay with the text"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn lsp_error_event_is_shown_on_status_bar() {
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::Error("rename failed".into()));
    assert_eq!(
        app.editor.status_message.as_deref(),
        Some("LSP: rename failed"),
        "a server error should surface, not be swallowed"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn diagnostic_nav_and_caret_message() {
    // Drives the `diagnostics` plugin: diagnostics arrive via the LspDiagnostics event, nav goes
    // through exec_id, and the caret message is read off the plugin-published status item.
    let path = temp_file("aaa\nbbb\nccc\n");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    let head = |app: &App| {
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head
    };
    let caret_msg = |app: &App| app.editor.status_items.get("lsp.diag").cloned();

    feed_diagnostics(
        &mut app,
        id,
        vec![diag(0, 0, 0, 1, "first"), diag(2, 0, 2, 1, "third")],
    );
    // Caret at 0 covers the first diagnostic; the plugin publishes its glyph + message.
    assert_eq!(caret_msg(&app).as_deref(), Some("E first"));
    // Next jumps to the line-3 diagnostic (offset 8) and updates the caret message.
    app.exec_id("lsp.nextDiagnostic");
    assert_eq!(head(&app), 8);
    assert_eq!(caret_msg(&app).as_deref(), Some("E third"));
    // Next past the last wraps to the first; Prev from there wraps to the last.
    app.exec_id("lsp.nextDiagnostic");
    assert_eq!(head(&app), 0);
    app.exec_id("lsp.prevDiagnostic");
    assert_eq!(head(&app), 8);
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
fn lsp_manager_is_inert_without_a_configured_server() {
    // With no server configured, every request resolves to `false`, notifications are
    // no-ops, and the event queue stays empty — the manager is dormant (plan §10).
    use std::collections::HashMap;
    let mut mgr =
        crate::lsp::LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
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
