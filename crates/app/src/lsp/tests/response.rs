//! Response correlation: matching a reply to the request that produced it, the error matrix
//! (surfaced errors vs. silently-dropped `ContentModified`), and the staleness/supersede gating
//! that drops answers for text the buffer has since moved past.
use super::*;

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
