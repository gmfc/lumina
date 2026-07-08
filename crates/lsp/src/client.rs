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
use crate::{
    CompletionItem, Diagnostic, DiagnosticsUpdate, Incoming, Location, Severity, TextEdit,
    WorkspaceEdit,
};

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
            return Some(Incoming::Response { id, result });
        }
    }
    None
}

/// Extract hover text from a `textDocument/hover` result. Handles `MarkupContent`,
/// `MarkedString` (string or `{language,value}`), and arrays of those.
pub fn parse_hover(result: &Value) -> Option<String> {
    fn marked_to_string(v: &Value) -> Option<String> {
        if let Some(s) = v.as_str() {
            return Some(s.to_string());
        }
        v.get("value").and_then(|x| x.as_str()).map(String::from)
    }
    let contents = result.get("contents")?;
    let text = if let Some(arr) = contents.as_array() {
        arr.iter()
            .filter_map(marked_to_string)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        marked_to_string(contents)?
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Extract definition location(s). Accepts `Location`, `Location[]`, and `LocationLink[]`.
pub fn parse_locations(result: &Value) -> Vec<Location> {
    fn one(v: &Value) -> Option<Location> {
        // LocationLink uses `targetUri`/`targetSelectionRange`; Location uses `uri`/`range`.
        let (uri, range) = if let Some(uri) = v.get("uri").and_then(|u| u.as_str()) {
            (uri, v.get("range")?)
        } else {
            (
                v.get("targetUri").and_then(|u| u.as_str())?,
                v.get("targetSelectionRange")
                    .or_else(|| v.get("targetRange"))?,
            )
        };
        let start = range.get("start")?;
        let end = range.get("end")?;
        Some(Location {
            uri: uri.to_string(),
            line: start.get("line")?.as_u64()? as u32,
            character: start.get("character")?.as_u64()? as u32,
            end_line: end.get("line")?.as_u64()? as u32,
            end_character: end.get("character")?.as_u64()? as u32,
        })
    }
    match result {
        Value::Array(arr) => arr.iter().filter_map(one).collect(),
        Value::Null => Vec::new(),
        v => one(v).into_iter().collect(),
    }
}

/// Extract completion items. Accepts `CompletionItem[]` or `CompletionList {items}`.
pub fn parse_completion(result: &Value) -> Vec<CompletionItem> {
    let items = if let Some(arr) = result.as_array() {
        arr.clone()
    } else if let Some(arr) = result.get("items").and_then(|i| i.as_array()) {
        arr.clone()
    } else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| {
            let label = it.get("label").and_then(|l| l.as_str())?.to_string();
            // Prefer explicit insertText, then a textEdit's newText, else the label.
            let insert_text = it
                .get("insertText")
                .and_then(|t| t.as_str())
                .map(String::from)
                .or_else(|| {
                    it.get("textEdit")
                        .and_then(|e| e.get("newText"))
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| label.clone());
            let detail = it.get("detail").and_then(|d| d.as_str()).map(String::from);
            let kind = it.get("kind").and_then(|k| k.as_u64()).map(|k| k as u8);
            Some(CompletionItem {
                label,
                detail,
                insert_text,
                kind,
            })
        })
        .collect()
}

/// Parse a rename result (`WorkspaceEdit`). Handles both `changes` and `documentChanges`.
pub fn parse_workspace_edit(result: &Value) -> WorkspaceEdit {
    let mut out = WorkspaceEdit::default();
    let parse_edits =
        |arr: &Vec<Value>| -> Vec<TextEdit> { arr.iter().filter_map(parse_text_edit).collect() };
    if let Some(changes) = result.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            if let Some(arr) = edits.as_array() {
                out.changes.push((uri.clone(), parse_edits(arr)));
            }
        }
    } else if let Some(docs) = result.get("documentChanges").and_then(|d| d.as_array()) {
        for doc in docs {
            let uri = doc
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(|u| u.as_str());
            let edits = doc.get("edits").and_then(|e| e.as_array());
            if let (Some(uri), Some(edits)) = (uri, edits) {
                out.changes.push((uri.to_string(), parse_edits(edits)));
            }
        }
    }
    out
}

fn parse_text_edit(v: &Value) -> Option<TextEdit> {
    let range = v.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some(TextEdit {
        start_line: start.get("line")?.as_u64()? as u32,
        start_char16: start.get("character")?.as_u64()? as u32,
        end_line: end.get("line")?.as_u64()? as u32,
        end_char16: end.get("character")?.as_u64()? as u32,
        new_text: v.get("newText")?.as_str()?.to_string(),
    })
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

    #[test]
    fn classify_distinguishes_notification_and_response() {
        let notif = serde_json::from_str::<Value>(
            r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///a","diagnostics":[]}}"#,
        )
        .unwrap();
        assert!(matches!(classify(&notif), Some(Incoming::Diagnostics(_))));

        let resp =
            serde_json::from_str::<Value>(r#"{"jsonrpc":"2.0","id":7,"result":null}"#).unwrap();
        match classify(&resp) {
            Some(Incoming::Response { id, .. }) => assert_eq!(id, 7),
            other => panic!("expected response, got {other:?}"),
        }
    }

    #[test]
    fn hover_handles_markup_and_marked_array() {
        let markup =
            serde_json::json!({ "contents": { "kind": "markdown", "value": "**x**: i32" } });
        assert_eq!(parse_hover(&markup).as_deref(), Some("**x**: i32"));

        let arr = serde_json::json!({ "contents": ["line 1", { "language": "rust", "value": "fn f()" }] });
        assert_eq!(parse_hover(&arr).as_deref(), Some("line 1\nfn f()"));

        assert_eq!(parse_hover(&serde_json::json!({ "contents": "" })), None);
    }

    #[test]
    fn locations_handle_single_array_and_link() {
        let single = serde_json::json!({
            "uri": "file:///a.rs",
            "range": {"start":{"line":3,"character":2},"end":{"line":3,"character":8}}
        });
        let locs = parse_locations(&single);
        assert_eq!(locs.len(), 1);
        assert_eq!((locs[0].line, locs[0].character), (3, 2));

        let link = serde_json::json!([{
            "targetUri": "file:///b.rs",
            "targetSelectionRange": {"start":{"line":10,"character":0},"end":{"line":10,"character":4}}
        }]);
        let locs = parse_locations(&link);
        assert_eq!(locs[0].uri, "file:///b.rs");
        assert_eq!(locs[0].line, 10);
    }

    #[test]
    fn completion_reads_list_and_array_forms() {
        let list = serde_json::json!({
            "items": [
                {"label": "println!", "insertText": "println!"},
                {"label": "push", "detail": "fn push(&mut self)"}
            ]
        });
        let items = parse_completion(&list);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].insert_text, "println!");
        assert_eq!(items[1].insert_text, "push"); // falls back to label
        assert_eq!(items[1].detail.as_deref(), Some("fn push(&mut self)"));
    }

    #[test]
    fn workspace_edit_parses_changes_map() {
        let edit = serde_json::json!({
            "changes": {
                "file:///a.rs": [
                    {"range":{"start":{"line":1,"character":4},"end":{"line":1,"character":7}},"newText":"bar"}
                ]
            }
        });
        let we = parse_workspace_edit(&edit);
        assert_eq!(we.changes.len(), 1);
        assert_eq!(we.changes[0].0, "file:///a.rs");
        assert_eq!(we.changes[0].1[0].new_text, "bar");
    }
}
