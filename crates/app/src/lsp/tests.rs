//! Unit tests for the LSP manager: request/response plumbing, staleness gating,
//! diagnostics, and the crash/restart policy.
use super::*;
use editor_lsp::{Cap, Incoming, ResponseError, ServerCaps};

fn manager() -> LspManager {
    LspManager::new(Path::new("/tmp"), HashMap::new(), "test".into())
}

/// A pending entry for a request kind (staleness fields default to a fresh doc at version 0).
fn pend(kind: Pending) -> PendingEntry {
    PendingEntry {
        kind,
        uri: "file:///x.rs".into(),
        version: 0,
    }
}

/// Push an inbound message onto the merged channel as the forwarding thread would.
fn feed(mgr: &LspManager, lang: &str, msg: Incoming) {
    mgr.tx.send((lang.into(), ClientMsg::Msg(msg))).unwrap();
}

/// Signal that a connection's process exited.
fn feed_exit(mgr: &LspManager, lang: &str) {
    mgr.tx.send((lang.into(), ClientMsg::Exited)).unwrap();
}

#[test]
fn init_response_stores_caps_and_becomes_ready() {
    // The awaited InitializeResult transitions the connection to Running with parsed caps,
    // and produces no user-facing event (the handshake is internal).
    let mut mgr = manager();
    mgr.state
        .insert("rust".into(), ClientState::Initializing { init_id: 1 });
    let caps = serde_json::json!({ "capabilities": { "hoverProvider": true } });
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: caps,
            error: None,
        },
    );
    assert!(mgr.poll().is_empty());
    assert!(mgr.is_ready("rust"));
    assert!(mgr.request_allowed("rust", Cap::Hover));
    assert!(!mgr.request_allowed("rust", Cap::Completion));
}

#[test]
fn request_allowed_requires_running_and_capability() {
    let mut mgr = manager();
    assert!(!mgr.request_allowed("rust", Cap::Hover)); // no state
    mgr.state.insert(
        "rust".into(),
        ClientState::Running(ServerCaps {
            hover: true,
            ..Default::default()
        }),
    );
    assert!(mgr.request_allowed("rust", Cap::Hover));
    assert!(!mgr.request_allowed("rust", Cap::Rename)); // unsupported
    mgr.state
        .insert("rust".into(), ClientState::Initializing { init_id: 1 });
    assert!(!mgr.request_allowed("rust", Cap::Hover)); // still initializing
}

#[test]
fn colliding_ids_route_per_language() {
    // Two servers both use id 1; the (language, id) key keeps their responses distinct.
    let mut mgr = manager();
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::Rename));
    mgr.pending.insert(("py".into(), 1), pend(Pending::Hover));
    feed(
        &mgr,
        "py",
        Incoming::Response {
            id: 1,
            result: serde_json::json!({ "contents": "doc" }),
            error: None,
        },
    );
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: Default::default(),
            error: None,
        },
    );
    let events = mgr.poll();
    assert!(events.iter().any(|e| matches!(e, LspEvent::Hover(_))));
    assert!(events.iter().any(|e| matches!(e, LspEvent::Rename(_))));
}

#[test]
fn configuration_response_matches_item_arity() {
    // Wrong arity wedges servers, so a null-per-item response must match the request.
    let params = serde_json::json!({ "items": [{ "section": "a" }, { "section": "b" }] });
    assert_eq!(
        configuration_response(&params),
        serde_json::json!([null, null])
    );
    assert_eq!(
        configuration_response(&serde_json::json!({})),
        serde_json::json!([])
    );
}

#[test]
fn workspace_folders_response_names_the_root() {
    let r = workspace_folders_response("file:///home/g/proj/");
    assert_eq!(r[0]["uri"], "file:///home/g/proj/");
    assert_eq!(r[0]["name"], "proj");
}

#[test]
fn app_needing_requests_route_to_the_app() {
    // applyEdit/showMessageRequest/showDocument are surfaced for the app to act + answer.
    let mut mgr = manager();
    for method in [
        "workspace/applyEdit",
        "window/showMessageRequest",
        "window/showDocument",
    ] {
        feed(
            &mgr,
            "rust",
            Incoming::ServerRequest {
                id: serde_json::json!(1),
                method: method.into(),
                params: serde_json::json!({}),
            },
        );
    }
    let routed: Vec<String> = mgr
        .poll()
        .into_iter()
        .filter_map(|e| match e {
            LspEvent::ServerRequest { method, .. } => Some(method),
            _ => None,
        })
        .collect();
    assert_eq!(
        routed,
        [
            "workspace/applyEdit",
            "window/showMessageRequest",
            "window/showDocument"
        ]
    );
}

#[test]
fn local_requests_and_unknown_do_not_route_to_the_app() {
    // Manager-local requests (configuration, refresh, …) and unknown methods are answered in
    // poll (no handle in tests → no-op) and produce no app event.
    let mut mgr = manager();
    for method in [
        "workspace/configuration",
        "window/workDoneProgress/create",
        "some/unknown/method",
    ] {
        feed(
            &mgr,
            "rust",
            Incoming::ServerRequest {
                id: serde_json::json!(1),
                method: method.into(),
                params: serde_json::json!({ "items": [] }),
            },
        );
    }
    assert!(mgr.poll().is_empty());
}

#[test]
fn show_message_notification_becomes_a_message_event() {
    let mut mgr = manager();
    feed(
        &mgr,
        "rust",
        Incoming::Notification {
            method: "window/showMessage".into(),
            params: serde_json::json!({ "type": 3, "message": "reloading" }),
        },
    );
    let events = mgr.poll();
    assert!(matches!(events.as_slice(), [LspEvent::Message(m)] if m == "reloading"));
}

#[test]
fn error_response_surfaces_as_error_event() {
    // A server error reply to a tracked request becomes an `Error` event, not a parsed
    // (empty) result that would read as "nothing found".
    let mut mgr = manager();
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::Rename));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: Default::default(), // Null; irrelevant on the error path
            error: Some(ResponseError {
                code: -32603,
                message: "rename failed".into(),
            }),
        },
    );
    let events = mgr.poll();
    assert!(
        matches!(events.as_slice(), [LspEvent::Error(m)] if m == "rename failed"),
        "error response did not surface as LspEvent::Error"
    );
}

#[test]
fn success_response_still_parses_result() {
    let mut mgr = manager();
    mgr.pending
        .insert(("rust".into(), 2), pend(Pending::Rename));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 2,
            result: Default::default(), // a null result still parses to an (empty) Rename
            error: None,
        },
    );
    let events = mgr.poll();
    assert!(matches!(events.as_slice(), [LspEvent::Rename(_)]));
}

#[test]
fn format_signature_brackets_the_active_param() {
    let sig = editor_lsp::SignatureHelp {
        label: "f(a, b)".into(),
        active_param: Some((2, 3)),
    };
    assert_eq!(format_signature(&sig), "f([a], b)");
    let none = editor_lsp::SignatureHelp {
        label: "f()".into(),
        active_param: None,
    };
    assert_eq!(format_signature(&none), "f()");
}

#[test]
fn stale_response_by_version_is_dropped() {
    // The buffer moved (v3 → v5) after we asked; the answer is for the old text → drop it.
    let mut mgr = manager();
    mgr.versions.insert("file:///x.rs".into(), 5);
    mgr.pending.insert(
        ("rust".into(), 1),
        PendingEntry {
            kind: Pending::Hover,
            uri: "file:///x.rs".into(),
            version: 3,
        },
    );
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: serde_json::json!({ "contents": "doc" }),
            error: None,
        },
    );
    assert!(mgr.poll().is_empty());
}

#[test]
fn superseded_cancelable_response_is_dropped() {
    // Two hovers in flight; id 2 supersedes id 1. The late id-1 answer is dropped; id 2 wins.
    let mut mgr = manager();
    mgr.pending.insert(("rust".into(), 1), pend(Pending::Hover));
    mgr.pending.insert(("rust".into(), 2), pend(Pending::Hover));
    mgr.inflight.insert(("rust".into(), Pending::Hover), 2);
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: serde_json::json!({ "contents": "old" }),
            error: None,
        },
    );
    assert!(mgr.poll().is_empty(), "superseded response was not dropped");
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 2,
            result: serde_json::json!({ "contents": "new" }),
            error: None,
        },
    );
    assert!(matches!(mgr.poll().as_slice(), [LspEvent::Hover(m)] if m == "new"));
}

#[test]
fn content_modified_error_is_dropped_silently() {
    // -32801 ContentModified (and -32800/-32802) are not user errors — no Error event.
    let mut mgr = manager();
    mgr.pending
        .insert(("rust".into(), 1), pend(Pending::Rename));
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: Default::default(),
            error: Some(ResponseError {
                code: ResponseError::CONTENT_MODIFIED,
                message: "stale".into(),
            }),
        },
    );
    assert!(mgr.poll().is_empty());
}

#[test]
fn restart_backoff_is_exponential_and_capped() {
    assert_eq!(restart_backoff(1), Duration::from_millis(250));
    assert_eq!(restart_backoff(2), Duration::from_millis(500));
    assert_eq!(restart_backoff(3), Duration::from_millis(1000));
    assert_eq!(restart_backoff(4), Duration::from_millis(2000));
    assert_eq!(restart_backoff(5), Duration::from_millis(4000));
    assert_eq!(restart_backoff(9), Duration::from_millis(4000)); // capped
}

#[test]
fn breaker_trips_only_within_the_window() {
    let now = Instant::now();
    // Five crashes inside the window → tripped.
    let recent: Vec<Instant> = (0..5).map(|i| now - Duration::from_secs(i * 10)).collect();
    assert!(breaker_tripped(&recent, now));
    // Four recent + one ancient (outside the window) → not tripped.
    let mut spread = recent[..4].to_vec();
    spread.push(now - Duration::from_secs(600));
    assert!(!breaker_tripped(&spread, now));
}

#[test]
fn server_exit_fails_pending_and_clears_diagnostics() {
    let mut mgr = manager();
    mgr.state
        .insert("rust".into(), ClientState::Running(ServerCaps::default()));
    mgr.pending.insert(("rust".into(), 5), pend(Pending::Hover));
    mgr.published
        .entry("rust".into())
        .or_default()
        .insert("file:///a.rs".into());
    feed_exit(&mgr, "rust");
    let events = mgr.poll();

    assert!(!mgr.state.contains_key("rust"), "state not torn down");
    assert!(mgr.pending.is_empty(), "pending not failed locally");
    assert!(
            events
                .iter()
                .any(|e| matches!(e, LspEvent::Diagnostics(u) if u.uri == "file:///a.rs" && u.diagnostics.is_empty())),
            "stale diagnostics not cleared"
        );
    assert!(events
        .iter()
        .any(|e| matches!(e, LspEvent::ServerExited { lang } if lang == "rust")));
    assert!(events.iter().any(|e| matches!(e, LspEvent::Error(_))));
    // First crash → scheduled for restart, breaker not tripped.
    assert!(mgr.restart_after.contains_key("rust"));
    assert!(!mgr.failed.contains_key("rust"));
}

#[test]
fn did_close_only_for_opened_docs_and_prunes_bookkeeping() {
    let mut mgr = manager();
    let uri = uri_for(Path::new("/x.rs"));
    // Never opened this session → no stray close, nothing to prune (no panic).
    mgr.did_close(Path::new("/x.rs"), "rust");
    assert!(mgr.open_docs.get("rust").is_none_or(|s| s.is_empty()));

    // Simulate an opened doc with diagnostics (as did_open / a publish would record).
    mgr.open_docs
        .entry("rust".into())
        .or_default()
        .insert(uri.clone());
    mgr.published
        .entry("rust".into())
        .or_default()
        .insert(uri.clone());
    mgr.did_close(Path::new("/x.rs"), "rust");
    // Closing removes it from the attach set and the published set (no unbounded growth).
    assert!(mgr.open_docs.get("rust").is_none_or(|s| !s.contains(&uri)));
    assert!(mgr.published.get("rust").is_none_or(|s| !s.contains(&uri)));
}

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
fn diagnostic_refresh_drops_result_ids_and_emits_refresh() {
    // `workspace/diagnostic/refresh` is answered, drops cached resultIds (forcing a full
    // re-pull), and surfaces a DiagnosticsRefresh so the app re-arms its debounced pull.
    let mut mgr = manager();
    mgr.diag_result_id
        .insert("file:///x.rs".into(), "r1".into());
    feed(
        &mgr,
        "rust",
        Incoming::ServerRequest {
            id: serde_json::json!(7),
            method: "workspace/diagnostic/refresh".into(),
            params: serde_json::json!({}),
        },
    );
    let events = mgr.poll();
    assert!(mgr.diag_result_id.is_empty(), "resultIds were not dropped");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LspEvent::DiagnosticsRefresh { lang } if lang == "rust")),
        "refresh did not surface a DiagnosticsRefresh event"
    );
}

#[test]
fn register_capability_request_stores_watchers_and_answers_locally() {
    // A `client/registerCapability` for didChangeWatchedFiles is answered locally (no app
    // event) and compiles + stores the watchers by registration id (§3.6/§8.1).
    let mut mgr = manager();
    feed(
        &mgr,
        "rust",
        Incoming::ServerRequest {
            id: serde_json::json!(1),
            method: "client/registerCapability".into(),
            params: serde_json::json!({ "registrations": [{
                "id": "w1",
                "method": "workspace/didChangeWatchedFiles",
                "registerOptions": { "watchers": [{ "globPattern": "**/*.rs" }] }
            }]}),
        },
    );
    assert!(
        mgr.poll().is_empty(),
        "registration must not surface an app event"
    );
    assert!(
        mgr.file_watchers
            .get("rust")
            .is_some_and(|m| m.contains_key("w1")),
        "watchers were not stored"
    );
}

#[test]
fn unregister_capability_removes_by_misspelled_key() {
    let mut mgr = manager();
    mgr.register_capability(
        "rust",
        &serde_json::json!({ "registrations": [{
            "id": "w1",
            "method": "workspace/didChangeWatchedFiles",
            "registerOptions": { "watchers": [{ "globPattern": "**/*.rs" }] }
        }]}),
    );
    assert!(mgr
        .file_watchers
        .get("rust")
        .is_some_and(|m| m.contains_key("w1")));
    // The spec misspells the params key as `unregisterations`.
    mgr.unregister_capability(
        "rust",
        &serde_json::json!({ "unregisterations": [{
            "id": "w1", "method": "workspace/didChangeWatchedFiles"
        }]}),
    );
    assert!(mgr
        .file_watchers
        .get("rust")
        .is_none_or(|m| !m.contains_key("w1")));
}

#[test]
fn server_exit_clears_dynamic_registrations() {
    // Dynamic registrations are connection-scoped: a crash drops them (the restart re-registers).
    let mut mgr = manager();
    mgr.register_capability(
        "rust",
        &serde_json::json!({ "registrations": [{
            "id": "w1",
            "method": "workspace/didChangeWatchedFiles",
            "registerOptions": { "watchers": [{ "globPattern": "**/*.rs" }] }
        }]}),
    );
    assert!(mgr.file_watchers.contains_key("rust"));
    feed_exit(&mgr, "rust");
    let _ = mgr.poll();
    assert!(
        !mgr.file_watchers.contains_key("rust"),
        "registrations not cleared on exit"
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

/// Path to the `mock_lsp_server` workspace bin (built alongside the test binary), resolved
/// relative to the current test executable so it works under any target dir.
fn mock_server_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // drop the test binary (…/deps/<name>)
    if p.ends_with("deps") {
        p.pop(); // …/deps → …/debug
    }
    p.push(format!("mock_lsp_server{}", std::env::consts::EXE_SUFFIX));
    p
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

#[test]
fn breaker_trips_after_repeated_crashes() {
    let mut mgr = manager();
    for _ in 0..CRASH_LIMIT {
        feed_exit(&mgr, "rust");
    }
    let events = mgr.poll();
    assert!(
        mgr.failed.contains_key("rust"),
        "breaker did not trip after {CRASH_LIMIT} crashes"
    );
    assert!(
        !mgr.restart_after.contains_key("rust"),
        "should not be scheduled to restart"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, LspEvent::Error(m) if m.contains("not restarting"))));
}
