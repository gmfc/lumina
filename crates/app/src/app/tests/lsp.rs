use super::*;

mod completion;
mod features;
mod lifecycle;
mod nav;

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

/// Broadcast a plugin event the way the LSP poll loop does, then drain it.
fn feed_event(app: &mut App, ev: editor_plugin::event::Event) {
    app.editor.pending_events.push(ev);
    app.drain_workers();
}

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
