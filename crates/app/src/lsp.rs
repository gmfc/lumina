//! LSP integration (plan §10): manages a language server per language, forwards document
//! open/change notifications, and correlates request responses (diagnostics, hover,
//! go-to-definition, completion, rename) into high-level [`LspEvent`]s the app acts on.
//!
//! Servers are configured in `config.toml` (`[lsp] rust = "rust-analyzer"`); with none
//! configured the manager is inert, so CI and default runs never require a server.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};

use editor_lsp::client::{parse_completion, parse_hover, parse_locations, parse_workspace_edit};
use editor_lsp::{
    CompletionItem, DiagnosticsUpdate, Incoming, Location, LspClient, LspHandle, WorkspaceEdit,
};

/// What a pending request was, so its response can be interpreted when it arrives.
#[derive(Debug, Clone, Copy)]
enum Pending {
    Hover,
    Definition,
    Completion,
    Rename,
}

/// A high-level LSP result, handed to the app after correlating a response with its request.
pub enum LspEvent {
    Diagnostics(DiagnosticsUpdate),
    Hover(String),
    Goto(Location),
    Completion(Vec<CompletionItem>),
    Rename(WorkspaceEdit),
}

/// Owns the running servers and the merged incoming-message stream.
pub struct LspManager {
    tx: Sender<Incoming>,
    rx: Receiver<Incoming>,
    /// Configured `language → server command` (with args split on whitespace).
    servers: HashMap<String, Vec<String>>,
    /// Live handles by language id.
    clients: HashMap<String, LspHandle>,
    /// Languages we've already tried (and failed) to spawn, to avoid retry storms.
    failed: HashMap<String, ()>,
    /// Per-document version counter for didChange.
    versions: HashMap<String, i64>,
    /// In-flight requests by JSON-RPC id, so responses can be interpreted.
    pending: HashMap<i64, Pending>,
    root_uri: String,
}

impl LspManager {
    pub fn new(root: &Path, servers: HashMap<String, Vec<String>>) -> LspManager {
        let (tx, rx) = channel();
        LspManager {
            tx,
            rx,
            servers,
            clients: HashMap::new(),
            failed: HashMap::new(),
            versions: HashMap::new(),
            pending: HashMap::new(),
            root_uri: uri_for(root),
        }
    }

    /// Drain incoming messages, turning responses into high-level [`LspEvent`]s by matching
    /// each against the request that produced it.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Incoming::Diagnostics(u) => out.push(LspEvent::Diagnostics(u)),
                Incoming::Response { id, result } => {
                    let Some(kind) = self.pending.remove(&id) else {
                        continue;
                    };
                    match kind {
                        Pending::Hover => {
                            if let Some(text) = parse_hover(&result) {
                                out.push(LspEvent::Hover(text));
                            }
                        }
                        Pending::Definition => {
                            if let Some(loc) = parse_locations(&result).into_iter().next() {
                                out.push(LspEvent::Goto(loc));
                            }
                        }
                        Pending::Completion => {
                            out.push(LspEvent::Completion(parse_completion(&result)));
                        }
                        Pending::Rename => {
                            out.push(LspEvent::Rename(parse_workspace_edit(&result)));
                        }
                    }
                }
            }
        }
        out
    }

    /// Send a request for the active document, recording its kind for response correlation.
    /// `line`/`character` are LSP coordinates (character is a UTF-16 column).
    fn send_request<F>(&mut self, language: &str, kind: Pending, build: F) -> bool
    where
        F: FnOnce(&LspHandle) -> std::io::Result<i64>,
    {
        if !self.ensure_client(language) {
            return false;
        }
        let Some(client) = self.clients.get(language) else {
            return false;
        };
        match build(client) {
            Ok(id) => {
                self.pending.insert(id, kind);
                true
            }
            Err(_) => false,
        }
    }

    pub fn request_hover(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, Pending::Hover, |c| c.hover(&uri, line, character))
    }

    pub fn request_definition(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, Pending::Definition, |c| {
            c.definition(&uri, line, character)
        })
    }

    pub fn request_completion(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, Pending::Completion, |c| {
            c.completion(&uri, line, character)
        })
    }

    pub fn request_rename(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, Pending::Rename, |c| {
            c.rename(&uri, line, character, new_name)
        })
    }

    /// Ensure a server is running for `language`, spawning + wiring its diagnostics if so.
    fn ensure_client(&mut self, language: &str) -> bool {
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
        match LspClient::spawn(&program, &args.unwrap_or_default(), &self.root_uri) {
            Ok((handle, rx)) => {
                // Forward this server's diagnostics onto the shared channel.
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    while let Ok(update) = rx.recv() {
                        if tx.send(update).is_err() {
                            break;
                        }
                    }
                });
                self.clients.insert(language.to_string(), handle);
                true
            }
            Err(_) => {
                self.failed.insert(language.to_string(), ());
                false
            }
        }
    }

    /// Notify the server that a document opened.
    pub fn did_open(&mut self, path: &Path, language: &str, text: &str) {
        if !self.ensure_client(language) {
            return;
        }
        let uri = uri_for(path);
        self.versions.insert(uri.clone(), 1);
        if let Some(client) = self.clients.get(language) {
            let _ = client.did_open(&uri, language, 1, text);
        }
    }

    /// Notify the server that a document changed (full sync).
    pub fn did_change(&mut self, path: &Path, language: &str, text: &str) {
        if !self.clients.contains_key(language) {
            return;
        }
        let uri = uri_for(path);
        let version = self.versions.entry(uri.clone()).or_insert(1);
        *version += 1;
        let v = *version;
        if let Some(client) = self.clients.get(language) {
            let _ = client.did_change(&uri, v, text);
        }
    }

    /// True if any server is configured (so the app knows whether to bother notifying).
    pub fn is_enabled(&self) -> bool {
        !self.servers.is_empty()
    }
}

/// A `file://` URI for a path (best-effort; no percent-encoding of exotic chars).
pub fn uri_for(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// The path from a `file://` URI, if it is one.
pub fn path_from_uri(uri: &str) -> Option<std::path::PathBuf> {
    uri.strip_prefix("file://").map(std::path::PathBuf::from)
}
