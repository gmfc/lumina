use super::*;

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
fn goto_definition_via_menu_command_jumps_through_exec_id() {
    // Proves the exact path the right-click menu uses: exec_id("lsp.gotoDefinition") → lsp plugin
    // → host.lsp_request → manager → mock server → LspEvent::Goto → jump. If this works, the menu
    // works (its Enter/click just call exec_id).
    let bin = mock_server_bin();
    if !bin.exists() {
        eprintln!("skipping: mock_lsp_server not found at {bin:?}");
        return;
    }
    let path = temp_rs_file("fn foo() {\n    foo();\n}\n");
    let uri = format!("file://{}", path.display());
    let transcript = format!(
        r#"[
        {{"expect":"initialize"}},
        {{"respond":{{"capabilities":{{"definitionProvider":true}}}}}},
        {{"expect":"initialized"}},
        {{"expect":"textDocument/didOpen"}},
        {{"expect":"textDocument/definition"}},
        {{"respond":{{"uri":"{uri}","range":{{"start":{{"line":0,"character":3}},"end":{{"line":0,"character":6}}}}}}}},
        {{"exit":0}}
    ]"#
    );
    let mut tpath = std::env::temp_dir();
    tpath.push(format!("lumina_gotodef_{}.json", std::process::id()));
    std::fs::write(&tpath, &transcript).unwrap();

    let mut app = app_with(&path);
    app.editor.lsp_enabled = true; // production sets this true (discovery on)
    let servers = std::collections::HashMap::from([(
        "rust".to_string(),
        vec![
            bin.to_string_lossy().into_owned(),
            tpath.to_string_lossy().into_owned(),
        ],
    )]);
    app.lsp = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), servers, "test".into());

    // Drive the handshake until the server is Running.
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

    // Caret on the call-site `foo` (line 1), then invoke the menu's command.
    // Let the doc's didOpen go out before issuing a request (production does this on the tick
    // after the server becomes ready; the user triggers a command much later still).
    app.update_lsp();
    app.drain_workers();
    app.editor.active_document_mut().unwrap().set_caret(16);
    app.exec_id("lsp.gotoDefinition");

    // Let the definition response round-trip → jump to the definition (line 0, offset <= 9).
    let mut jumped = false;
    for _ in 0..400 {
        app.update_lsp();
        app.drain_workers();
        let head = app
            .editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head;
        if head <= 9 {
            jumped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        jumped,
        "gotoDefinition via the menu command jumped to the definition"
    );

    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&tpath).ok();
}
