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
use crate::Incoming;

mod parse;
pub use parse::*;

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
    ) -> io::Result<(LspHandle, Receiver<Incoming>)> {
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
                    if let Some(msg) = classify(&value) {
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

    /// Send a request, returning the JSON-RPC id so the caller can correlate the response.
    fn request(&self, method: &str, params: Value) -> io::Result<i64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        Ok(id)
    }

    fn position(uri: &str, line: u32, character: u32) -> Value {
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        })
    }

    /// Request hover info at a position. `character` is a UTF-16 column.
    pub fn hover(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        self.request("textDocument/hover", Self::position(uri, line, character))
    }

    /// Request the definition location(s) at a position.
    pub fn definition(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        self.request(
            "textDocument/definition",
            Self::position(uri, line, character),
        )
    }

    /// Request the implementation location(s) of the symbol at a position.
    pub fn implementation(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        self.request(
            "textDocument/implementation",
            Self::position(uri, line, character),
        )
    }

    /// Request the type-definition location(s) of the symbol at a position.
    pub fn type_definition(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        self.request(
            "textDocument/typeDefinition",
            Self::position(uri, line, character),
        )
    }

    /// Request completions at a position.
    pub fn completion(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        self.request(
            "textDocument/completion",
            Self::position(uri, line, character),
        )
    }

    /// Request a rename of the symbol at a position to `new_name`.
    pub fn rename(&self, uri: &str, line: u32, character: u32, new_name: &str) -> io::Result<i64> {
        let mut params = Self::position(uri, line, character);
        params["newName"] = json!(new_name);
        self.request("textDocument/rename", params)
    }

    /// Request all references to the symbol at a position (declaration included).
    pub fn references(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        let mut params = Self::position(uri, line, character);
        params["context"] = json!({ "includeDeclaration": true });
        self.request("textDocument/references", params)
    }

    /// Request the symbols declared in a document.
    pub fn document_symbols(&self, uri: &str) -> io::Result<i64> {
        self.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )
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
                        "publishDiagnostics": { "relatedInformation": false },
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "definition": { "linkSupport": true },
                        "completion": {
                            "completionItem": { "snippetSupport": false }
                        },
                        "rename": { "prepareSupport": false }
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

/// Classify an incoming server message: a diagnostics notification or a response to one of our
/// requests. Other notifications/requests are ignored.
fn classify(value: &Value) -> Option<Incoming> {
    if value.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
        return parse_diagnostics(value).map(Incoming::Diagnostics);
    }
    // A response carries a numeric id and a result (or error).
    if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
        if value.get("result").is_some() || value.get("error").is_some() {
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            // Preserve the server's error message so the app can report the failure rather than
            // treating it as an empty result (a `null` result and a real error look identical
            // otherwise, silently turning e.g. a failed rename into a no-op).
            let error = value.get("error").map(|e| {
                e.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| e.to_string())
            });
            return Some(Incoming::Response { id, result, error });
        }
    }
    None
}
