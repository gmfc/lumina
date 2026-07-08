//! LSP integration (plan §10): manages a language server per language, forwards document
//! open/change notifications, and collects diagnostics onto one channel the app drains.
//!
//! Servers are configured in `config.toml` (`[lsp] rust = "rust-analyzer"`); with none
//! configured the manager is inert, so CI and default runs never require a server.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};

use editor_lsp::{DiagnosticsUpdate, LspClient, LspHandle};

/// Owns the running servers and the merged diagnostics stream.
pub struct LspManager {
    tx: Sender<DiagnosticsUpdate>,
    rx: Receiver<DiagnosticsUpdate>,
    /// Configured `language → server command` (with args split on whitespace).
    servers: HashMap<String, Vec<String>>,
    /// Live handles by language id.
    clients: HashMap<String, LspHandle>,
    /// Languages we've already tried (and failed) to spawn, to avoid retry storms.
    failed: HashMap<String, ()>,
    /// Per-document version counter for didChange.
    versions: HashMap<String, i64>,
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
            root_uri: uri_for(root),
        }
    }

    /// Drain any diagnostics that have arrived since the last call.
    pub fn poll(&self) -> Vec<DiagnosticsUpdate> {
        let mut out = Vec::new();
        while let Ok(u) = self.rx.try_recv() {
            out.push(u);
        }
        out
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
