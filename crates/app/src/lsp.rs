//! LSP integration (plan §10): manages a language server per language, forwards document
//! open/change notifications, and correlates request responses (diagnostics, hover,
//! go-to-definition, completion, rename) into high-level [`LspEvent`]s the app acts on.
//!
//! Servers are configured in `config.toml` (`[lsp] rust = "rust-analyzer"`); with none
//! configured the manager is inert, so CI and default runs never require a server.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};

use editor_lsp::client::{
    parse_capabilities, parse_completion, parse_document_symbols, parse_hover, parse_locations,
    parse_workspace_edit,
};
use editor_lsp::{
    Cap, CompletionItem, DiagnosticsUpdate, DocumentSymbol, Incoming, Location, LspClient,
    LspHandle, ServerCaps, WorkspaceEdit,
};

mod requests;

/// What a pending request was, so its response can be interpreted when it arrives.
#[derive(Debug, Clone, Copy)]
enum Pending {
    Hover,
    Definition,
    Completion,
    Rename,
    References,
    DocumentSymbols,
}

/// Per-connection lifecycle. The full Starting/ShuttingDown/Crashed machine is a later PR;
/// PR1 needs only the Initializing→Running gate that a conformant handshake requires.
enum ClientState {
    /// `initialize` sent; awaiting the response (whose id is `init_id`) to store capabilities
    /// and send `initialized`.
    Initializing { init_id: i64 },
    /// Handshake complete; feature requests are gated on these capabilities.
    Running(ServerCaps),
}

/// A high-level LSP result, handed to the app after correlating a response with its request.
pub enum LspEvent {
    Diagnostics(DiagnosticsUpdate),
    Hover(String),
    Goto(Location),
    Completion(Vec<CompletionItem>),
    Rename(WorkspaceEdit),
    References(Vec<Location>),
    DocumentSymbols(Vec<DocumentSymbol>),
    /// The server replied to one of our requests with an error instead of a result.
    Error(String),
}

/// Owns the running servers and the merged incoming-message stream.
pub struct LspManager {
    /// Merged inbound stream, tagged with the originating language so responses route to the
    /// right connection even when two servers happen to reuse the same JSON-RPC id (each
    /// connection numbers ids from 1).
    tx: Sender<(String, Incoming)>,
    rx: Receiver<(String, Incoming)>,
    /// Configured `language → server command` (with args split on whitespace).
    servers: HashMap<String, Vec<String>>,
    /// Live handles by language id.
    clients: HashMap<String, LspHandle>,
    /// Per-connection handshake/lifecycle state by language id.
    state: HashMap<String, ClientState>,
    /// Languages we've already tried (and failed) to spawn, to avoid retry storms.
    failed: HashMap<String, ()>,
    /// Per-document version counter for didChange.
    versions: HashMap<String, i64>,
    /// In-flight requests by (language, JSON-RPC id), so responses can be interpreted.
    pending: HashMap<(String, i64), Pending>,
    root_uri: String,
    /// The editor version, sent in `initialize`'s `clientInfo`.
    client_version: String,
}

impl LspManager {
    pub fn new(
        root: &Path,
        servers: HashMap<String, Vec<String>>,
        client_version: String,
    ) -> LspManager {
        let (tx, rx) = channel();
        LspManager {
            tx,
            rx,
            servers,
            clients: HashMap::new(),
            state: HashMap::new(),
            failed: HashMap::new(),
            versions: HashMap::new(),
            pending: HashMap::new(),
            root_uri: uri_for(root),
            client_version,
        }
    }

    /// Drain incoming messages: complete pending handshakes, and turn feature responses into
    /// high-level [`LspEvent`]s by matching each against the request that produced it.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut out = Vec::new();
        while let Ok((lang, msg)) = self.rx.try_recv() {
            match msg {
                Incoming::Diagnostics(u) => out.push(LspEvent::Diagnostics(u)),
                Incoming::Response { id, result, error } => {
                    // Is this the awaited `InitializeResult` for this connection?
                    let is_init = matches!(
                        self.state.get(&lang),
                        Some(ClientState::Initializing { init_id }) if *init_id == id
                    );
                    if is_init {
                        match error {
                            Some(err) => {
                                // Initialize failed: drop the connection, don't retry.
                                self.clients.remove(&lang);
                                self.state.remove(&lang);
                                self.failed.insert(lang.clone(), ());
                                out.push(LspEvent::Error(format!("initialize failed: {err}")));
                            }
                            None => {
                                // Capabilities in hand → send `initialized` and start serving.
                                let caps = parse_capabilities(&result);
                                if let Some(handle) = self.clients.get(&lang) {
                                    let _ = handle.send_initialized();
                                }
                                self.state.insert(lang.clone(), ClientState::Running(caps));
                            }
                        }
                        continue;
                    }
                    let Some(kind) = self.pending.remove(&(lang.clone(), id)) else {
                        continue;
                    };
                    // A JSON-RPC error response is reported as-is rather than parsing a null
                    // result as "no results" (which would make a failed rename/goto a silent
                    // no-op). Otherwise interpret the result against the request kind.
                    if let Some(message) = error {
                        out.push(LspEvent::Error(message));
                    } else {
                        out.extend(response_event(kind, &result));
                    }
                }
            }
        }
        out
    }

    /// Ensure a connection for `language` is at least started (spawned + `Initializing`).
    /// Returns whether a connection record now exists (initializing or running). Non-blocking:
    /// the handshake completes later in [`LspManager::poll`].
    pub(crate) fn ensure_started(&mut self, language: &str) -> bool {
        if self.clients.contains_key(language) {
            return true;
        }
        if self.failed.contains_key(language) {
            return false;
        }
        let Some(cmd) = self.servers.get(language).cloned() else {
            return false;
        };
        let (program, args) = cmd
            .split_first()
            .map(|(p, a)| (p.clone(), a.to_vec()))
            .unzip();
        let Some(program) = program else {
            return false;
        };
        match LspClient::spawn(
            &program,
            &args.unwrap_or_default(),
            &self.root_uri,
            &self.client_version,
        ) {
            Ok((handle, rx, init_id)) => {
                // Forward this server's messages onto the shared channel, tagged with the
                // language so `poll` can route them (ids collide across connections).
                let tx = self.tx.clone();
                let lang = language.to_string();
                std::thread::spawn(move || {
                    while let Ok(msg) = rx.recv() {
                        if tx.send((lang.clone(), msg)).is_err() {
                            break;
                        }
                    }
                });
                self.clients.insert(language.to_string(), handle);
                self.state
                    .insert(language.to_string(), ClientState::Initializing { init_id });
                true
            }
            Err(_) => {
                self.failed.insert(language.to_string(), ());
                false
            }
        }
    }

    /// True once the handshake completed and the connection is serving requests.
    pub(crate) fn is_ready(&self, language: &str) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(_)))
    }

    /// Gate: the connection is `Running` and advertised support for `cap`.
    fn request_allowed(&self, language: &str, cap: Cap) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(caps)) if caps.allows(cap))
    }

    /// Notify the server that a document opened. Sends only once the connection is `Running`;
    /// returns whether the notification was actually sent (so the caller records the sent
    /// revision only on a real send).
    pub fn did_open(&mut self, path: &Path, language: &str, text: &str) -> bool {
        if !self.is_ready(language) {
            return false;
        }
        let uri = uri_for(path);
        self.versions.insert(uri.clone(), 1);
        if let Some(client) = self.clients.get(language) {
            return client.did_open(&uri, language, 1, text).is_ok();
        }
        false
    }

    /// Notify the server that a document changed (full sync). Sends only once `Running`.
    pub fn did_change(&mut self, path: &Path, language: &str, text: &str) -> bool {
        if !self.is_ready(language) {
            return false;
        }
        let uri = uri_for(path);
        let version = self.versions.entry(uri.clone()).or_insert(1);
        *version += 1;
        let v = *version;
        if let Some(client) = self.clients.get(language) {
            return client.did_change(&uri, v, text).is_ok();
        }
        false
    }

    /// True if any server is configured (so the app knows whether to bother notifying).
    pub fn is_enabled(&self) -> bool {
        !self.servers.is_empty()
    }
}

/// Interpret a successful response `result` against the request `kind` that produced it. Returns
/// `None` when the payload carries nothing to act on (an empty hover / no definition), so the
/// caller simply drops it.
fn response_event(kind: Pending, result: &serde_json::Value) -> Option<LspEvent> {
    Some(match kind {
        Pending::Hover => LspEvent::Hover(parse_hover(result)?),
        Pending::Definition => LspEvent::Goto(parse_locations(result).into_iter().next()?),
        Pending::Completion => LspEvent::Completion(parse_completion(result)),
        Pending::Rename => LspEvent::Rename(parse_workspace_edit(result)),
        Pending::References => LspEvent::References(parse_locations(result)),
        Pending::DocumentSymbols => LspEvent::DocumentSymbols(parse_document_symbols(result)),
    })
}

/// A `file://` URI for a path (best-effort; no percent-encoding of exotic chars).
pub fn uri_for(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// The path from a `file://` URI, if it is one.
pub fn path_from_uri(uri: &str) -> Option<std::path::PathBuf> {
    uri.strip_prefix("file://").map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_lsp::{Cap, Incoming, ServerCaps};

    fn manager() -> LspManager {
        LspManager::new(Path::new("/tmp"), HashMap::new(), "test".into())
    }

    #[test]
    fn init_response_stores_caps_and_becomes_ready() {
        // The awaited InitializeResult transitions the connection to Running with parsed caps,
        // and produces no user-facing event (the handshake is internal).
        let mut mgr = manager();
        mgr.state
            .insert("rust".into(), ClientState::Initializing { init_id: 1 });
        let caps = serde_json::json!({ "capabilities": { "hoverProvider": true } });
        mgr.tx
            .send((
                "rust".into(),
                Incoming::Response {
                    id: 1,
                    result: caps,
                    error: None,
                },
            ))
            .unwrap();
        assert!(mgr.poll().is_empty());
        assert!(mgr.is_ready("rust"));
        assert!(mgr.request_allowed("rust", Cap::Hover));
        assert!(!mgr.request_allowed("rust", Cap::Completion));
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
        mgr.pending.insert(("rust".into(), 1), Pending::Rename);
        mgr.pending.insert(("py".into(), 1), Pending::Hover);
        mgr.tx
            .send((
                "py".into(),
                Incoming::Response {
                    id: 1,
                    result: serde_json::json!({ "contents": "doc" }),
                    error: None,
                },
            ))
            .unwrap();
        mgr.tx
            .send((
                "rust".into(),
                Incoming::Response {
                    id: 1,
                    result: Default::default(),
                    error: None,
                },
            ))
            .unwrap();
        let events = mgr.poll();
        assert!(events.iter().any(|e| matches!(e, LspEvent::Hover(_))));
        assert!(events.iter().any(|e| matches!(e, LspEvent::Rename(_))));
    }

    #[test]
    fn error_response_surfaces_as_error_event() {
        // A server error reply to a tracked request becomes an `Error` event, not a parsed
        // (empty) result that would read as "nothing found".
        let mut mgr = manager();
        mgr.pending.insert(("rust".into(), 1), Pending::Rename);
        mgr.tx
            .send((
                "rust".into(),
                Incoming::Response {
                    id: 1,
                    result: Default::default(), // Null; irrelevant on the error path
                    error: Some("rename failed".into()),
                },
            ))
            .unwrap();
        let events = mgr.poll();
        assert!(
            matches!(events.as_slice(), [LspEvent::Error(m)] if m == "rename failed"),
            "error response did not surface as LspEvent::Error"
        );
    }

    #[test]
    fn success_response_still_parses_result() {
        let mut mgr = manager();
        mgr.pending.insert(("rust".into(), 2), Pending::Rename);
        mgr.tx
            .send((
                "rust".into(),
                Incoming::Response {
                    id: 2,
                    result: Default::default(), // a null result still parses to an (empty) Rename
                    error: None,
                },
            ))
            .unwrap();
        let events = mgr.poll();
        assert!(matches!(events.as_slice(), [LspEvent::Rename(_)]));
    }
}
