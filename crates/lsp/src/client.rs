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
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::transport;
use crate::{Incoming, ResponseError};

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
        client_version: &str,
    ) -> io::Result<(LspHandle, Receiver<Incoming>, i64)> {
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
        // Send `initialize` only. `initialized` is deferred until the caller sees the response
        // (§3.2 ordering); the returned id lets it recognize that response.
        let init_id = handle.send_initialize(root_uri, client_version)?;
        Ok((handle, rx, init_id))
    }
}

/// Build the `initialize` request params. Pure (no I/O) so it is unit-tested. Declares only
/// capabilities the client actually implements (honest declaration): utf-16 only, no snippet
/// engine, no prepareRename, plaintext hover. `rootPath`/`workspaceFolders` are derived from
/// `root_uri`.
pub fn initialize_params(root_uri: &str, client_version: &str) -> Value {
    let root_path = root_uri.strip_prefix("file://").unwrap_or(root_uri);
    let name = root_path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("root");
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "lumina", "version": client_version },
        "rootUri": root_uri,
        "rootPath": root_path,
        "workspaceFolders": [ { "uri": root_uri, "name": name } ],
        "trace": "off",
        "capabilities": {
            "general": { "positionEncodings": ["utf-16"] },
            "textDocument": {
                "publishDiagnostics": { "relatedInformation": false },
                "hover": { "contentFormat": ["plaintext"] },
                "signatureHelp": { "signatureInformation": { "parameterInformation": { "labelOffsetSupport": true }, "activeParameterSupport": true } },
                "definition": { "linkSupport": true },
                "typeDefinition": { "linkSupport": true },
                "implementation": { "linkSupport": true },
                "references": {},
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "completion": {
                    "contextSupport": true,
                    "completionItem": {
                        "snippetSupport": true,
                        "resolveSupport": { "properties": ["documentation", "detail", "additionalTextEdits"] }
                    }
                },
                "rename": { "prepareSupport": false },
                "formatting": {}
            }
        }
    })
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

    /// Resolve a completion item to fetch its lazy fields (documentation, additionalTextEdits).
    /// The server keys off the echoed `data`; `label` disambiguates.
    pub fn resolve_completion(&self, label: &str, data: &Value) -> io::Result<i64> {
        self.request(
            "completionItem/resolve",
            json!({ "label": label, "data": data }),
        )
    }

    /// Request signature help at a position (the parameter hints while typing a call).
    pub fn signature_help(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        self.request(
            "textDocument/signatureHelp",
            Self::position(uri, line, character),
        )
    }

    /// Request the occurrences of the symbol at a position (read/write highlights).
    pub fn document_highlight(&self, uri: &str, line: u32, character: u32) -> io::Result<i64> {
        self.request(
            "textDocument/documentHighlight",
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

    /// Search for symbols across the workspace by name (server-side fuzzy matching).
    pub fn workspace_symbols(&self, query: &str) -> io::Result<i64> {
        self.request("workspace/symbol", json!({ "query": query }))
    }

    /// Run a server-declared command (§8.4). The result is typically ignored — effects come back
    /// as `workspace/applyEdit`.
    pub fn execute_command(&self, command: &str, arguments: &Value) -> io::Result<i64> {
        self.request(
            "workspace/executeCommand",
            json!({ "command": command, "arguments": arguments }),
        )
    }

    /// Request code actions for a range. `context.diagnostics` is empty for now (quickfixes that
    /// key off it may be limited — see the roadmap); refactor/source actions still apply.
    #[allow(clippy::too_many_arguments)]
    pub fn code_action(
        &self,
        uri: &str,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
    ) -> io::Result<i64> {
        self.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": start_line, "character": start_char },
                    "end": { "line": end_line, "character": end_char }
                },
                "context": { "diagnostics": [], "triggerKind": 1 }
            }),
        )
    }

    /// Request whole-document formatting; `tab_size`/`insert_spaces` come from buffer settings.
    /// The response is a `TextEdit[]` to apply as one atomic group.
    pub fn formatting(&self, uri: &str, tab_size: u32, insert_spaces: bool) -> io::Result<i64> {
        self.request(
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": uri },
                "options": { "tabSize": tab_size, "insertSpaces": insert_spaces }
            }),
        )
    }

    /// Send the `initialize` request only (not `initialized`); returns its JSON-RPC id so the
    /// caller can recognize the response and complete the handshake in order (§3.2): capabilities
    /// must be received before `initialized`, and nothing else may be sent until then.
    pub fn send_initialize(&self, root_uri: &str, client_version: &str) -> io::Result<i64> {
        self.request("initialize", initialize_params(root_uri, client_version))
    }

    /// Send the `initialized` notification — only after `InitializeResult` has arrived.
    pub fn send_initialized(&self) -> io::Result<()> {
        self.notify("initialized", json!({}))
    }

    /// Answer a server→client request with a result. `id` is echoed verbatim.
    pub fn respond(&self, id: &Value, result: Value) -> io::Result<()> {
        self.send(json_response(id, result))
    }

    /// Answer a server→client request with an error (e.g. `-32601` for an unsupported method).
    pub fn respond_err(&self, id: &Value, code: i64, message: &str) -> io::Result<()> {
        self.send(json_error(id, code, message))
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

    /// Ask the server to cancel an in-flight request (§1.4). Advisory: the server still sends a
    /// response (conventionally error `-32800`), so the caller keeps the pending entry until it
    /// arrives.
    pub fn cancel(&self, id: i64) {
        let _ = self.notify("$/cancelRequest", json!({ "id": id }));
    }

    /// Ask the server to shut down cleanly (fire-and-forget: `shutdown` request + `exit`).
    pub fn shutdown(&self) {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
    }

    /// Graceful ordered teardown (§3.8): `shutdown` → `exit` → wait up to `deadline` for the
    /// process to exit → SIGKILL. Never blocks longer than `deadline`. Use this on
    /// restart/quit; `Drop` is only the last-resort kill.
    pub fn stop(&mut self, deadline: Duration) {
        self.shutdown();
        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return, // exited cleanly
                Ok(None) if start.elapsed() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break, // deadline hit, or wait errored
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
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
fn classify(value: &Value) -> Option<Incoming> {
    if let Some(method) = value.get("method").and_then(|m| m.as_str()) {
        if method == "textDocument/publishDiagnostics" {
            return parse_diagnostics(value).map(Incoming::Diagnostics);
        }
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        // `method` + `id` is a server→client request (answer it, §1.3); `method` alone is a
        // notification. The id stays raw — it may be a string and must be echoed verbatim.
        return Some(match value.get("id") {
            Some(id) => Incoming::ServerRequest {
                id: id.clone(),
                method: method.to_string(),
                params,
            },
            None => Incoming::Notification {
                method: method.to_string(),
                params,
            },
        });
    }
    // A response carries a numeric id and a result (or error).
    if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
        if value.get("result").is_some() || value.get("error").is_some() {
            let result = value.get("result").cloned().unwrap_or(Value::Null);
            // Preserve the server's error code + message so the app can apply the error matrix
            // (§9.5) instead of surfacing every failure — a `null` result and a real error look
            // identical otherwise, silently turning e.g. a failed rename into a no-op.
            let error = value.get("error").map(|e| ResponseError {
                code: e.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                message: e
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| e.to_string()),
            });
            return Some(Incoming::Response { id, result, error });
        }
    }
    None
}

/// Build a JSON-RPC success response. Pure (unit-tested); `id` is echoed verbatim (it may be a
/// string).
pub fn json_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC error response (e.g. `-32601 MethodNotFound` for an unsupported request).
pub fn json_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
