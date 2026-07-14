//! Unit tests for the LSP manager: request/response plumbing, staleness gating,
//! diagnostics, and the crash/restart policy.
//!
//! This is the test module *root*. It holds the shared fixtures (`manager`, `feed`,
//! `feed_exit`, `pend`, `mock_server_bin`) plus a couple of cross-cutting tests, and declares
//! the per-concern submodules that mirror the production `lsp/` layout (lifecycle, response,
//! server-initiated messages, feature round-trips). Each submodule reaches these fixtures — and
//! the manager internals re-exported by the parent `lsp` module — through its own `use super::*`.
use super::*;

mod features;
mod lifecycle;
mod response;
mod server_msgs;

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
