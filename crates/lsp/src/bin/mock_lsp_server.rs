//! A tiny scripted LSP server for the transcript harness (§12 L2) — **test fixture, not a real
//! server**. It speaks LSP framing over stdio and plays a JSON transcript passed as `argv[1]`,
//! letting the client's transport + lifecycle be exercised deterministically in CI without any
//! language server installed.
//!
//! The transcript is a JSON array of steps, played top to bottom:
//!
//! - `{"expect": "<method>"}` — block for the next client message; fail (exit 1) unless its
//!   `method` matches. If the message carries an `id`, remember it for the next `respond`.
//! - `{"respond": <result>}` — reply to the last expected request, echoing its `id`.
//! - `{"respond_error": {"code": n, "message": s}}` — reply with a JSON-RPC error instead.
//! - `{"notify": {"method": s, "params": v}}` — send a server→client notification.
//! - `{"request": {"id": v, "method": s, "params": v}}` — send a server→client request.
//! - `{"exit": n}` — flush and exit with code `n` (crash-by-exit at step N).
//!
//! Reaching the end of the transcript exits 0 (clean stdout close → the client sees EOF).

use std::io::{self, BufReader, Write};

use editor_lsp::transport::{encode, read_message};
use serde_json::Value;

fn main() {
    let path = std::env::args().nth(1).expect("transcript path arg");
    let src = std::fs::read_to_string(&path).expect("read transcript");
    let steps: Vec<Value> = serde_json::from_str(&src).expect("parse transcript");

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let mut last_id = Value::Null;

    for step in &steps {
        if let Some(method) = step.get("expect").and_then(|m| m.as_str()) {
            // Block for the next client message; a clean EOF here means the client hung up early.
            let Ok(Some(body)) = read_message(&mut reader) else {
                std::process::exit(0);
            };
            let msg: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if msg.get("method").and_then(|m| m.as_str()) != Some(method) {
                // Mismatch → fail the transcript (the client will see EOF and its recv will time
                // out, failing the test). stderr is discarded by the client, so exit code is the
                // only signal.
                std::process::exit(1);
            }
            if let Some(id) = msg.get("id") {
                last_id = id.clone();
            }
        } else if let Some(result) = step.get("respond") {
            send(
                &mut out,
                serde_json::json!({ "jsonrpc": "2.0", "id": last_id, "result": result }),
            );
        } else if let Some(err) = step.get("respond_error") {
            send(
                &mut out,
                serde_json::json!({ "jsonrpc": "2.0", "id": last_id, "error": err }),
            );
        } else if let Some(n) = step.get("notify") {
            send(
                &mut out,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": n.get("method").cloned().unwrap_or(Value::Null),
                    "params": n.get("params").cloned().unwrap_or(Value::Null),
                }),
            );
        } else if let Some(r) = step.get("request") {
            send(
                &mut out,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": r.get("id").cloned().unwrap_or(Value::Null),
                    "method": r.get("method").cloned().unwrap_or(Value::Null),
                    "params": r.get("params").cloned().unwrap_or(Value::Null),
                }),
            );
        } else if let Some(code) = step.get("exit").and_then(|c| c.as_i64()) {
            let _ = out.flush();
            std::process::exit(code as i32);
        }
    }
    let _ = out.flush();
}

fn send(out: &mut impl Write, msg: Value) {
    let _ = out.write_all(&encode(&msg.to_string()));
    let _ = out.flush();
}
