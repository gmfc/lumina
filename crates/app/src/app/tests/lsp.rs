use super::*;

#[test]
fn workspace_edit_applies_rename_across_occurrences() {
    let path = temp_file("let foo = foo + 1;");
    let mut app = app_with(&path);
    let uri = crate::lsp::uri_for(&path);
    let edit = editor_lsp::WorkspaceEdit {
        changes: vec![editor_lsp::DocEdit {
            uri,
            version: None,
            edits: vec![
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
        }],
    };
    app.handle_lsp_event(crate::lsp::LspEvent::Rename(edit));
    app.drain_workers(); // broadcast LspWorkspaceEdit → rename plugin → apply on drain
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "let bar = bar + 1;"
    );
}

#[test]
fn server_apply_edit_request_mutates_the_buffer() {
    // A server→client workspace/applyEdit is applied through the same Transaction pipeline as
    // rename (invariant #1), so a server-initiated edit lands in the buffer.
    let path = temp_file("let foo = foo + 1;");
    let mut app = app_with(&path);
    let uri = crate::lsp::uri_for(&path);
    let mut changes = serde_json::Map::new();
    changes.insert(
        uri,
        serde_json::json!([
            {"range":{"start":{"line":0,"character":4},"end":{"line":0,"character":7}},"newText":"bar"},
            {"range":{"start":{"line":0,"character":10},"end":{"line":0,"character":13}},"newText":"bar"}
        ]),
    );
    let params = serde_json::json!({ "label": "fix", "edit": { "changes": changes } });
    app.handle_lsp_event(crate::lsp::LspEvent::ServerRequest {
        lang: "rust".into(),
        id: serde_json::json!(1),
        method: "workspace/applyEdit".into(),
        params,
    });
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "let bar = bar + 1;"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn formatting_response_applies_edits_to_the_requested_doc() {
    // A textDocument/formatting response's TextEdit[] is applied to the document named by the
    // response uri (not whatever is active) through the Transaction pipeline (invariant #1).
    let path = temp_file("let  x=1 ;");
    let mut app = app_with(&path);
    let edits = vec![editor_lsp::TextEdit {
        start_line: 0,
        start_char16: 0,
        end_line: 0,
        end_char16: 10, // the whole line
        new_text: "let x = 1;".into(),
    }];
    app.handle_lsp_event(crate::lsp::LspEvent::Formatting {
        uri: crate::lsp::uri_for(&path),
        edits,
    });
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "let x = 1;"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn formatting_targets_requested_doc_not_active() {
    // Regression: formatting requested for doc A must land on A even if the user switched to B
    // before the async response arrived — it must never corrupt the now-active doc.
    let path_a = temp_file("let  a=1 ;");
    let path_b = temp_file("let  b=2 ;");
    let mut app = app_with(&path_a);
    app.open_path(&path_b); // B is now the active document
    let edits = vec![editor_lsp::TextEdit {
        start_line: 0,
        start_char16: 0,
        end_line: 0,
        end_char16: 10,
        new_text: "let a = 1;".into(),
    }];
    app.handle_lsp_event(crate::lsp::LspEvent::Formatting {
        uri: crate::lsp::uri_for(&path_a),
        edits,
    });
    let a_id = app.editor.workspace.find_by_path(&path_a).unwrap();
    assert_eq!(
        app.editor
            .workspace
            .documents
            .get(a_id)
            .unwrap()
            .to_string(),
        "let a = 1;",
        "A should be formatted"
    );
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "let  b=2 ;",
        "the active doc B must be untouched"
    );
    std::fs::remove_file(&path_a).ok();
    std::fs::remove_file(&path_b).ok();
}

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
    // The passive whole-doc feature requests are inert too (gated on a Running connection).
    assert!(!mgr.request_semantic_tokens(p, "rust"));
    assert!(!mgr.request_inlay_hints(p, "rust", 10));
    assert!(!mgr.request_code_lens(p, "rust"));
    assert!(!mgr.request_folding_ranges(p, "rust"));
    assert!(!mgr.request_pull_diagnostics(p, "rust"));
    // …and their capability gates report unsupported without a handshake.
    assert!(!mgr.supports_semantic_tokens("rust"));
    assert!(!mgr.supports_inlay_hints("rust"));
    assert!(!mgr.supports_code_lens("rust"));
    assert!(!mgr.supports_folding("rust"));
    assert!(!mgr.supports_pull("rust"));
    // Forwarding a disk change with no registered watcher is a no-op (must not panic).
    mgr.notify_watched_file_change(p);
    mgr.did_open(p, "rust", "text"); // no server → no-op
    mgr.did_change(p, "rust", "text"); // no open doc → no-op
    assert!(mgr.poll().is_empty());
}

/// The decoration layer id published for the active doc, if any.
fn active_layer<'a>(app: &'a App, layer: &str) -> Option<&'a editor_plugin::DecorationSet> {
    let id = app.editor.workspace.active_doc()?;
    app.editor.decorations.get(&id)?.get(layer)
}

/// Path to the `mock_lsp_server` workspace bin, relative to the current test executable.
fn mock_server_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // drop the test binary (…/deps/<name>)
    if p.ends_with("deps") {
        p.pop(); // …/deps → …/debug
    }
    p.push(format!("mock_lsp_server{}", std::env::consts::EXE_SUFFIX));
    p
}

#[test]
fn update_lsp_syncs_and_requests_passive_features_end_to_end() {
    // Drive the App's `update_lsp` tick against the scripted mock through the manager: it starts
    // the server, handshakes, sends didOpen, and requests every passive feature, then the debounced
    // pull. Covers sync_document / request_passive_features / poll_pull_diagnostics end to end.
    let bin = mock_server_bin();
    if !bin.exists() {
        eprintln!("skipping: mock_lsp_server not found at {bin:?}");
        return;
    }
    let transcript = r#"[
        {"expect": "initialize"},
        {"respond": {"capabilities": {
            "semanticTokensProvider": {"legend": {"tokenTypes": ["keyword"], "tokenModifiers": []}, "full": true},
            "inlayHintProvider": true,
            "codeLensProvider": {"resolveProvider": false},
            "foldingRangeProvider": true,
            "diagnosticProvider": {"interFileDependencies": false, "workspaceDiagnostics": false}
        }}},
        {"expect": "initialized"},
        {"expect": "textDocument/didOpen"},
        {"expect": "textDocument/semanticTokens/full"},
        {"respond": {"data": [0, 0, 2, 0, 0]}},
        {"expect": "textDocument/inlayHint"},
        {"respond": []},
        {"expect": "textDocument/codeLens"},
        {"respond": []},
        {"expect": "textDocument/foldingRange"},
        {"respond": []},
        {"expect": "textDocument/diagnostic"},
        {"respond": {"kind": "full", "items": []}},
        {"exit": 0}
    ]"#;
    let mut tpath = std::env::temp_dir();
    tpath.push(format!("lumina_update_lsp_{}.json", std::process::id()));
    std::fs::write(&tpath, transcript).unwrap();

    let path = temp_rs_file("fn x() {}\n");
    let mut app = app_with(&path);
    // Point the app's (otherwise inert) manager at the mock server.
    let servers = std::collections::HashMap::from([(
        "rust".to_string(),
        vec![
            bin.to_string_lossy().into_owned(),
            tpath.to_string_lossy().into_owned(),
        ],
    )]);
    app.lsp = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), servers, "test".into());

    // Tick the loop until the semantic-tokens response round-trips into a published layer.
    let mut got_tokens = false;
    for _ in 0..400 {
        app.update_lsp();
        app.drain_workers();
        if active_layer(&app, "lsp.semantic").is_some() {
            got_tokens = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        got_tokens,
        "update_lsp should sync the doc + request semantic tokens end to end"
    );

    // After a quiet period the debounced diagnostics pull fires on the next tick.
    std::thread::sleep(std::time::Duration::from_millis(320));
    app.update_lsp();
    app.drain_workers();

    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&tpath).ok();
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

/// A temp `.rs` file (so language detection gives `rust` and `lsp_position` resolves).
fn temp_rs_file(contents: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "lumina_lsp_{}_{}.rs",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&p, contents).unwrap();
    p
}

#[test]
fn dispatch_lsp_request_covers_every_request_kind() {
    // With a rust doc but no server, every request kind dispatches through its arm (the requests
    // no-op at the capability gate) and the two client-command shims queue follow-up requests.
    use editor_plugin::LspRequestKind as K;
    let path = temp_rs_file("fn main() { let x = 1; }\n");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(3); // on a word
    for kind in [
        K::Hover,
        K::Definition,
        K::Implementation,
        K::TypeDefinition,
        K::Completion,
        K::References,
        K::DocumentSymbols,
        K::Rename("y".into()),
        K::Formatting,
        K::SignatureHelp,
        K::DocumentHighlight,
        K::WorkspaceSymbols("q".into()),
        K::CodeAction,
        K::ResolveCompletion {
            label: "l".into(),
            data: serde_json::Value::Null,
        },
        K::ExecuteCommand {
            command: "editor.action.triggerSuggest".into(),
            arguments: serde_json::Value::Null,
        },
        K::ExecuteCommand {
            command: "editor.action.triggerParameterHints".into(),
            arguments: serde_json::Value::Null,
        },
        K::ExecuteCommand {
            command: "some.server.command".into(),
            arguments: serde_json::json!([]),
        },
    ] {
        app.dispatch_lsp_request(kind); // must not panic
    }
    assert!(
        !app.editor.pending_lsp_requests.is_empty(),
        "the triggerSuggest/triggerParameterHints shims should queue follow-up requests"
    );
    std::fs::remove_file(&path).ok();
}

/// Broadcast a plugin event the way the LSP poll loop does, then drain it.
fn feed_event(app: &mut App, ev: editor_plugin::event::Event) {
    app.editor.pending_events.push(ev);
    app.drain_workers();
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
fn handle_server_request_covers_window_methods_and_unknown() {
    let path = temp_rs_file("let foo = 1;\n");
    let mut app = app_with(&path);
    // showMessageRequest surfaces on the statusline and is answered.
    app.handle_server_request(
        "rust".into(),
        serde_json::json!(2),
        "window/showMessageRequest",
        serde_json::json!({ "type": 1, "message": "reload?", "actions": [{"title": "OK"}] }),
    );
    assert_eq!(app.editor.status_message.as_deref(), Some("LSP: reload?"));
    // showDocument for a non-file / missing target claims no success but still answers.
    app.handle_server_request(
        "rust".into(),
        serde_json::json!(3),
        "window/showDocument",
        serde_json::json!({ "uri": "file:///does/not/exist", "external": false }),
    );
    // Any other routed method is still answered (never hangs the server).
    app.handle_server_request(
        "rust".into(),
        serde_json::json!(4),
        "some/unknown/request",
        serde_json::Value::Null,
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

#[test]
fn refresh_events_reissue_requests_without_panicking() {
    // The refresh arms collect the language's open docs and re-request; with no server the
    // requests are inert, but the collection + per-doc dispatch loops run (a `.rs` doc so the
    // `rust` language filter matches).
    let path = temp_rs_file("fn x() {}\n");
    let mut app = app_with(&path);
    app.handle_lsp_event(crate::lsp::LspEvent::SemanticTokensRefresh {
        lang: "rust".into(),
    });
    app.handle_lsp_event(crate::lsp::LspEvent::InlayHintRefresh {
        lang: "rust".into(),
    });
    app.handle_lsp_event(crate::lsp::LspEvent::CodeLensRefresh {
        lang: "rust".into(),
    });
    app.handle_lsp_event(crate::lsp::LspEvent::DiagnosticsRefresh {
        lang: "rust".into(),
    });
    std::fs::remove_file(&path).ok();
}
