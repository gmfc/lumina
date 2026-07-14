//! Server-initiated messages: the `workspace/configuration` and `workspaceFolders` replies, the
//! routing split between requests the app must answer (applyEdit/showMessage…) and manager-local
//! ones, `client/(un)registerCapability` for watched files, and the various `.../refresh` pokes.
use super::*;

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
