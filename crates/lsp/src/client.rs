//! A spawning LSP client: starts a server process, runs the initialize handshake, streams
//! document open/change notifications, and forwards `publishDiagnostics` onto a channel.
//!
//! This needs a real server binary, so it is exercised in integration/manual runs, never in
//! CI. The framing ([`crate::transport`]) and position math ([`crate::position`]) it relies
//! on are unit-tested independently.

use std::io::{self, BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::AtomicI64;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::thread;

use serde_json::Value;

use crate::transport;
use crate::{Incoming, ResponseError};

mod parse;
pub use parse::*;

mod handshake;
mod requests;

// The pure builders live in the child modules but are exercised by this module's unit tests
// (`super::*`); re-export them here for the test tree only.
#[cfg(test)]
pub(crate) use handshake::initialize_params;
#[cfg(test)]
pub(crate) use requests::{json_error, json_response};

#[cfg(test)]
mod tests;

/// A live connection to a language server. Dropping it kills the server.
pub struct LspHandle {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    child: Child,
}

/// Entry point for spawning servers.
pub struct LspClient;

impl LspClient {
    /// Spawn `command args…`, run the initialize handshake for `root_uri`, and return a
    /// handle plus a receiver of diagnostics updates.
    pub fn spawn(
        command: &str,
        args: &[String],
        root_uri: &str,
        client_version: &str,
    ) -> io::Result<(LspHandle, Receiver<Incoming>, i64)> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Capture stderr (rather than discarding it) so a server's own logs can be surfaced in
            // the LSP panel — the primary way to diagnose a server that misbehaves.
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("no stderr"))?;

        let (tx, rx) = channel();
        // stderr reader: forward each line as `Incoming::Log`. Ends on EOF (the server closed
        // stderr) or when the receiver is gone; either way the thread simply exits.
        {
            let tx = tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if tx.send(Incoming::Log(line)).is_err() {
                        break;
                    }
                }
            });
        }
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            // Terminal drop by design (§5): a framing error (including the `InvalidData` cases
            // `transport::read_message` builds), EOF (`Ok(None)`), or a JSON parse error all end
            // the loop and the thread. The reader owns the only end of the pipe, so there is
            // nowhere to log-and-continue *to* — a corrupt stream means the connection is over.
            // The app observes this as the diagnostics channel disconnecting (the matching `rx`
            // yields `Err`), which is the signal it acts on; re-surfacing each byte-level error
            // would be noise, so we swallow it deliberately rather than silently.
            while let Ok(Some(body)) = transport::read_message(&mut reader) {
                // Likewise: a body that fails to parse as JSON is unrecoverable framing garbage —
                // skip it and read on until the stream ends.
                if let Ok(value) = serde_json::from_str::<Value>(&body) {
                    if let Some(msg) = classify(value) {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let handle = LspHandle {
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            child,
        };
        // Send `initialize` only. `initialized` is deferred until the caller sees the response
        // (§3.2 ordering); the returned id lets it recognize that response.
        let init_id = handle.send_initialize(root_uri, client_version)?;
        Ok((handle, rx, init_id))
    }
}

impl Drop for LspHandle {
    fn drop(&mut self) {
        // Fire-and-forget graceful teardown then kill — non-blocking, so quitting never hangs on
        // a slow server. The ordered ladder that *waits* for a clean exit is `stop`, called
        // explicitly on restart/quit when we can afford the deadline.
        self.shutdown();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Classify an incoming server message into the shape the app acts on. A message with a
/// `method` is a request (has `id`, must be answered) or a notification (no `id`);
/// `publishDiagnostics` is special-cased. A message with `id` + `result`/`error` is a response.
fn classify(mut value: Value) -> Option<Incoming> {
    // Take ownership so the (potentially large) `params`/`result` payloads are **moved** out of the
    // parsed message with `Value::take` rather than deep-cloned — this runs on every server message.
    // `method` is extracted to an owned `String` up front so no borrow of `value` outlives the moves.
    let method = value
        .get("method")
        .and_then(|m| m.as_str())
        .map(str::to_string);
    if let Some(method) = method {
        if method == "textDocument/publishDiagnostics" {
            return parse_diagnostics(&value).map(Incoming::Diagnostics);
        }
        let params = value
            .get_mut("params")
            .map(Value::take)
            .unwrap_or(Value::Null);
        // `method` + `id` is a server→client request (answer it, §1.3); `method` alone is a
        // notification. The id stays raw — it may be a string and must be echoed verbatim.
        return Some(match value.get_mut("id").map(Value::take) {
            Some(id) => Incoming::ServerRequest { id, method, params },
            None => Incoming::Notification { method, params },
        });
    }
    // A response carries a numeric id and a result (or error).
    if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
        if value.get("result").is_some() || value.get("error").is_some() {
            // Preserve the server's error code + message so the app can apply the error matrix
            // (§9.5) instead of surfacing every failure — a `null` result and a real error look
            // identical otherwise, silently turning e.g. a failed rename into a no-op. (Read before
            // taking `result`, since both borrow `value`.)
            let error = value.get("error").map(|e| ResponseError {
                code: e.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                message: e
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| e.to_string()),
            });
            let result = value
                .get_mut("result")
                .map(Value::take)
                .unwrap_or(Value::Null);
            return Some(Incoming::Response { id, result, error });
        }
    }
    None
}
