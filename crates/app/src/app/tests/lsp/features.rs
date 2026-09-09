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

// ---- format on save ------------------------------------------------------

/// `format_on_save` must not cost anything when no server can format the file. The save path
/// blocks on the formatter, so a file with no language server (or a server that never declared
/// `documentFormattingProvider`) has to fall straight through to the write rather than stalling
/// for the whole timeout on every Ctrl+S.
#[test]
fn format_on_save_does_not_stall_without_a_server() {
    let path = temp_file("a  \nb\n");
    let mut app = app_with(&path);
    app.config.format_on_save = true;
    app.editor.active_document_mut().unwrap().dirty = true;

    let started = std::time::Instant::now();
    app.save_active();
    let took = started.elapsed();

    assert!(
        took < std::time::Duration::from_millis(200),
        "no formatter applies, so the save should not wait for one (took {took:?})"
    );
    assert!(
        !app.editor.active_document().unwrap().dirty,
        "it still saved"
    );
    std::fs::remove_file(&path).ok();
}

/// The formatter's edits reach the *file*, not just the buffer — i.e. formatting really does
/// happen before the write rather than racing it. Drives a real mock-server subprocess so the
/// bounded wait in `format_before_save` is exercised end to end.
#[cfg(unix)]
#[test]
fn format_on_save_writes_the_formatted_text() {
    let bin = mock_server_bin();
    if !bin.exists() {
        eprintln!("skipping: mock_lsp_server not found at {bin:?}");
        return;
    }
    let path = temp_rs_file("fn  main(){}\n");
    let transcript = r#"[
        {"expect":"initialize"},
        {"respond":{"capabilities":{"documentFormattingProvider":true}}},
        {"expect":"initialized"},
        {"expect":"textDocument/didOpen"},
        {"expect":"textDocument/formatting"},
        {"respond":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":12}},"newText":"fn main() {}"}]},
        {"exit":0}
    ]"#;
    let mut tpath = std::env::temp_dir();
    tpath.push(format!("lumina_fmtsave_{}.json", std::process::id()));
    std::fs::write(&tpath, transcript).unwrap();

    let mut app = app_with(&path);
    app.editor.lsp_enabled = true;
    app.config.format_on_save = true;
    let servers = std::collections::HashMap::from([(
        "rust".to_string(),
        vec![
            bin.to_string_lossy().into_owned(),
            tpath.to_string_lossy().into_owned(),
        ],
    )]);
    app.lsp = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), servers, "test".into());

    let mut ready = false;
    for _ in 0..400 {
        app.update_lsp();
        app.drain_workers();
        if app.lsp.is_ready("rust") {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ready, "the server handshake completes");
    // Let didOpen go out, as production does on the tick after the server is ready.
    app.update_lsp();
    app.drain_workers();

    // Dirty the buffer so `save_active` has something to write, then save.
    app.editor.active_document_mut().unwrap().set_caret(0);
    app.save_active();

    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk, "fn main() {}\n",
        "the formatter's edit should be in the file, not just the buffer"
    );
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "fn main() {}\n"
    );
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&tpath).ok();
}

/// With the setting off (the default) the formatter is never asked, so a buffer saves byte for
/// byte as the user left it — the "never silently rewrite" rule the other on-save options follow.
#[test]
fn format_on_save_off_leaves_the_buffer_alone() {
    let path = temp_file("keep   me\n");
    let mut app = app_with(&path);
    assert!(!app.config.format_on_save, "off by default");
    app.editor.active_document_mut().unwrap().dirty = true;
    app.save_active();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep   me\n");
    std::fs::remove_file(&path).ok();
}

// ---- didSave -------------------------------------------------------------

/// The client never sent `textDocument/didSave` and declared no
/// `textDocument.synchronization` at all, so a server's on-save pass never ran. For
/// rust-analyzer that pass *is* flycheck — the borrow-checker and type errors, as distinct from
/// the analyzer's own incremental diagnostics — so the more valuable half was never requested.
///
/// Drives a real mock-server subprocess: the transcript demands `textDocument/didSave` after the
/// open, so the test only passes if the notification actually goes out on the wire.
#[cfg(unix)]
#[test]
fn saving_notifies_the_server() {
    let bin = mock_server_bin();
    if !bin.exists() {
        eprintln!("skipping: mock_lsp_server not found at {bin:?}");
        return;
    }
    let path = temp_rs_file("fn main() {}\n");
    let transcript = r#"[
        {"expect":"initialize"},
        {"respond":{"capabilities":{}}},
        {"expect":"initialized"},
        {"expect":"textDocument/didOpen"},
        {"expect":"textDocument/didSave"},
        {"notify":{"method":"window/logMessage","params":{"type":3,"message":"saw didSave"}}},
        {"exit":0}
    ]"#;
    let mut tpath = std::env::temp_dir();
    tpath.push(format!("lumina_didsave_{}.json", std::process::id()));
    std::fs::write(&tpath, transcript).unwrap();

    let mut app = app_with(&path);
    app.editor.lsp_enabled = true;
    let servers = std::collections::HashMap::from([(
        "rust".to_string(),
        vec![
            bin.to_string_lossy().into_owned(),
            tpath.to_string_lossy().into_owned(),
        ],
    )]);
    app.lsp = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), servers, "test".into());

    let mut ready = false;
    for _ in 0..400 {
        app.update_lsp();
        app.drain_workers();
        if app.lsp.is_ready("rust") {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ready, "the server handshake completes");
    app.update_lsp(); // let didOpen go out
    app.drain_workers();

    app.editor.active_document_mut().unwrap().dirty = true;
    app.save_active();

    // The transcript only reaches its logMessage step if `didSave` arrived and matched; a
    // mismatch exits(1) and nothing is ever logged.
    let mut saw = false;
    for _ in 0..400 {
        app.update_lsp();
        app.drain_workers();
        if app
            .lsp
            .recent_logs(50)
            .iter()
            .any(|l: &String| l.contains("saw didSave"))
        {
            saw = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(saw, "the server received textDocument/didSave");

    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&tpath).ok();
}
