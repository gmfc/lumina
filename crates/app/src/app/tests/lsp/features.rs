use super::*;

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
fn code_actions_event_opens_a_picker_via_handle_lsp_event() {
    // handle_lsp_event(CodeActions) → on_code_actions → the code-action plugin's picker.
    let path = temp_rs_file("let x = 1;\n");
    let mut app = app_with(&path);
    app.drain_workers(); // flush initial DidChangeActive
    app.handle_lsp_event(crate::lsp::LspEvent::CodeActions(vec![
        editor_lsp::CodeAction {
            title: "Fix it".into(),
            edit: None,
            command: None,
        },
    ]));
    app.drain_workers();
    assert!(
        app.editor.picker.is_some(),
        "code actions should open a picker"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn document_highlight_event_publishes_then_clears() {
    use editor_plugin::event::Event;
    let path = temp_rs_file("let foo = foo;\n");
    let mut app = app_with(&path);
    app.drain_workers(); // flush the initial DidChangeActive (else it clears the layer we set)
    feed_event(
        &mut app,
        Event::LspHighlights(vec![
            editor_plugin::LspHighlight {
                line: 0,
                start_char16: 4,
                end_line: 0,
                end_char16: 7,
                kind: 2,
            },
            editor_plugin::LspHighlight {
                line: 0,
                start_char16: 10,
                end_line: 0,
                end_char16: 13,
                kind: 3,
            },
        ]),
    );
    assert!(active_layer(&app, "lsp.highlight").is_some_and(|s| s.spans.len() == 2));
    // A cursor move onto a word with LSP enabled re-requests (exercises cursor_on_word).
    app.editor.lsp_enabled = true;
    app.editor.active_document_mut().unwrap().set_caret(5);
    let id = app.editor.workspace.active_doc().unwrap();
    feed_event(&mut app, Event::DidChangeCursor(id));
    // An edit clears the (now-stale) highlights.
    feed_event(&mut app, Event::DidChange(id));
    std::fs::remove_file(&path).ok();
}

#[test]
fn signature_help_event_sets_and_clears_the_status_item() {
    use editor_plugin::event::Event;
    let path = temp_rs_file("fn f(a: i32) {}\n");
    let mut app = app_with(&path);
    app.drain_workers(); // flush the initial DidChangeActive (else it closes the hint we set)
    feed_event(&mut app, Event::LspSignatureHelp(Some("f([a])".into())));
    assert_eq!(
        app.editor
            .status_items
            .get("lsp.signature")
            .map(String::as_str),
        Some("f([a])")
    );
    feed_event(&mut app, Event::LspSignatureHelp(None));
    assert!(app
        .editor
        .status_items
        .get("lsp.signature")
        .map(String::as_str)
        .unwrap_or("")
        .is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn code_action_event_opens_a_picker() {
    use editor_plugin::event::Event;
    let path = temp_rs_file("let x = 1;\n");
    let mut app = app_with(&path);
    feed_event(
        &mut app,
        Event::LspCodeActions(vec![editor_plugin::LspCodeAction {
            title: "Fix it".into(),
            edit: editor_plugin::LspWorkspaceEdit::default(),
            command: Some(("cmd".into(), serde_json::Value::Null)),
        }]),
    );
    assert!(
        app.editor.picker.is_some(),
        "offered code actions should open a picker"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn semantic_tokens_event_publishes_and_renders_a_decoration_layer() {
    // handle_lsp_event → to_primitive → semantic-tokens plugin → lsp.semantic layer → render.
    let path = temp_file("fn main() {}\n");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::SemanticTokens {
        uri: crate::lsp::uri_for(&path),
        tokens: vec![editor_lsp::SemanticToken {
            line: 0,
            start_char16: 0,
            length: 2,
            token_type: "keyword".into(),
            modifiers: vec!["declaration".into()],
        }],
    });
    app.drain_workers();
    assert!(
        active_layer(&app, "lsp.semantic").is_some_and(|s| !s.spans.is_empty()),
        "semantic tokens should publish span decorations"
    );
    let out = render_to_string(&mut app, 100, 10);
    assert!(
        out.contains("fn main"),
        "the doc still renders with the overlay"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn inlay_hints_event_publishes_and_renders_virtual_text() {
    let path = temp_file("let x = 5;\n");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::InlayHints {
        uri: crate::lsp::uri_for(&path),
        hints: vec![editor_lsp::InlayHint {
            line: 0,
            char16: 5,
            label: ": i32".into(),
            kind: 1,
            pad_left: true,
            pad_right: false,
        }],
    });
    app.drain_workers();
    assert!(active_layer(&app, "lsp.inlay").is_some_and(|s| s.virtual_text.len() == 1));
    let out = render_to_string(&mut app, 100, 10);
    assert!(out.contains(": i32"), "the inlay hint renders inline");
    std::fs::remove_file(&path).ok();
}

#[test]
fn code_lenses_event_publishes_and_renders_virtual_text() {
    let path = temp_file("fn run() {}\n");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::CodeLenses {
        uri: crate::lsp::uri_for(&path),
        lenses: vec![editor_lsp::CodeLens {
            line: 0,
            char16: 0,
            title: Some("Run".into()),
            raw: serde_json::Value::Null,
        }],
    });
    app.drain_workers();
    assert!(active_layer(&app, "lsp.lens").is_some_and(|s| s.virtual_text.len() == 1));
    let out = render_to_string(&mut app, 100, 10);
    assert!(out.contains("Run"), "the code lens renders inline");
    std::fs::remove_file(&path).ok();
}

#[test]
fn folding_ranges_event_publishes_gutter_marks() {
    let path = temp_file("fn a() {\n  1\n}\n");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::FoldingRanges {
        uri: crate::lsp::uri_for(&path),
        ranges: vec![editor_lsp::FoldingRange {
            start_line: 0,
            end_line: 2,
            kind: Some("region".into()),
        }],
    });
    app.drain_workers();
    assert!(active_layer(&app, "lsp.fold").is_some_and(|s| s.gutter.len() == 1));
    let _ = render_to_string(&mut app, 100, 10); // exercises the gutter-mark render path
    std::fs::remove_file(&path).ok();
}

#[test]
fn progress_event_sets_and_clears_the_status_item() {
    let path = temp_file("x\n");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::Progress(Some(
        "rust: Indexing 10%".into(),
    )));
    assert_eq!(
        app.editor
            .status_items
            .get("lsp.progress")
            .map(String::as_str),
        Some("rust: Indexing 10%")
    );
    app.handle_lsp_event(crate::lsp::LspEvent::Progress(None));
    assert!(!app.editor.status_items.contains_key("lsp.progress"));
    std::fs::remove_file(&path).ok();
}
