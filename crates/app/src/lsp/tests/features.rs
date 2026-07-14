//! Feature round-trips: diagnostics overlap/context, pull diagnostics (full/unchanged), work-done
//! progress, semantic tokens, code lens, folding ranges, and the end-to-end mock-server drive.
use super::*;

#[test]
fn diag_overlaps_matches_point_and_range() {
    let d = |sl, sc, el, ec| {
        serde_json::json!({
            "range": {
                "start": {"line": sl, "character": sc},
                "end": {"line": el, "character": ec}
            }
        })
    };
    let diag = d(1, 4, 1, 9); // line 1, cols 4..9
                              // Point inside → overlaps; on either boundary → overlaps (touching counts).
    assert!(diag_overlaps(&diag, (1, 6), (1, 6)));
    assert!(diag_overlaps(&diag, (1, 4), (1, 4)));
    assert!(diag_overlaps(&diag, (1, 9), (1, 9)));
    // Point before/after the range on the same line → no.
    assert!(!diag_overlaps(&diag, (1, 3), (1, 3)));
    assert!(!diag_overlaps(&diag, (1, 10), (1, 10)));
    // A different line → no.
    assert!(!diag_overlaps(&diag, (2, 6), (2, 6)));
    // A wider selection that straddles the range → yes.
    assert!(diag_overlaps(&diag, (0, 0), (5, 0)));
    // A diagnostic with no range is never included.
    assert!(!diag_overlaps(&serde_json::json!({}), (0, 0), (9, 9)));
}

#[test]
fn code_action_context_echoes_overlapping_diagnostics() {
    // A publish snapshots raw diagnostics per URI; a codeAction at a point echoes only the
    // ones overlapping it (preserving `data`), and a re-publish replaces the snapshot.
    let mut mgr = manager();
    let uri = "file:///x.rs";
    let raw = vec![
        serde_json::json!({
            "range":{"start":{"line":1,"character":4},"end":{"line":1,"character":9}},
            "severity":1,"message":"here","data":{"fix":1}
        }),
        serde_json::json!({
            "range":{"start":{"line":3,"character":0},"end":{"line":3,"character":2}},
            "severity":2,"message":"elsewhere"
        }),
    ];
    feed(
        &mgr,
        "rust",
        Incoming::Diagnostics(DiagnosticsUpdate {
            uri: uri.into(),
            diagnostics: Vec::new(), // model diagnostics irrelevant to this path
            raw: raw.clone(),
        }),
    );
    let _ = mgr.poll(); // stores the raw snapshot
                        // A point on line 1 col 6 sees only the first diagnostic.
    let ctx = mgr.context_diagnostics(uri, (1, 6), (1, 6));
    assert_eq!(ctx.len(), 1);
    assert_eq!(ctx[0].get("data"), Some(&serde_json::json!({"fix":1})));
    // A point on line 3 sees only the second.
    assert_eq!(mgr.context_diagnostics(uri, (3, 1), (3, 1)).len(), 1);
    // A point on an empty line sees none.
    assert!(mgr.context_diagnostics(uri, (2, 0), (2, 0)).is_empty());
    // An empty publish clears the snapshot.
    feed(
        &mgr,
        "rust",
        Incoming::Diagnostics(DiagnosticsUpdate {
            uri: uri.into(),
            diagnostics: Vec::new(),
            raw: Vec::new(),
        }),
    );
    let _ = mgr.poll();
    assert!(mgr.context_diagnostics(uri, (1, 6), (1, 6)).is_empty());
}

#[test]
fn pull_full_report_publishes_and_caches_result_id() {
    // A full pull report reuses the push path: it emits a Diagnostics event, caches the
    // resultId for the next `previousResultId`, and snapshots the raw for codeAction context.
    let mut mgr = manager();
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::Diagnostic));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: serde_json::json!({
                "kind":"full","resultId":"r1",
                "items":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
                          "severity":1,"message":"boom","data":{"fix":1}}]
            }),
            error: None,
        },
    );
    let events = mgr.poll();
    assert!(
            events.iter().any(
                |e| matches!(e, LspEvent::Diagnostics(u) if u.uri == "file:///x.rs" && u.diagnostics.len() == 1)
            ),
            "full pull report did not publish diagnostics"
        );
    assert_eq!(
        mgr.diag_result_id.get("file:///x.rs").map(String::as_str),
        Some("r1")
    );
    // The raw snapshot feeds codeAction context at the diagnostic's position.
    assert_eq!(
        mgr.context_diagnostics("file:///x.rs", (0, 0), (0, 0))
            .len(),
        1
    );
}

#[test]
fn pull_unchanged_report_keeps_diagnostics_and_updates_result_id() {
    // An `unchanged` report emits no event and preserves the existing raw snapshot; only the
    // cached resultId advances.
    let mut mgr = manager();
    mgr.diag_raw.insert(
            "file:///x.rs".into(),
            vec![serde_json::json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}})],
        );
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::Diagnostic));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: serde_json::json!({ "kind":"unchanged", "resultId":"r2" }),
            error: None,
        },
    );
    let events = mgr.poll();
    assert!(
        !events.iter().any(|e| matches!(e, LspEvent::Diagnostics(_))),
        "unchanged report should emit no diagnostics event"
    );
    assert_eq!(
        mgr.diag_result_id.get("file:///x.rs").map(String::as_str),
        Some("r2")
    );
    assert!(
        mgr.diag_raw.contains_key("file:///x.rs"),
        "unchanged report must keep the existing raw snapshot"
    );
}

#[test]
fn progress_begin_report_end_updates_the_statusline_line() {
    let mut mgr = manager();
    // begin → a Progress line naming the server + title.
    feed(
        &mgr,
        "rust",
        Incoming::Notification {
            method: "$/progress".into(),
            params: serde_json::json!({ "token":"idx",
                    "value": { "kind":"begin", "title":"Indexing", "percentage": 0 } }),
        },
    );
    assert!(
        matches!(mgr.poll().as_slice(), [LspEvent::Progress(Some(s))] if s.contains("rust") && s.contains("Indexing"))
    );
    // report → message + percentage fold in.
    feed(
        &mgr,
        "rust",
        Incoming::Notification {
            method: "$/progress".into(),
            params: serde_json::json!({ "token":"idx",
                    "value": { "kind":"report", "message":"crate 3/7", "percentage": 42 } }),
        },
    );
    assert!(
        matches!(mgr.poll().as_slice(), [LspEvent::Progress(Some(s))] if s.contains("42%") && s.contains("crate 3/7"))
    );
    // end → the token clears; with nothing else active the line goes empty.
    feed(
        &mgr,
        "rust",
        Incoming::Notification {
            method: "$/progress".into(),
            params: serde_json::json!({ "token":"idx", "value": { "kind":"end" } }),
        },
    );
    assert!(matches!(mgr.poll().as_slice(), [LspEvent::Progress(None)]));
    assert!(mgr.progress.is_empty());
}

#[test]
fn progress_without_kind_is_ignored() {
    // A `$/progress` on a partial-result token carries a list chunk (no `kind`) — not work-done
    // progress. We send no partial-result tokens yet, so it's dropped.
    let mut mgr = manager();
    feed(
        &mgr,
        "rust",
        Incoming::Notification {
            method: "$/progress".into(),
            params: serde_json::json!({ "token":"pr", "value": [ {"x":1} ] }),
        },
    );
    assert!(mgr.poll().is_empty());
    assert!(mgr.progress.is_empty());
}

#[test]
fn server_exit_clears_progress() {
    let mut mgr = manager();
    mgr.progress.push(ProgressItem {
        lang: "rust".into(),
        token: "idx".into(),
        title: "Indexing".into(),
        message: None,
        percentage: None,
    });
    feed_exit(&mgr, "rust");
    let events = mgr.poll();
    assert!(mgr.progress.is_empty(), "progress not cleared on exit");
    assert!(events.iter().any(|e| matches!(e, LspEvent::Progress(None))));
}

#[test]
fn semantic_tokens_response_decodes_with_the_connection_legend() {
    // A `.../full` response is decoded in poll against the Running connection's legend into a
    // SemanticTokens event for the requested uri.
    let mut mgr = manager();
    mgr.state.insert(
        "rust".into(),
        ClientState::Running(ServerCaps {
            semantic_tokens: true,
            semantic_legend: editor_lsp::SemanticLegend {
                token_types: vec!["function".into()],
                token_modifiers: Vec::new(),
            },
            ..Default::default()
        }),
    );
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::SemanticTokens));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: serde_json::json!({ "data": [0, 0, 4, 0, 0] }),
            error: None,
        },
    );
    let events = mgr.poll();
    assert!(
        matches!(events.as_slice(), [LspEvent::SemanticTokens { uri, tokens }]
                if uri == "file:///x.rs" && tokens.len() == 1 && tokens[0].token_type == "function"),
        "semantic tokens response did not decode into a SemanticTokens event"
    );
}

#[test]
fn semantic_tokens_refresh_surfaces_an_event() {
    let mut mgr = manager();
    feed(
        &mgr,
        "rust",
        Incoming::ServerRequest {
            id: serde_json::json!(3),
            method: "workspace/semanticTokens/refresh".into(),
            params: serde_json::Value::Null,
        },
    );
    assert!(mgr
        .poll()
        .iter()
        .any(|e| matches!(e, LspEvent::SemanticTokensRefresh { lang } if lang == "rust")));
}

#[test]
fn code_lens_response_emits_resolved_then_accumulates() {
    // The initial response emits its already-resolved lenses; a later resolve response appends
    // the newly-titled lens and re-emits the growing set.
    let mut mgr = manager();
    mgr.state.insert(
        "rust".into(),
        ClientState::Running(ServerCaps {
            code_lens: true,
            code_lens_resolve: true,
            ..Default::default()
        }),
    );
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::CodeLens));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: serde_json::json!([
                {"range":{"start":{"line":2,"character":0},"end":{"line":2,"character":1}},
                 "command":{"title":"Run"}},
                {"range":{"start":{"line":5,"character":0},"end":{"line":5,"character":1}},
                 "data":{"id":9}}
            ]),
            error: None,
        },
    );
    assert!(
        matches!(mgr.poll().as_slice(), [LspEvent::CodeLenses { uri, lenses }]
                if uri == "file:///x.rs" && lenses.len() == 1 && lenses[0].title.as_deref() == Some("Run"))
    );
    // Simulate the resolve response landing (no client in tests, so pend it by hand).
    mgr.pending
        .insert(("rust".into(), 2), pend(Pending::CodeLensResolve));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 2,
            result: serde_json::json!(
                {"range":{"start":{"line":5,"character":0},"end":{"line":5,"character":1}},
                 "command":{"title":"Debug"}}
            ),
            error: None,
        },
    );
    assert!(
        matches!(mgr.poll().as_slice(), [LspEvent::CodeLenses { lenses, .. }]
                if lenses.len() == 2 && lenses[1].title.as_deref() == Some("Debug"))
    );
}

#[test]
fn code_lens_refresh_surfaces_an_event() {
    let mut mgr = manager();
    feed(
        &mgr,
        "rust",
        Incoming::ServerRequest {
            id: serde_json::json!(4),
            method: "workspace/codeLens/refresh".into(),
            params: serde_json::Value::Null,
        },
    );
    assert!(mgr
        .poll()
        .iter()
        .any(|e| matches!(e, LspEvent::CodeLensRefresh { lang } if lang == "rust")));
}

#[test]
fn folding_range_response_becomes_a_folding_event() {
    let mut mgr = manager();
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::FoldingRange));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: serde_json::json!([{ "startLine": 2, "endLine": 8, "kind": "region" }]),
            error: None,
        },
    );
    assert!(
        matches!(mgr.poll().as_slice(), [LspEvent::FoldingRanges { uri, ranges }]
                if uri == "file:///x.rs" && ranges.len() == 1 && ranges[0].start_line == 2)
    );
}

#[test]
fn mock_server_drives_passive_feature_requests_end_to_end() {
    // Spawn the real transport against the scripted mock through the manager, then run the
    // full passive-feature flow (handshake → didOpen → semantic tokens / inlay hints / code
    // lens / folding / pull diagnostics / hover) and assert each result surfaces as an event.
    let bin = mock_server_bin();
    if !bin.exists() {
        // Built only by `cargo test --workspace` (as CI + coverage run); skip a narrow run.
        eprintln!("skipping: mock_lsp_server not found at {bin:?}");
        return;
    }
    let transcript = r#"[
            {"expect": "initialize"},
            {"respond": {"capabilities": {
                "hoverProvider": true,
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
            {"respond": [{"position": {"line": 0, "character": 0}, "label": ": i32"}]},
            {"expect": "textDocument/codeLens"},
            {"respond": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "command": {"title": "Run"}}]},
            {"expect": "textDocument/foldingRange"},
            {"respond": [{"startLine": 0, "endLine": 2}]},
            {"expect": "textDocument/diagnostic"},
            {"respond": {"kind": "full", "items": []}},
            {"expect": "textDocument/hover"},
            {"respond": {"contents": "docs"}},
            {"exit": 0}
        ]"#;
    let mut tpath = std::env::temp_dir();
    tpath.push(format!("lumina_mgr_transcript_{}.json", std::process::id()));
    std::fs::write(&tpath, transcript).unwrap();

    let servers = HashMap::from([(
        "rust".to_string(),
        vec![
            bin.to_string_lossy().into_owned(),
            tpath.to_string_lossy().into_owned(),
        ],
    )]);
    let mut mgr = LspManager::new(Path::new("/tmp"), servers, "test".into());
    mgr.ensure_started("rust");

    // Wait for the async handshake to complete.
    let mut ready = false;
    for _ in 0..300 {
        let _ = mgr.poll();
        if mgr.is_ready("rust") {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready, "handshake did not complete");

    let doc = Path::new("/tmp/mock_doc.rs");
    assert!(
        mgr.did_open(doc, "rust", "fn x() {}"),
        "didOpen should send"
    );
    assert!(mgr.request_semantic_tokens(doc, "rust"));
    assert!(mgr.request_inlay_hints(doc, "rust", 3));
    assert!(mgr.request_code_lens(doc, "rust"));
    assert!(mgr.request_folding_ranges(doc, "rust"));
    assert!(mgr.request_pull_diagnostics(doc, "rust"));
    assert!(mgr.request_hover(doc, "rust", 0, 0));

    let mut events = Vec::new();
    for _ in 0..300 {
        events.extend(mgr.poll());
        if events.len() >= 6 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LspEvent::SemanticTokens { .. })),
        "no semantic tokens event"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LspEvent::InlayHints { .. })),
        "no inlay hints event"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LspEvent::CodeLenses { .. })),
        "no code lens event"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LspEvent::FoldingRanges { .. })),
        "no folding event"
    );
    assert!(
        events.iter().any(|e| matches!(e, LspEvent::Hover(_))),
        "no hover event"
    );

    mgr.stop_all(Duration::from_secs(2));
    std::fs::remove_file(&tpath).ok();
}
