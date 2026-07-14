use super::*;

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
