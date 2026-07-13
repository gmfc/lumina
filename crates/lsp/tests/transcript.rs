//! Mock-server transcript harness (§12 L2): spawn the real transport against a scripted stdio
//! server (`mock_lsp_server` bin) and assert the client's lifecycle end to end — no language
//! server installed, fully deterministic, CI-friendly. This exercises the actual subprocess +
//! framing + `classify` path the in-process manager unit tests can't reach.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use editor_lsp::{Incoming, LspClient, LspHandle};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Write `transcript` to a temp file and spawn the mock server playing it.
fn spawn(transcript: &str) -> (LspHandle, Receiver<Incoming>, i64, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "lumina_transcript_{}_{}.json",
        std::process::id(),
        n
    ));
    std::fs::write(&path, transcript).unwrap();
    let bin = env!("CARGO_BIN_EXE_mock_lsp_server");
    let (handle, rx, init_id) = LspClient::spawn(
        bin,
        &[path.to_string_lossy().into_owned()],
        "file:///root",
        "test",
    )
    .expect("spawn mock server");
    (handle, rx, init_id, path)
}

/// The next inbound message, or fail if none arrives promptly (the mock died / mismatched).
fn recv(rx: &Receiver<Incoming>) -> Incoming {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("no inbound message within timeout (mock server exited or a step mismatched)")
}

#[test]
fn drives_init_hover_notification_and_server_request() {
    let transcript = r#"[
        {"expect": "initialize"},
        {"respond": {"capabilities": {"hoverProvider": true}}},
        {"expect": "initialized"},
        {"expect": "textDocument/hover"},
        {"respond": {"contents": "docs here"}},
        {"notify": {"method": "window/showMessage", "params": {"type": 3, "message": "hi"}}},
        {"request": {"id": 100, "method": "workspace/applyEdit", "params": {"edit": {}}}},
        {"exit": 0}
    ]"#;
    let (mut handle, rx, init_id, path) = spawn(transcript);

    // The initialize response is correlated by the id the client assigned.
    match recv(&rx) {
        Incoming::Response { id, result, error } => {
            assert_eq!(id, init_id);
            assert!(error.is_none());
            assert_eq!(result["capabilities"]["hoverProvider"], true);
        }
        _ => panic!("expected the initialize response first"),
    }

    // Finish the handshake, then a hover round-trips through the real framing.
    handle.send_initialized().unwrap();
    let hover_id = handle.hover("file:///x.rs", 0, 0).unwrap();
    match recv(&rx) {
        Incoming::Response { id, result, .. } => {
            assert_eq!(
                id, hover_id,
                "hover response must correlate to its request id"
            );
            assert_eq!(result["contents"], "docs here");
        }
        _ => panic!("expected the hover response"),
    }

    // A server→client notification and request mid-stream are classified distinctly.
    match recv(&rx) {
        Incoming::Notification { method, .. } => assert_eq!(method, "window/showMessage"),
        _ => panic!("expected a server notification"),
    }
    match recv(&rx) {
        Incoming::ServerRequest { method, id, .. } => {
            assert_eq!(method, "workspace/applyEdit");
            assert_eq!(
                id,
                serde_json::json!(100),
                "server request id echoed verbatim"
            );
        }
        _ => panic!("expected a server→client request"),
    }

    handle.stop(Duration::from_secs(2));
    std::fs::remove_file(&path).ok();
}

#[test]
fn initialize_error_is_surfaced_as_an_error_response() {
    let transcript = r#"[
        {"expect": "initialize"},
        {"respond_error": {"code": -32603, "message": "boom"}},
        {"exit": 0}
    ]"#;
    let (mut handle, rx, init_id, path) = spawn(transcript);
    match recv(&rx) {
        Incoming::Response { id, error, .. } => {
            assert_eq!(id, init_id);
            let e = error.expect("an error object, not a null result");
            assert_eq!(e.code, -32603);
            assert_eq!(e.message, "boom");
        }
        _ => panic!("expected the initialize error response"),
    }
    handle.stop(Duration::from_secs(2));
    std::fs::remove_file(&path).ok();
}

#[test]
fn crash_by_exit_disconnects_the_stream() {
    // A server that exits (crash-by-exit at step N) closes its stdout; the forwarding thread
    // reaches EOF and drops the channel, so the client observes a disconnect rather than hanging.
    let transcript = r#"[
        {"expect": "initialize"},
        {"respond": {"capabilities": {}}},
        {"exit": 0}
    ]"#;
    let (mut handle, rx, _init_id, path) = spawn(transcript);
    let _ = recv(&rx); // drain the initialize response

    let mut disconnected = false;
    for _ in 0..50 {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Err(RecvTimeoutError::Disconnected) => {
                disconnected = true;
                break;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Ok(_) => {}
        }
    }
    assert!(
        disconnected,
        "the inbound channel should disconnect after the server exits"
    );
    handle.stop(Duration::from_secs(1));
    std::fs::remove_file(&path).ok();
}
