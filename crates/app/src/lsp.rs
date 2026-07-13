//! LSP integration (plan §10): manages a language server per language, forwards document
//! open/change notifications, and correlates request responses (diagnostics, hover,
//! go-to-definition, completion, rename) into high-level [`LspEvent`]s the app acts on.
//!
//! Servers are configured in `config.toml` (`[lsp] rust = "rust-analyzer"`); with none
//! configured the manager is inert, so CI and default runs never require a server.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

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

/// Per-connection lifecycle gate. The Crashed terminal state is represented by removal from
/// `state` + an entry in `failed` (circuit breaker tripped); a live connection is either
/// Initializing or Running.
enum ClientState {
    /// `initialize` sent; awaiting the response (whose id is `init_id`) to store capabilities
    /// and send `initialized`.
    Initializing { init_id: i64 },
    /// Handshake complete; feature requests are gated on these capabilities.
    Running(ServerCaps),
}

/// A message from a connection's forwarding thread. `Exited` is synthesized when the server's
/// stdout closes (the process died), turning a silent crash into an observable event.
enum ClientMsg {
    Msg(Incoming),
    Exited,
}

/// Circuit breaker: if a server crashes this many times within [`CRASH_WINDOW`], stop
/// auto-restarting it *(≈ VS Code default)*.
const CRASH_LIMIT: usize = 5;
const CRASH_WINDOW: Duration = Duration::from_secs(180);

/// Whether the crash `times` (already pruned to the window) have hit the breaker limit.
fn breaker_tripped(times: &[Instant], now: Instant) -> bool {
    times
        .iter()
        .filter(|&&t| now.saturating_duration_since(t) <= CRASH_WINDOW)
        .count()
        >= CRASH_LIMIT
}

/// Exponential restart backoff for the Nth consecutive crash (1-based): 250 ms → 4 s.
fn restart_backoff(crash_count: usize) -> Duration {
    let shift = crash_count.saturating_sub(1).min(4);
    Duration::from_millis((250u64 << shift).min(4000))
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
    /// A `window/showMessage` notice to surface on the statusline.
    Message(String),
    /// A connection's server process exited. The app clears its per-doc `didOpen` bookkeeping for
    /// this language so documents are re-synced when the connection restarts.
    ServerExited {
        lang: String,
    },
    /// A server→client request the app must act on **and answer** (applyEdit,
    /// showMessageRequest, showDocument). `id` is the raw JSON-RPC id to echo in the reply,
    /// sent back through [`LspManager::respond`].
    ServerRequest {
        lang: String,
        id: serde_json::Value,
        method: String,
        params: serde_json::Value,
    },
}

/// Owns the running servers and the merged incoming-message stream.
pub struct LspManager {
    /// Merged inbound stream, tagged with the originating language so responses route to the
    /// right connection even when two servers happen to reuse the same JSON-RPC id (each
    /// connection numbers ids from 1).
    tx: Sender<(String, ClientMsg)>,
    rx: Receiver<(String, ClientMsg)>,
    /// Configured `language → server command` (with args split on whitespace).
    servers: HashMap<String, Vec<String>>,
    /// Live handles by language id.
    clients: HashMap<String, LspHandle>,
    /// Per-connection handshake/lifecycle state by language id.
    state: HashMap<String, ClientState>,
    /// Languages we've given up on (spawn failure or the crash breaker tripped).
    failed: HashMap<String, ()>,
    /// Per-document version counter for didChange.
    versions: HashMap<String, i64>,
    /// In-flight requests by (language, JSON-RPC id), so responses can be interpreted.
    pending: HashMap<(String, i64), Pending>,
    /// URIs each server has published diagnostics for, so they can be cleared on a crash.
    published: HashMap<String, HashSet<String>>,
    /// Recent crash timestamps per language (pruned to `CRASH_WINDOW`) for the breaker.
    crash_times: HashMap<String, Vec<Instant>>,
    /// Earliest instant a language may be respawned, enforcing restart backoff.
    restart_after: HashMap<String, Instant>,
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
            published: HashMap::new(),
            crash_times: HashMap::new(),
            restart_after: HashMap::new(),
            root_uri: uri_for(root),
            client_version,
        }
    }

    /// Drain incoming messages: complete pending handshakes, and turn feature responses into
    /// high-level [`LspEvent`]s by matching each against the request that produced it.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut out = Vec::new();
        while let Ok((lang, cmsg)) = self.rx.try_recv() {
            let msg = match cmsg {
                ClientMsg::Msg(m) => m,
                ClientMsg::Exited => {
                    self.handle_exit(&lang, &mut out);
                    continue;
                }
            };
            match msg {
                Incoming::Diagnostics(u) => {
                    // Remember which docs this server has diagnostics for, so a crash can clear
                    // its now-stale squiggles.
                    self.published
                        .entry(lang.clone())
                        .or_default()
                        .insert(u.uri.clone());
                    out.push(LspEvent::Diagnostics(u));
                }
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
                Incoming::ServerRequest { id, method, params } => {
                    // Every server→client request MUST be answered (§1.3) — silence deadlocks
                    // servers that await the reply. Manager-local ones are answered here; the
                    // rest are routed to the app (which owns docs/UI) to act and answer.
                    match method.as_str() {
                        "workspace/configuration" => {
                            self.respond(&lang, &id, configuration_response(&params))
                        }
                        "workspace/workspaceFolders" => {
                            let folders = workspace_folders_response(&self.root_uri);
                            self.respond(&lang, &id, folders)
                        }
                        "client/registerCapability"
                        | "client/unregisterCapability"
                        | "window/workDoneProgress/create"
                        | "workspace/semanticTokens/refresh"
                        | "workspace/inlayHint/refresh"
                        | "workspace/codeLens/refresh"
                        | "workspace/diagnostic/refresh" => {
                            self.respond(&lang, &id, serde_json::Value::Null)
                        }
                        "workspace/applyEdit"
                        | "window/showMessageRequest"
                        | "window/showDocument" => out.push(LspEvent::ServerRequest {
                            lang: lang.clone(),
                            id,
                            method,
                            params,
                        }),
                        _ => {
                            if let Some(h) = self.clients.get(&lang) {
                                let _ = h.respond_err(&id, -32601, "method not found");
                            }
                        }
                    }
                }
                Incoming::Notification { method, params } => {
                    // window/showMessage → statusline. logMessage / $/progress / telemetry /
                    // unknown are dropped (progress UI + a log view are later PRs).
                    if method == "window/showMessage" {
                        if let Some(msg) = params.get("message").and_then(|m| m.as_str()) {
                            out.push(LspEvent::Message(msg.to_string()));
                        }
                    }
                }
            }
        }
        out
    }

    /// Answer a server→client request for `language`, echoing its raw `id`. Public so the app
    /// can reply after acting on a routed [`LspEvent::ServerRequest`].
    pub fn respond(&self, language: &str, id: &serde_json::Value, result: serde_json::Value) {
        if let Some(client) = self.clients.get(language) {
            let _ = client.respond(id, result);
        }
    }

    /// Handle a connection's process exiting (§3.9). Fails its in-flight requests, clears its
    /// diagnostics, tells the app to re-sync, and applies the restart policy: auto-restart after
    /// exponential backoff unless the crash breaker has tripped ([`CRASH_LIMIT`] in
    /// [`CRASH_WINDOW`]).
    fn handle_exit(&mut self, lang: &str, out: &mut Vec<LspEvent>) {
        self.clients.remove(lang);
        self.state.remove(lang);

        // Fail in-flight requests locally — no response is coming.
        let dead: Vec<(String, i64)> = self
            .pending
            .keys()
            .filter(|(l, _)| l == lang)
            .cloned()
            .collect();
        let had_pending = !dead.is_empty();
        for key in dead {
            self.pending.remove(&key);
        }

        // Clear this server's stale diagnostics from the UI.
        if let Some(uris) = self.published.remove(lang) {
            for uri in uris {
                out.push(LspEvent::Diagnostics(DiagnosticsUpdate {
                    uri,
                    diagnostics: Vec::new(),
                }));
            }
        }

        // Let the app forget per-doc sync bookkeeping so docs re-open after a restart.
        out.push(LspEvent::ServerExited {
            lang: lang.to_string(),
        });
        if had_pending {
            out.push(LspEvent::Error(format!("{lang}: language server exited")));
        }

        // Restart policy: breaker + exponential backoff.
        let now = Instant::now();
        let times = self.crash_times.entry(lang.to_string()).or_default();
        times.retain(|&t| now.saturating_duration_since(t) <= CRASH_WINDOW);
        times.push(now);
        let count = times.len();
        if breaker_tripped(times, now) {
            self.failed.insert(lang.to_string(), ());
            self.restart_after.remove(lang);
            out.push(LspEvent::Error(format!(
                "{lang}: language server crashed {count} times; not restarting"
            )));
        } else {
            self.restart_after
                .insert(lang.to_string(), now + restart_backoff(count));
        }
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
        // Respect restart backoff after a crash: don't respawn until the cool-off passes.
        if let Some(after) = self.restart_after.get(language) {
            if Instant::now() < *after {
                return false;
            }
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
                // language so `poll` can route them (ids collide across connections). A synthetic
                // `Exited` is emitted when the stream closes so a crash becomes observable.
                let tx = self.tx.clone();
                let lang = language.to_string();
                std::thread::spawn(move || {
                    while let Ok(msg) = rx.recv() {
                        if tx.send((lang.clone(), ClientMsg::Msg(msg))).is_err() {
                            return;
                        }
                    }
                    let _ = tx.send((lang, ClientMsg::Exited));
                });
                self.restart_after.remove(language);
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

    /// Notify the server that a document closed (§4.1): its truth reverts to disk. Sends only
    /// once `Running`; drops the per-doc version so a later reopen restarts its counter.
    pub fn did_close(&mut self, path: &Path, language: &str) {
        let uri = uri_for(path);
        self.versions.remove(&uri);
        if self.is_ready(language) {
            if let Some(client) = self.clients.get(language) {
                let _ = client.did_close(&uri);
            }
        }
    }

    /// Gracefully stop all connections (ordered shutdown with a per-server deadline), e.g. on
    /// editor quit. Never blocks longer than `deadline` per server.
    pub fn stop_all(&mut self, deadline: Duration) {
        for (_lang, client) in self.clients.iter_mut() {
            client.stop(deadline);
        }
        self.clients.clear();
        self.state.clear();
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

/// Build the response to `workspace/configuration`: one entry per requested item. We hold no
/// per-server settings yet, so every entry is `null` — but the arity **must** match the request
/// (a wrong-arity response wedges servers like pyright, §3.7).
fn configuration_response(params: &serde_json::Value) -> serde_json::Value {
    let n = params
        .get("items")
        .and_then(|i| i.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    serde_json::Value::Array(vec![serde_json::Value::Null; n])
}

/// Build the response to `workspace/workspaceFolders`: the single project root (§8.2).
fn workspace_folders_response(root_uri: &str) -> serde_json::Value {
    serde_json::json!([{ "uri": root_uri, "name": folder_name(root_uri) }])
}

/// The last path segment of a `file://` root URI, used as the workspace-folder name.
fn folder_name(root_uri: &str) -> String {
    root_uri
        .trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("root")
        .to_string()
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

    /// Push an inbound message onto the merged channel as the forwarding thread would.
    fn feed(mgr: &LspManager, lang: &str, msg: Incoming) {
        mgr.tx.send((lang.into(), ClientMsg::Msg(msg))).unwrap();
    }

    /// Signal that a connection's process exited.
    fn feed_exit(mgr: &LspManager, lang: &str) {
        mgr.tx.send((lang.into(), ClientMsg::Exited)).unwrap();
    }

    #[test]
    fn init_response_stores_caps_and_becomes_ready() {
        // The awaited InitializeResult transitions the connection to Running with parsed caps,
        // and produces no user-facing event (the handshake is internal).
        let mut mgr = manager();
        mgr.state
            .insert("rust".into(), ClientState::Initializing { init_id: 1 });
        let caps = serde_json::json!({ "capabilities": { "hoverProvider": true } });
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: caps,
                error: None,
            },
        );
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
        feed(
            &mgr,
            "py",
            Incoming::Response {
                id: 1,
                result: serde_json::json!({ "contents": "doc" }),
                error: None,
            },
        );
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: Default::default(),
                error: None,
            },
        );
        let events = mgr.poll();
        assert!(events.iter().any(|e| matches!(e, LspEvent::Hover(_))));
        assert!(events.iter().any(|e| matches!(e, LspEvent::Rename(_))));
    }

    #[test]
    fn configuration_response_matches_item_arity() {
        // Wrong arity wedges servers, so a null-per-item response must match the request.
        let params = serde_json::json!({ "items": [{ "section": "a" }, { "section": "b" }] });
        assert_eq!(
            configuration_response(&params),
            serde_json::json!([null, null])
        );
        assert_eq!(
            configuration_response(&serde_json::json!({})),
            serde_json::json!([])
        );
    }

    #[test]
    fn workspace_folders_response_names_the_root() {
        let r = workspace_folders_response("file:///home/g/proj/");
        assert_eq!(r[0]["uri"], "file:///home/g/proj/");
        assert_eq!(r[0]["name"], "proj");
    }

    #[test]
    fn app_needing_requests_route_to_the_app() {
        // applyEdit/showMessageRequest/showDocument are surfaced for the app to act + answer.
        let mut mgr = manager();
        for method in [
            "workspace/applyEdit",
            "window/showMessageRequest",
            "window/showDocument",
        ] {
            feed(
                &mgr,
                "rust",
                Incoming::ServerRequest {
                    id: serde_json::json!(1),
                    method: method.into(),
                    params: serde_json::json!({}),
                },
            );
        }
        let routed: Vec<String> = mgr
            .poll()
            .into_iter()
            .filter_map(|e| match e {
                LspEvent::ServerRequest { method, .. } => Some(method),
                _ => None,
            })
            .collect();
        assert_eq!(
            routed,
            [
                "workspace/applyEdit",
                "window/showMessageRequest",
                "window/showDocument"
            ]
        );
    }

    #[test]
    fn local_requests_and_unknown_do_not_route_to_the_app() {
        // Manager-local requests (configuration, refresh, …) and unknown methods are answered in
        // poll (no handle in tests → no-op) and produce no app event.
        let mut mgr = manager();
        for method in [
            "workspace/configuration",
            "workspace/semanticTokens/refresh",
            "some/unknown/method",
        ] {
            feed(
                &mgr,
                "rust",
                Incoming::ServerRequest {
                    id: serde_json::json!(1),
                    method: method.into(),
                    params: serde_json::json!({ "items": [] }),
                },
            );
        }
        assert!(mgr.poll().is_empty());
    }

    #[test]
    fn show_message_notification_becomes_a_message_event() {
        let mut mgr = manager();
        feed(
            &mgr,
            "rust",
            Incoming::Notification {
                method: "window/showMessage".into(),
                params: serde_json::json!({ "type": 3, "message": "reloading" }),
            },
        );
        let events = mgr.poll();
        assert!(matches!(events.as_slice(), [LspEvent::Message(m)] if m == "reloading"));
    }

    #[test]
    fn error_response_surfaces_as_error_event() {
        // A server error reply to a tracked request becomes an `Error` event, not a parsed
        // (empty) result that would read as "nothing found".
        let mut mgr = manager();
        mgr.pending.insert(("rust".into(), 1), Pending::Rename);
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: Default::default(), // Null; irrelevant on the error path
                error: Some("rename failed".into()),
            },
        );
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
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 2,
                result: Default::default(), // a null result still parses to an (empty) Rename
                error: None,
            },
        );
        let events = mgr.poll();
        assert!(matches!(events.as_slice(), [LspEvent::Rename(_)]));
    }

    #[test]
    fn restart_backoff_is_exponential_and_capped() {
        assert_eq!(restart_backoff(1), Duration::from_millis(250));
        assert_eq!(restart_backoff(2), Duration::from_millis(500));
        assert_eq!(restart_backoff(3), Duration::from_millis(1000));
        assert_eq!(restart_backoff(4), Duration::from_millis(2000));
        assert_eq!(restart_backoff(5), Duration::from_millis(4000));
        assert_eq!(restart_backoff(9), Duration::from_millis(4000)); // capped
    }

    #[test]
    fn breaker_trips_only_within_the_window() {
        let now = Instant::now();
        // Five crashes inside the window → tripped.
        let recent: Vec<Instant> = (0..5).map(|i| now - Duration::from_secs(i * 10)).collect();
        assert!(breaker_tripped(&recent, now));
        // Four recent + one ancient (outside the window) → not tripped.
        let mut spread = recent[..4].to_vec();
        spread.push(now - Duration::from_secs(600));
        assert!(!breaker_tripped(&spread, now));
    }

    #[test]
    fn server_exit_fails_pending_and_clears_diagnostics() {
        let mut mgr = manager();
        mgr.state
            .insert("rust".into(), ClientState::Running(ServerCaps::default()));
        mgr.pending.insert(("rust".into(), 5), Pending::Hover);
        mgr.published
            .entry("rust".into())
            .or_default()
            .insert("file:///a.rs".into());
        feed_exit(&mgr, "rust");
        let events = mgr.poll();

        assert!(!mgr.state.contains_key("rust"), "state not torn down");
        assert!(mgr.pending.is_empty(), "pending not failed locally");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LspEvent::Diagnostics(u) if u.uri == "file:///a.rs" && u.diagnostics.is_empty())),
            "stale diagnostics not cleared"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, LspEvent::ServerExited { lang } if lang == "rust")));
        assert!(events.iter().any(|e| matches!(e, LspEvent::Error(_))));
        // First crash → scheduled for restart, breaker not tripped.
        assert!(mgr.restart_after.contains_key("rust"));
        assert!(!mgr.failed.contains_key("rust"));
    }

    #[test]
    fn breaker_trips_after_repeated_crashes() {
        let mut mgr = manager();
        for _ in 0..CRASH_LIMIT {
            feed_exit(&mgr, "rust");
        }
        let events = mgr.poll();
        assert!(
            mgr.failed.contains_key("rust"),
            "breaker did not trip after {CRASH_LIMIT} crashes"
        );
        assert!(
            !mgr.restart_after.contains_key("rust"),
            "should not be scheduled to restart"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, LspEvent::Error(m) if m.contains("not restarting"))));
    }
}
