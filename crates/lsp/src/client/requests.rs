//! The `LspHandle` request/notification surface: one method per LSP call plus the low-level
//! `send`/`notify`/`request` framing helpers and the server→client response builders.

use std::io::{self, Write};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::transport;

use super::LspHandle;

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

    pub(super) fn notify(&self, method: &str, params: Value) -> io::Result<()> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Send a request, returning the JSON-RPC id so the caller can correlate the response.
    pub(super) fn request(&self, method: &str, params: Value) -> io::Result<i64> {
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

    /// Request code actions for a range. `context.diagnostics` carries the diagnostics overlapping
    /// the range verbatim (echoed from `publishDiagnostics`) so the server can offer quickfixes
    /// bound to them (§6.1); refactor/source actions apply regardless. `triggerKind: 1` = invoked.
    #[allow(clippy::too_many_arguments)]
    pub fn code_action(
        &self,
        uri: &str,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        context_diagnostics: &[Value],
    ) -> io::Result<i64> {
        self.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": start_line, "character": start_char },
                    "end": { "line": end_line, "character": end_char }
                },
                "context": { "diagnostics": context_diagnostics, "triggerKind": 1 }
            }),
        )
    }

    /// Request full-document semantic tokens (§7.1). The response is `{ resultId?, data: uint[] }`
    /// decoded against the server's legend.
    pub fn semantic_tokens_full(&self, uri: &str) -> io::Result<i64> {
        self.request(
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Request code lenses for a document (§6.4). The response is `CodeLens[]` (some unresolved).
    pub fn code_lens(&self, uri: &str) -> io::Result<i64> {
        self.request(
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Resolve a code lens's command lazily (§6.4). `lens` is the original lens JSON.
    pub fn resolve_code_lens(&self, lens: &Value) -> io::Result<i64> {
        self.request("codeLens/resolve", lens.clone())
    }

    /// Request folding ranges for a document (§7.3). The response is `FoldingRange[]`.
    pub fn folding_range(&self, uri: &str) -> io::Result<i64> {
        self.request(
            "textDocument/foldingRange",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Request inlay hints for a range (§7.2). The response is `InlayHint[]`.
    pub fn inlay_hint(
        &self,
        uri: &str,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
    ) -> io::Result<i64> {
        self.request(
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": start_line, "character": start_char },
                    "end": { "line": end_line, "character": end_char }
                }
            }),
        )
    }

    /// Notify the server of watched-file changes it registered for (§8.1). `changes` is a
    /// pre-built `FileEvent[]` (`{ uri, type }`); a fire-and-forget notification.
    pub fn did_change_watched_files(&self, changes: &[Value]) -> io::Result<()> {
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": changes }),
        )
    }

    /// Pull diagnostics for a document (§5.1). `identifier` echoes the server's
    /// `diagnosticProvider.identifier`; `previous_result_id` is the last cached resultId, letting
    /// the server answer `unchanged`. Both are omitted from the params when `None`.
    pub fn diagnostic(
        &self,
        uri: &str,
        identifier: Option<&str>,
        previous_result_id: Option<&str>,
    ) -> io::Result<i64> {
        let mut params = json!({ "textDocument": { "uri": uri } });
        if let Some(id) = identifier {
            params["identifier"] = json!(id);
        }
        if let Some(rid) = previous_result_id {
            params["previousResultId"] = json!(rid);
        }
        self.request("textDocument/diagnostic", params)
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

/// Build a JSON-RPC success response. Pure (unit-tested); `id` is echoed verbatim (it may be a
/// string).
pub(crate) fn json_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC error response (e.g. `-32601 MethodNotFound` for an unsupported request).
pub(crate) fn json_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
