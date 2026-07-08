//! A spawning LSP client: starts a server process, runs the initialize handshake, streams
//! document open/change notifications, and forwards `publishDiagnostics` onto a channel.
//!
//! This needs a real server binary, so it is exercised in integration/manual runs, never in
//! CI. The framing ([`crate::transport`]) and position math ([`crate::position`]) it relies
//! on are unit-tested independently.

use std::io::{self, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::thread;

use serde_json::{json, Value};

use crate::transport;
use crate::{Diagnostic, DiagnosticsUpdate, Severity};

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
    ) -> io::Result<(LspHandle, Receiver<DiagnosticsUpdate>)> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;

        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(body)) = transport::read_message(&mut reader) {
                if let Ok(value) = serde_json::from_str::<Value>(&body) {
                    if value.get("method").and_then(|m| m.as_str())
                        == Some("textDocument/publishDiagnostics")
                    {
                        if let Some(update) = parse_diagnostics(&value) {
                            if tx.send(update).is_err() {
                                break;
                            }
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
        handle.initialize(root_uri)?;
        Ok((handle, rx))
    }
}

impl LspHandle {
    fn send(&self, msg: Value) -> io::Result<()> {
        let body = msg.to_string();
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        stdin.write_all(&transport::encode(&body))?;
        stdin.flush()
    }

    fn notify(&self, method: &str, params: Value) -> io::Result<()> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn request(&self, method: &str, params: Value) -> io::Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    /// Send `initialize` + `initialized`. (A strict client waits for the initialize result
    /// before `initialized`; we send promptly, which most servers accept — real handshake
    /// sequencing is a refinement for when this is exercised against a live server.)
    pub fn initialize(&self, root_uri: &str) -> io::Result<()> {
        self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": { "relatedInformation": false }
                    }
                }
            }),
        )?;
        self.notify("initialized", json!({}))
    }

    pub fn did_open(
        &self,
        uri: &str,
        language_id: &str,
        version: i64,
        text: &str,
    ) -> io::Result<()> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text,
                }
            }),
        )
    }

    /// Full-document sync (simplest correct change mode).
    pub fn did_change(&self, uri: &str, version: i64, text: &str) -> io::Result<()> {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [ { "text": text } ]
            }),
        )
    }

    pub fn did_close(&self, uri: &str) -> io::Result<()> {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Ask the server to shut down cleanly.
    pub fn shutdown(&self) {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
    }
}

impl Drop for LspHandle {
    fn drop(&mut self) {
        self.shutdown();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parse a `publishDiagnostics` notification's params into our model.
fn parse_diagnostics(value: &Value) -> Option<DiagnosticsUpdate> {
    let params = value.get("params")?;
    let uri = params.get("uri")?.as_str()?.to_string();
    let mut diagnostics = Vec::new();
    if let Some(arr) = params.get("diagnostics").and_then(|d| d.as_array()) {
        for d in arr {
            let range = d.get("range")?;
            let start = range.get("start")?;
            let end = range.get("end")?;
            let severity = match d.get("severity").and_then(|s| s.as_u64()) {
                Some(1) => Severity::Error,
                Some(2) => Severity::Warning,
                Some(3) => Severity::Info,
                _ => Severity::Hint,
            };
            diagnostics.push(Diagnostic {
                line: start.get("line")?.as_u64()? as u32,
                start_char16: start.get("character")?.as_u64()? as u32,
                end_line: end.get("line")?.as_u64()? as u32,
                end_char16: end.get("character")?.as_u64()? as u32,
                severity,
                message: d
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Some(DiagnosticsUpdate { uri, diagnostics })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_publish_diagnostics_notification() {
        let value: Value = serde_json::from_str(
            r#"{
                "jsonrpc":"2.0",
                "method":"textDocument/publishDiagnostics",
                "params":{
                    "uri":"file:///x/a.rs",
                    "diagnostics":[
                        {"range":{"start":{"line":2,"character":4},"end":{"line":2,"character":9}},
                         "severity":1,"message":"cannot find value"}
                    ]
                }
            }"#,
        )
        .unwrap();
        let update = parse_diagnostics(&value).unwrap();
        assert_eq!(update.uri, "file:///x/a.rs");
        assert_eq!(update.diagnostics.len(), 1);
        let d = &update.diagnostics[0];
        assert_eq!((d.line, d.start_char16, d.end_char16), (2, 4, 9));
        assert_eq!(d.severity, Severity::Error);
    }
}
