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
    parse_capabilities, parse_code_actions, parse_completion,
    parse_completion_item_additional_edits, parse_diagnostic_report, parse_document_highlights,
    parse_document_symbols, parse_hover, parse_inlay_hints, parse_locations, parse_semantic_tokens,
    parse_signature_help, parse_text_edits, parse_workspace_edit, parse_workspace_symbols,
};
use editor_lsp::{
    Cap, CodeAction, CompletionList, DiagnosticsUpdate, DocumentHighlight, DocumentSymbol,
    Incoming, Location, LspClient, LspHandle, PullReport, ServerCaps, SignatureHelp, TextEdit,
    WorkspaceEdit,
};

mod requests;
mod watchers;

/// What a pending request was, so its response can be interpreted when it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Pending {
    Hover,
    Definition,
    Completion,
    Rename,
    References,
    DocumentSymbols,
    Formatting,
    SignatureHelp,
    DocumentHighlight,
    WorkspaceSymbols,
    CodeAction,
    ResolveCompletion,
    Diagnostic,
    SemanticTokens,
    InlayHint,
}

/// Whether a request kind is auto-cancelled when a newer one of the same kind supersedes it
/// (§9.3): passive, cursor-driven lookups are; explicit user actions (rename, references,
/// symbols) run to completion.
fn is_cancelable(kind: Pending) -> bool {
    matches!(
        kind,
        Pending::Hover
            | Pending::Definition
            | Pending::Completion
            | Pending::SignatureHelp
            | Pending::DocumentHighlight
            // A newer full-doc token / hint request supersedes the older one (typing bursts).
            | Pending::SemanticTokens
            | Pending::InlayHint
    )
}

/// A pending request tagged for staleness detection (§9.4): the document + the version it was
/// asked against, so a response that arrives after the buffer moved can be dropped.
struct PendingEntry {
    kind: Pending,
    uri: String,
    version: i64,
}

/// One active work-done progress token (§1.5), keyed by `(lang, token)` and rendered as a
/// statusline segment. `title` is fixed at `begin`; `message`/`percentage` update on `report`.
struct ProgressItem {
    lang: String,
    token: String,
    title: String,
    message: Option<String>,
    percentage: Option<u32>,
}

impl ProgressItem {
    /// A one-line render: `lang: title — message 45%` (message/percentage omitted when absent).
    fn render(&self) -> String {
        let mut s = format!("{}: {}", self.lang, self.title);
        if let Some(m) = self.message.as_deref().filter(|m| !m.is_empty()) {
            s.push_str(" — ");
            s.push_str(m);
        }
        if let Some(p) = self.percentage {
            s.push_str(&format!(" {p}%"));
        }
        s
    }
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
    Completion(CompletionList),
    /// Late `additionalTextEdits` from `completionItem/resolve` (auto-imports), for the document
    /// the completion was accepted in.
    CompletionResolvedEdits {
        uri: String,
        edits: Vec<TextEdit>,
    },
    Rename(WorkspaceEdit),
    References(Vec<Location>),
    DocumentSymbols(Vec<DocumentSymbol>),
    /// Workspace symbol search results: `(name, location)` pairs for a picker.
    WorkspaceSymbols(Vec<(String, Location)>),
    /// Whole-document formatting edits for the document they were requested against (carried as
    /// a `uri` so a tab switch during the async round-trip can't misapply them to another doc).
    Formatting {
        uri: String,
        edits: Vec<TextEdit>,
    },
    /// Signature help: the active signature line with its active parameter marked, or `None` to
    /// clear the hint (cursor left the call).
    SignatureHelp(Option<String>),
    /// Occurrences of the symbol under the cursor (read/write highlights).
    Highlights(Vec<DocumentHighlight>),
    /// Code actions offered for the cursor/selection (title + edit).
    CodeActions(Vec<CodeAction>),
    /// The server asked (`workspace/diagnostic/refresh`) that pulled diagnostics be recomputed;
    /// the app re-arms its debounced pull for that language's open docs.
    DiagnosticsRefresh {
        lang: String,
    },
    /// Rendered work-done progress for the statusline (§1.5), or `None` when nothing is active.
    /// Aggregates every server's in-flight progress tokens into one line.
    Progress(Option<String>),
    /// Full-document semantic tokens (§7.1) for the document at `uri` (carried so a tab switch
    /// during the round-trip paints the right doc). Already decoded against the server legend.
    SemanticTokens {
        uri: String,
        tokens: Vec<editor_lsp::SemanticToken>,
    },
    /// The server asked (`workspace/semanticTokens/refresh`) that tokens be recomputed; the app
    /// re-requests for this language's open docs.
    SemanticTokensRefresh {
        lang: String,
    },
    /// Inlay hints (§7.2) for the document at `uri`.
    InlayHints {
        uri: String,
        hints: Vec<editor_lsp::InlayHint>,
    },
    /// The server asked (`workspace/inlayHint/refresh`) that hints be recomputed; the app
    /// re-requests for this language's open docs.
    InlayHintRefresh {
        lang: String,
    },
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
    /// In-flight requests by (language, JSON-RPC id), so responses can be interpreted + staleness
    /// checked.
    pending: HashMap<(String, i64), PendingEntry>,
    /// Latest in-flight request id per (language, cancelable kind), for supersede/cancel (§1.4)
    /// and dropping superseded responses (§9.4).
    inflight: HashMap<(String, Pending), i64>,
    /// The open-document set per server (its attach set): URIs we've sent `didOpen` for and not
    /// yet closed. Gates `didClose` (no stray close for a doc this session never opened) and is
    /// cleared on crash.
    open_docs: HashMap<String, HashSet<String>>,
    /// URIs each server has published diagnostics for, so they can be cleared on a crash.
    published: HashMap<String, HashSet<String>>,
    /// The latest raw diagnostic JSON per document URI, so a `codeAction` request can echo the
    /// diagnostics overlapping its range into `context.diagnostics` (§6.1). Replaced on each
    /// publish; cleared on close/crash.
    diag_raw: HashMap<String, Vec<serde_json::Value>>,
    /// The last pull-diagnostics `resultId` per document URI, echoed as `previousResultId` so the
    /// server can answer `unchanged` (§5.1). Dropped on close/crash/refresh.
    diag_result_id: HashMap<String, String>,
    /// Dynamic `didChangeWatchedFiles` registrations, per language then registration id (§8.1).
    /// The client forwards matching disk changes as `workspace/didChangeWatchedFiles`.
    /// Connection-scoped: cleared when a server exits.
    file_watchers: HashMap<String, HashMap<String, Vec<watchers::FileWatcher>>>,
    /// Active work-done progress tokens across all servers (§1.5), in `begin` order for a stable
    /// statusline render. Entries are added on `begin`, updated on `report`, dropped on `end`/crash.
    progress: Vec<ProgressItem>,
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
            inflight: HashMap::new(),
            open_docs: HashMap::new(),
            published: HashMap::new(),
            diag_raw: HashMap::new(),
            diag_result_id: HashMap::new(),
            file_watchers: HashMap::new(),
            progress: Vec::new(),
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
                    // Snapshot the raw diagnostics for this URI (replacing the prior batch) so a
                    // later codeAction request can echo the overlapping ones into its context.
                    if u.raw.is_empty() {
                        self.diag_raw.remove(&u.uri);
                    } else {
                        self.diag_raw.insert(u.uri.clone(), u.raw.clone());
                    }
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
                                out.push(LspEvent::Error(format!(
                                    "initialize failed: {}",
                                    err.message
                                )));
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
                    let Some(entry) = self.pending.remove(&(lang.clone(), id)) else {
                        continue;
                    };
                    // Superseded? A newer request of the same cancelable kind is now in flight,
                    // so this (older) answer is stale even if the buffer never changed (§9.4).
                    let key = (lang.clone(), entry.kind);
                    let superseded = is_cancelable(entry.kind)
                        && self.inflight.get(&key).is_some_and(|&latest| latest != id);
                    if !superseded {
                        self.inflight.remove(&key); // this request resolved its kind's slot
                    }
                    if let Some(err) = error {
                        // Error matrix (§9.5): cancellations/staleness (-32800/-32801/-32802) and
                        // superseded replies are not user errors — drop silently. A real failure
                        // (RequestFailed, etc.) is surfaced.
                        if !superseded && !err.is_droppable() {
                            out.push(LspEvent::Error(err.message));
                        }
                        continue;
                    }
                    // Drop a result that no longer matches the buffer it was computed against — a
                    // stale answer would render/apply at shifted positions (§9.4). Edit-producing
                    // results (rename) must never apply across versions; display results just drop.
                    let current = self
                        .versions
                        .get(&entry.uri)
                        .copied()
                        .unwrap_or(entry.version);
                    if superseded || current != entry.version {
                        continue;
                    }
                    // A pull-diagnostics report caches its resultId and reuses the push path (so it
                    // updates the same diagnostics model + raw snapshot); everything else maps
                    // straight to an event.
                    if entry.kind == Pending::Diagnostic {
                        self.handle_pull_report(&entry.uri, &lang, &result, &mut out);
                    } else if entry.kind == Pending::SemanticTokens {
                        // Decode against this connection's legend (fixed at capability time).
                        let tokens = match self.state.get(&lang) {
                            Some(ClientState::Running(caps)) => {
                                parse_semantic_tokens(&result, &caps.semantic_legend)
                            }
                            _ => Vec::new(),
                        };
                        out.push(LspEvent::SemanticTokens {
                            uri: entry.uri.clone(),
                            tokens,
                        });
                    } else {
                        out.extend(response_event(entry.kind, &entry.uri, &result));
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
                        "client/registerCapability" => {
                            self.register_capability(&lang, &params);
                            self.respond(&lang, &id, serde_json::Value::Null);
                        }
                        "client/unregisterCapability" => {
                            self.unregister_capability(&lang, &params);
                            self.respond(&lang, &id, serde_json::Value::Null);
                        }
                        "window/workDoneProgress/create" | "workspace/codeLens/refresh" => {
                            self.respond(&lang, &id, serde_json::Value::Null)
                        }
                        "workspace/semanticTokens/refresh" => {
                            // Ack, then tell the app to re-request tokens for this language's docs
                            // (project-wide meaning changed, e.g. a dependency reindexed §7.1).
                            self.respond(&lang, &id, serde_json::Value::Null);
                            out.push(LspEvent::SemanticTokensRefresh { lang: lang.clone() });
                        }
                        "workspace/inlayHint/refresh" => {
                            self.respond(&lang, &id, serde_json::Value::Null);
                            out.push(LspEvent::InlayHintRefresh { lang: lang.clone() });
                        }
                        "workspace/diagnostic/refresh" => {
                            // Ack, then drop cached resultIds (forcing a full re-pull) and tell the
                            // app to re-arm its debounced pull for this language's docs (§5.1).
                            self.respond(&lang, &id, serde_json::Value::Null);
                            self.diag_result_id.clear();
                            out.push(LspEvent::DiagnosticsRefresh { lang: lang.clone() });
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
                    // window/showMessage → statusline; $/progress → the work-done spinner (§1.5).
                    // logMessage / telemetry / unknown are dropped (a log view is a later PR).
                    if method == "window/showMessage" {
                        if let Some(msg) = params.get("message").and_then(|m| m.as_str()) {
                            out.push(LspEvent::Message(msg.to_string()));
                        }
                    } else if method == "$/progress" {
                        if let Some(ev) = self.handle_progress(&lang, &params) {
                            out.push(ev);
                        }
                    }
                }
            }
        }
        out
    }

    /// Turn a pull-diagnostics report into the same bookkeeping + event a push would produce
    /// (§5.1). A `full` report replaces the URI's diagnostics (and raw snapshot) and caches its
    /// resultId; an `unchanged` report only refreshes the cached resultId (the UI keeps what it
    /// has). Reusing the push path means pulled diagnostics render + feed codeAction context
    /// identically.
    fn handle_pull_report(
        &mut self,
        uri: &str,
        lang: &str,
        result: &serde_json::Value,
        out: &mut Vec<LspEvent>,
    ) {
        match parse_diagnostic_report(result) {
            PullReport::Full {
                result_id,
                diagnostics,
                raw,
            } => {
                match result_id {
                    Some(rid) => {
                        self.diag_result_id.insert(uri.to_string(), rid);
                    }
                    None => {
                        self.diag_result_id.remove(uri);
                    }
                }
                self.published
                    .entry(lang.to_string())
                    .or_default()
                    .insert(uri.to_string());
                if raw.is_empty() {
                    self.diag_raw.remove(uri);
                } else {
                    self.diag_raw.insert(uri.to_string(), raw.clone());
                }
                out.push(LspEvent::Diagnostics(DiagnosticsUpdate {
                    uri: uri.to_string(),
                    diagnostics,
                    raw,
                }));
            }
            PullReport::Unchanged { result_id } => {
                if let Some(rid) = result_id {
                    self.diag_result_id.insert(uri.to_string(), rid);
                }
            }
        }
    }

    /// Fold a `$/progress` notification into the active work-done set and return the re-rendered
    /// statusline line (§1.5). Values without a `kind` are partial-result streams (we send no
    /// partial-result tokens yet) and are ignored → `None`. `begin` adds, `report` updates,
    /// `end` removes; the token is normalized to a string (it may arrive as a number).
    fn handle_progress(&mut self, lang: &str, params: &serde_json::Value) -> Option<LspEvent> {
        let token = progress_token(params.get("token")?);
        let value = params.get("value")?;
        let same = |p: &ProgressItem| p.lang == lang && p.token == token;
        match value.get("kind")?.as_str()? {
            "begin" => {
                self.progress.retain(|p| !same(p)); // a re-begun token replaces the old one
                self.progress.push(ProgressItem {
                    lang: lang.to_string(),
                    token,
                    title: value
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    message: value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    percentage: value
                        .get("percentage")
                        .and_then(|v| v.as_u64())
                        .map(|p| p as u32),
                });
            }
            "report" => {
                if let Some(item) = self.progress.iter_mut().find(|p| same(p)) {
                    // A report omitting a field leaves the prior value in place.
                    if let Some(m) = value.get("message").and_then(|v| v.as_str()) {
                        item.message = Some(m.to_string());
                    }
                    if let Some(p) = value.get("percentage").and_then(|v| v.as_u64()) {
                        item.percentage = Some(p as u32);
                    }
                } else {
                    return None; // report for an unknown token → nothing to re-render
                }
            }
            "end" => {
                let before = self.progress.len();
                self.progress.retain(|p| !same(p));
                if self.progress.len() == before {
                    return None; // end for an unknown token
                }
            }
            _ => return None,
        }
        Some(LspEvent::Progress(self.render_progress()))
    }

    /// The active progress tokens rendered as one statusline line (` · `-joined), or `None` when
    /// nothing is in flight (so the segment clears).
    fn render_progress(&self) -> Option<String> {
        if self.progress.is_empty() {
            return None;
        }
        let line = self
            .progress
            .iter()
            .map(ProgressItem::render)
            .collect::<Vec<_>>()
            .join(" · ");
        Some(line)
    }

    /// Store dynamic capability registrations for `language` (§3.6). Today only
    /// `workspace/didChangeWatchedFiles` is acted on — its watchers are compiled and kept by
    /// registration id; other methods are accepted (and answered `null` by the caller) but not yet
    /// routed. Registrations are connection-scoped (cleared on exit).
    fn register_capability(&mut self, language: &str, params: &serde_json::Value) {
        let Some(regs) = params.get("registrations").and_then(|r| r.as_array()) else {
            return;
        };
        for reg in regs {
            let (Some(id), Some(method)) = (
                reg.get("id").and_then(|v| v.as_str()),
                reg.get("method").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            if method == "workspace/didChangeWatchedFiles" {
                let opts = reg
                    .get("registerOptions")
                    .unwrap_or(&serde_json::Value::Null);
                let ws = watchers::parse_watchers(opts);
                if !ws.is_empty() {
                    self.file_watchers
                        .entry(language.to_string())
                        .or_default()
                        .insert(id.to_string(), ws);
                }
            }
        }
    }

    /// Remove dynamic registrations by id (§3.6). The spec misspells the key as `unregisterations`
    /// — deserialize that exact key.
    fn unregister_capability(&mut self, language: &str, params: &serde_json::Value) {
        let Some(unregs) = params.get("unregisterations").and_then(|r| r.as_array()) else {
            return;
        };
        if let Some(by_id) = self.file_watchers.get_mut(language) {
            for u in unregs {
                if let Some(id) = u.get("id").and_then(|v| v.as_str()) {
                    by_id.remove(id);
                }
            }
        }
    }

    /// Forward a project-tree change to every server that dynamically registered a matching
    /// watcher (§8.1). The change type is inferred from the path's current existence — the
    /// editor's watcher doesn't distinguish create from modify, so a freshly created file is
    /// reported as `Changed`, which servers treat the same (they re-read the file). Sent only to
    /// `Running` connections; no-op when no watchers are registered.
    pub fn notify_watched_file_change(&self, path: &Path) {
        if self.file_watchers.is_empty() {
            return;
        }
        let change_type = if path.exists() {
            watchers::CHANGED
        } else {
            watchers::DELETED
        };
        for (lang, by_id) in &self.file_watchers {
            if !self.is_ready(lang) {
                continue;
            }
            if watchers::any_match(by_id.values().flatten(), path, change_type) {
                if let Some(client) = self.clients.get(lang) {
                    let change = serde_json::json!({
                        "uri": uri_for(path),
                        "type": change_type,
                    });
                    let _ = client.did_change_watched_files(&[change]);
                }
            }
        }
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
        self.inflight.retain(|(l, _), _| l != lang);
        // The restarted server starts with an empty mirror — forget the old attach set (the app
        // replays didOpen for its docs on resync).
        self.open_docs.remove(lang);
        // Dynamic registrations are connection-scoped: the restarted server re-registers (§3.6).
        self.file_watchers.remove(lang);
        // Drop this server's in-flight progress and refresh the statusline segment (§1.5).
        let had_progress = self.progress.iter().any(|p| p.lang == lang);
        self.progress.retain(|p| p.lang != lang);
        if had_progress {
            out.push(LspEvent::Progress(self.render_progress()));
        }

        // Clear this server's stale diagnostics from the UI (and forget their raw + resultId
        // caches — the restarted server recomputes from scratch).
        if let Some(uris) = self.published.remove(lang) {
            for uri in uris {
                self.diag_raw.remove(&uri);
                self.diag_result_id.remove(&uri);
                out.push(LspEvent::Diagnostics(DiagnosticsUpdate {
                    uri,
                    diagnostics: Vec::new(),
                    raw: Vec::new(),
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

    /// Whether the server declared `command` in `executeCommandProvider.commands` — only declared
    /// commands may be sent to `workspace/executeCommand` (§8.4).
    fn can_execute(&self, language: &str, command: &str) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(caps))
            if caps.execute_commands.iter().any(|c| c == command))
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
            let sent = client.did_open(&uri, language, 1, text).is_ok();
            if sent {
                self.open_docs
                    .entry(language.to_string())
                    .or_default()
                    .insert(uri);
            }
            return sent;
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

    /// Notify the server that a document closed (§4.1): its truth reverts to disk. Sends
    /// `didClose` only for a document this session actually opened (no stray close after a
    /// crash/restart) and drops the doc's per-server bookkeeping.
    pub fn did_close(&mut self, path: &Path, language: &str) {
        let uri = uri_for(path);
        self.versions.remove(&uri);
        self.diag_raw.remove(&uri);
        self.diag_result_id.remove(&uri);
        if let Some(p) = self.published.get_mut(language) {
            p.remove(&uri);
        }
        let was_open = self
            .open_docs
            .get_mut(language)
            .is_some_and(|open| open.remove(&uri));
        if was_open && self.is_ready(language) {
            if let Some(client) = self.clients.get(language) {
                let _ = client.did_close(&uri);
            }
        }
    }

    /// Gracefully stop all connections on quit, running the per-server teardowns **concurrently**
    /// under one global deadline (§3.8): every server gets the full `deadline` in parallel, so a
    /// hung server can't make quit wait `deadline × N` — total quit time stays ~`deadline`.
    pub fn stop_all(&mut self, deadline: Duration) {
        let threads: Vec<_> = self
            .clients
            .drain()
            .map(|(_lang, mut client)| std::thread::spawn(move || client.stop(deadline)))
            .collect();
        for t in threads {
            let _ = t.join();
        }
        self.state.clear();
    }

    /// The last document version synced to the server for `uri`, if any — the baseline a
    /// server-computed `WorkspaceEdit` must match to be safe to apply (§2.4).
    pub fn doc_version(&self, uri: &str) -> Option<i64> {
        self.versions.get(uri).copied()
    }

    /// The raw diagnostics for `uri` whose range overlaps the (LSP, UTF-16) range `[start, end]`,
    /// to echo into a `codeAction` request's `context.diagnostics` (§6.1) so the server can offer
    /// quickfixes bound to them.
    fn context_diagnostics(
        &self,
        uri: &str,
        start: (u32, u32),
        end: (u32, u32),
    ) -> Vec<serde_json::Value> {
        self.diag_raw
            .get(uri)
            .map(|ds| {
                ds.iter()
                    .filter(|d| diag_overlaps(d, start, end))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True if any server is configured (so the app knows whether to bother notifying).
    pub fn is_enabled(&self) -> bool {
        !self.servers.is_empty()
    }

    /// Whether `language`'s server is `Running` and advertised pull diagnostics (§5.1) — the gate
    /// for the app's debounced `textDocument/diagnostic` polling.
    pub fn supports_pull(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::PullDiagnostics)
    }

    /// Whether `language`'s server is `Running` and advertised full-document semantic tokens
    /// (§7.1) — the gate for the app requesting them alongside each doc sync.
    pub fn supports_semantic_tokens(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::SemanticTokens)
    }

    /// Whether `language`'s server is `Running` and advertised inlay hints (§7.2).
    pub fn supports_inlay_hints(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::InlayHint)
    }
}

/// Interpret a successful response `result` against the request `kind` that produced it. Returns
/// `None` when the payload carries nothing to act on (an empty hover / no definition), so the
/// caller simply drops it.
fn response_event(kind: Pending, uri: &str, result: &serde_json::Value) -> Option<LspEvent> {
    Some(match kind {
        Pending::Hover => LspEvent::Hover(parse_hover(result)?),
        Pending::Definition => LspEvent::Goto(parse_locations(result).into_iter().next()?),
        Pending::Completion => LspEvent::Completion(parse_completion(result)),
        Pending::ResolveCompletion => LspEvent::CompletionResolvedEdits {
            uri: uri.to_string(),
            edits: parse_completion_item_additional_edits(result),
        },
        Pending::Rename => LspEvent::Rename(parse_workspace_edit(result)),
        Pending::References => LspEvent::References(parse_locations(result)),
        Pending::DocumentSymbols => LspEvent::DocumentSymbols(parse_document_symbols(result)),
        Pending::WorkspaceSymbols => LspEvent::WorkspaceSymbols(parse_workspace_symbols(result)),
        Pending::Formatting => LspEvent::Formatting {
            uri: uri.to_string(),
            edits: parse_text_edits(result),
        },
        // Always emit (even `None`) so the statusline hint clears when the cursor leaves a call.
        Pending::SignatureHelp => {
            LspEvent::SignatureHelp(parse_signature_help(result).map(|s| format_signature(&s)))
        }
        // Always emit (even empty) so highlights clear when the cursor leaves a symbol.
        Pending::DocumentHighlight => LspEvent::Highlights(parse_document_highlights(result)),
        Pending::CodeAction => LspEvent::CodeActions(parse_code_actions(result)),
        Pending::InlayHint => LspEvent::InlayHints {
            uri: uri.to_string(),
            hints: parse_inlay_hints(result),
        },
        // Pull-diagnostics reports are handled in `poll` (they need the resultId cache), never here.
        Pending::Diagnostic => return None,
        // Semantic tokens are decoded in `poll` (they need the connection legend), never here.
        Pending::SemanticTokens => return None,
    })
}

/// Render a signature line for the statusline, marking the active parameter with brackets.
fn format_signature(sig: &SignatureHelp) -> String {
    match sig.active_param {
        Some((s, e)) => {
            let chars: Vec<char> = sig.label.chars().collect();
            if s <= e && e <= chars.len() {
                let pre: String = chars[..s].iter().collect();
                let mid: String = chars[s..e].iter().collect();
                let post: String = chars[e..].iter().collect();
                format!("{pre}[{mid}]{post}")
            } else {
                sig.label.clone()
            }
        }
        None => sig.label.clone(),
    }
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

/// Whether a raw diagnostic's `range` intersects the (LSP, UTF-16) range `[start, end]` — the
/// test for including it in a `codeAction` request's `context.diagnostics` (§6.1). Positions are
/// compared as lexicographic `(line, character)` tuples; a missing/malformed range means "don't
/// include" (it can't be positioned against the request).
fn diag_overlaps(raw: &serde_json::Value, start: (u32, u32), end: (u32, u32)) -> bool {
    let pos = |obj: &serde_json::Value, key: &str| -> Option<(u32, u32)> {
        let p = obj.get(key)?;
        Some((
            p.get("line")?.as_u64()? as u32,
            p.get("character")?.as_u64()? as u32,
        ))
    };
    let Some(range) = raw.get("range") else {
        return false;
    };
    let (Some(ds), Some(de)) = (pos(range, "start"), pos(range, "end")) else {
        return false;
    };
    // Intersection of [ds, de] with [start, end] in lexicographic (line, char) order. Touching
    // endpoints count (a point request on a diagnostic boundary still surfaces its fix).
    ds <= end && start <= de
}

/// Normalize a `$/progress` token to a string key — it may arrive as a JSON string or number.
fn progress_token(v: &serde_json::Value) -> String {
    v.as_str()
        .map(String::from)
        .unwrap_or_else(|| v.to_string())
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
    use editor_lsp::{Cap, Incoming, ResponseError, ServerCaps};

    fn manager() -> LspManager {
        LspManager::new(Path::new("/tmp"), HashMap::new(), "test".into())
    }

    /// A pending entry for a request kind (staleness fields default to a fresh doc at version 0).
    fn pend(kind: Pending) -> PendingEntry {
        PendingEntry {
            kind,
            uri: "file:///x.rs".into(),
            version: 0,
        }
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
        mgr.pending
            .insert(("rust".into(), 1), pend(Pending::Rename));
        mgr.pending.insert(("py".into(), 1), pend(Pending::Hover));
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
            "window/workDoneProgress/create",
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
        mgr.pending
            .insert(("rust".into(), 1), pend(Pending::Rename));
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: Default::default(), // Null; irrelevant on the error path
                error: Some(ResponseError {
                    code: -32603,
                    message: "rename failed".into(),
                }),
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
        mgr.pending
            .insert(("rust".into(), 2), pend(Pending::Rename));
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
    fn format_signature_brackets_the_active_param() {
        let sig = editor_lsp::SignatureHelp {
            label: "f(a, b)".into(),
            active_param: Some((2, 3)),
        };
        assert_eq!(format_signature(&sig), "f([a], b)");
        let none = editor_lsp::SignatureHelp {
            label: "f()".into(),
            active_param: None,
        };
        assert_eq!(format_signature(&none), "f()");
    }

    #[test]
    fn stale_response_by_version_is_dropped() {
        // The buffer moved (v3 → v5) after we asked; the answer is for the old text → drop it.
        let mut mgr = manager();
        mgr.versions.insert("file:///x.rs".into(), 5);
        mgr.pending.insert(
            ("rust".into(), 1),
            PendingEntry {
                kind: Pending::Hover,
                uri: "file:///x.rs".into(),
                version: 3,
            },
        );
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: serde_json::json!({ "contents": "doc" }),
                error: None,
            },
        );
        assert!(mgr.poll().is_empty());
    }

    #[test]
    fn superseded_cancelable_response_is_dropped() {
        // Two hovers in flight; id 2 supersedes id 1. The late id-1 answer is dropped; id 2 wins.
        let mut mgr = manager();
        mgr.pending.insert(("rust".into(), 1), pend(Pending::Hover));
        mgr.pending.insert(("rust".into(), 2), pend(Pending::Hover));
        mgr.inflight.insert(("rust".into(), Pending::Hover), 2);
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: serde_json::json!({ "contents": "old" }),
                error: None,
            },
        );
        assert!(mgr.poll().is_empty(), "superseded response was not dropped");
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 2,
                result: serde_json::json!({ "contents": "new" }),
                error: None,
            },
        );
        assert!(matches!(mgr.poll().as_slice(), [LspEvent::Hover(m)] if m == "new"));
    }

    #[test]
    fn content_modified_error_is_dropped_silently() {
        // -32801 ContentModified (and -32800/-32802) are not user errors — no Error event.
        let mut mgr = manager();
        mgr.pending
            .insert(("rust".into(), 1), pend(Pending::Rename));
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: Default::default(),
                error: Some(ResponseError {
                    code: ResponseError::CONTENT_MODIFIED,
                    message: "stale".into(),
                }),
            },
        );
        assert!(mgr.poll().is_empty());
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
        mgr.pending.insert(("rust".into(), 5), pend(Pending::Hover));
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
    fn did_close_only_for_opened_docs_and_prunes_bookkeeping() {
        let mut mgr = manager();
        let uri = uri_for(Path::new("/x.rs"));
        // Never opened this session → no stray close, nothing to prune (no panic).
        mgr.did_close(Path::new("/x.rs"), "rust");
        assert!(mgr.open_docs.get("rust").is_none_or(|s| s.is_empty()));

        // Simulate an opened doc with diagnostics (as did_open / a publish would record).
        mgr.open_docs
            .entry("rust".into())
            .or_default()
            .insert(uri.clone());
        mgr.published
            .entry("rust".into())
            .or_default()
            .insert(uri.clone());
        mgr.did_close(Path::new("/x.rs"), "rust");
        // Closing removes it from the attach set and the published set (no unbounded growth).
        assert!(mgr.open_docs.get("rust").is_none_or(|s| !s.contains(&uri)));
        assert!(mgr.published.get("rust").is_none_or(|s| !s.contains(&uri)));
    }

    #[test]
    fn diag_overlaps_matches_point_and_range() {
        let d = |sl, sc, el, ec| {
            serde_json::json!({
                "range": {
                    "start": {"line": sl, "character": sc},
                    "end": {"line": el, "character": ec}
                }
            })
        };
        let diag = d(1, 4, 1, 9); // line 1, cols 4..9
                                  // Point inside → overlaps; on either boundary → overlaps (touching counts).
        assert!(diag_overlaps(&diag, (1, 6), (1, 6)));
        assert!(diag_overlaps(&diag, (1, 4), (1, 4)));
        assert!(diag_overlaps(&diag, (1, 9), (1, 9)));
        // Point before/after the range on the same line → no.
        assert!(!diag_overlaps(&diag, (1, 3), (1, 3)));
        assert!(!diag_overlaps(&diag, (1, 10), (1, 10)));
        // A different line → no.
        assert!(!diag_overlaps(&diag, (2, 6), (2, 6)));
        // A wider selection that straddles the range → yes.
        assert!(diag_overlaps(&diag, (0, 0), (5, 0)));
        // A diagnostic with no range is never included.
        assert!(!diag_overlaps(&serde_json::json!({}), (0, 0), (9, 9)));
    }

    #[test]
    fn code_action_context_echoes_overlapping_diagnostics() {
        // A publish snapshots raw diagnostics per URI; a codeAction at a point echoes only the
        // ones overlapping it (preserving `data`), and a re-publish replaces the snapshot.
        let mut mgr = manager();
        let uri = "file:///x.rs";
        let raw = vec![
            serde_json::json!({
                "range":{"start":{"line":1,"character":4},"end":{"line":1,"character":9}},
                "severity":1,"message":"here","data":{"fix":1}
            }),
            serde_json::json!({
                "range":{"start":{"line":3,"character":0},"end":{"line":3,"character":2}},
                "severity":2,"message":"elsewhere"
            }),
        ];
        feed(
            &mgr,
            "rust",
            Incoming::Diagnostics(DiagnosticsUpdate {
                uri: uri.into(),
                diagnostics: Vec::new(), // model diagnostics irrelevant to this path
                raw: raw.clone(),
            }),
        );
        let _ = mgr.poll(); // stores the raw snapshot
                            // A point on line 1 col 6 sees only the first diagnostic.
        let ctx = mgr.context_diagnostics(uri, (1, 6), (1, 6));
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx[0].get("data"), Some(&serde_json::json!({"fix":1})));
        // A point on line 3 sees only the second.
        assert_eq!(mgr.context_diagnostics(uri, (3, 1), (3, 1)).len(), 1);
        // A point on an empty line sees none.
        assert!(mgr.context_diagnostics(uri, (2, 0), (2, 0)).is_empty());
        // An empty publish clears the snapshot.
        feed(
            &mgr,
            "rust",
            Incoming::Diagnostics(DiagnosticsUpdate {
                uri: uri.into(),
                diagnostics: Vec::new(),
                raw: Vec::new(),
            }),
        );
        let _ = mgr.poll();
        assert!(mgr.context_diagnostics(uri, (1, 6), (1, 6)).is_empty());
    }

    #[test]
    fn pull_full_report_publishes_and_caches_result_id() {
        // A full pull report reuses the push path: it emits a Diagnostics event, caches the
        // resultId for the next `previousResultId`, and snapshots the raw for codeAction context.
        let mut mgr = manager();
        mgr.pending
            .insert(("rust".into(), 1), pend(Pending::Diagnostic));
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: serde_json::json!({
                    "kind":"full","resultId":"r1",
                    "items":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
                              "severity":1,"message":"boom","data":{"fix":1}}]
                }),
                error: None,
            },
        );
        let events = mgr.poll();
        assert!(
            events.iter().any(
                |e| matches!(e, LspEvent::Diagnostics(u) if u.uri == "file:///x.rs" && u.diagnostics.len() == 1)
            ),
            "full pull report did not publish diagnostics"
        );
        assert_eq!(
            mgr.diag_result_id.get("file:///x.rs").map(String::as_str),
            Some("r1")
        );
        // The raw snapshot feeds codeAction context at the diagnostic's position.
        assert_eq!(
            mgr.context_diagnostics("file:///x.rs", (0, 0), (0, 0))
                .len(),
            1
        );
    }

    #[test]
    fn pull_unchanged_report_keeps_diagnostics_and_updates_result_id() {
        // An `unchanged` report emits no event and preserves the existing raw snapshot; only the
        // cached resultId advances.
        let mut mgr = manager();
        mgr.diag_raw.insert(
            "file:///x.rs".into(),
            vec![serde_json::json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}})],
        );
        mgr.pending
            .insert(("rust".into(), 1), pend(Pending::Diagnostic));
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: serde_json::json!({ "kind":"unchanged", "resultId":"r2" }),
                error: None,
            },
        );
        let events = mgr.poll();
        assert!(
            !events.iter().any(|e| matches!(e, LspEvent::Diagnostics(_))),
            "unchanged report should emit no diagnostics event"
        );
        assert_eq!(
            mgr.diag_result_id.get("file:///x.rs").map(String::as_str),
            Some("r2")
        );
        assert!(
            mgr.diag_raw.contains_key("file:///x.rs"),
            "unchanged report must keep the existing raw snapshot"
        );
    }

    #[test]
    fn diagnostic_refresh_drops_result_ids_and_emits_refresh() {
        // `workspace/diagnostic/refresh` is answered, drops cached resultIds (forcing a full
        // re-pull), and surfaces a DiagnosticsRefresh so the app re-arms its debounced pull.
        let mut mgr = manager();
        mgr.diag_result_id
            .insert("file:///x.rs".into(), "r1".into());
        feed(
            &mgr,
            "rust",
            Incoming::ServerRequest {
                id: serde_json::json!(7),
                method: "workspace/diagnostic/refresh".into(),
                params: serde_json::json!({}),
            },
        );
        let events = mgr.poll();
        assert!(mgr.diag_result_id.is_empty(), "resultIds were not dropped");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LspEvent::DiagnosticsRefresh { lang } if lang == "rust")),
            "refresh did not surface a DiagnosticsRefresh event"
        );
    }

    #[test]
    fn register_capability_request_stores_watchers_and_answers_locally() {
        // A `client/registerCapability` for didChangeWatchedFiles is answered locally (no app
        // event) and compiles + stores the watchers by registration id (§3.6/§8.1).
        let mut mgr = manager();
        feed(
            &mgr,
            "rust",
            Incoming::ServerRequest {
                id: serde_json::json!(1),
                method: "client/registerCapability".into(),
                params: serde_json::json!({ "registrations": [{
                    "id": "w1",
                    "method": "workspace/didChangeWatchedFiles",
                    "registerOptions": { "watchers": [{ "globPattern": "**/*.rs" }] }
                }]}),
            },
        );
        assert!(
            mgr.poll().is_empty(),
            "registration must not surface an app event"
        );
        assert!(
            mgr.file_watchers
                .get("rust")
                .is_some_and(|m| m.contains_key("w1")),
            "watchers were not stored"
        );
    }

    #[test]
    fn unregister_capability_removes_by_misspelled_key() {
        let mut mgr = manager();
        mgr.register_capability(
            "rust",
            &serde_json::json!({ "registrations": [{
                "id": "w1",
                "method": "workspace/didChangeWatchedFiles",
                "registerOptions": { "watchers": [{ "globPattern": "**/*.rs" }] }
            }]}),
        );
        assert!(mgr
            .file_watchers
            .get("rust")
            .is_some_and(|m| m.contains_key("w1")));
        // The spec misspells the params key as `unregisterations`.
        mgr.unregister_capability(
            "rust",
            &serde_json::json!({ "unregisterations": [{
                "id": "w1", "method": "workspace/didChangeWatchedFiles"
            }]}),
        );
        assert!(mgr
            .file_watchers
            .get("rust")
            .is_none_or(|m| !m.contains_key("w1")));
    }

    #[test]
    fn server_exit_clears_dynamic_registrations() {
        // Dynamic registrations are connection-scoped: a crash drops them (the restart re-registers).
        let mut mgr = manager();
        mgr.register_capability(
            "rust",
            &serde_json::json!({ "registrations": [{
                "id": "w1",
                "method": "workspace/didChangeWatchedFiles",
                "registerOptions": { "watchers": [{ "globPattern": "**/*.rs" }] }
            }]}),
        );
        assert!(mgr.file_watchers.contains_key("rust"));
        feed_exit(&mgr, "rust");
        let _ = mgr.poll();
        assert!(
            !mgr.file_watchers.contains_key("rust"),
            "registrations not cleared on exit"
        );
    }

    #[test]
    fn progress_begin_report_end_updates_the_statusline_line() {
        let mut mgr = manager();
        // begin → a Progress line naming the server + title.
        feed(
            &mgr,
            "rust",
            Incoming::Notification {
                method: "$/progress".into(),
                params: serde_json::json!({ "token":"idx",
                    "value": { "kind":"begin", "title":"Indexing", "percentage": 0 } }),
            },
        );
        assert!(
            matches!(mgr.poll().as_slice(), [LspEvent::Progress(Some(s))] if s.contains("rust") && s.contains("Indexing"))
        );
        // report → message + percentage fold in.
        feed(
            &mgr,
            "rust",
            Incoming::Notification {
                method: "$/progress".into(),
                params: serde_json::json!({ "token":"idx",
                    "value": { "kind":"report", "message":"crate 3/7", "percentage": 42 } }),
            },
        );
        assert!(
            matches!(mgr.poll().as_slice(), [LspEvent::Progress(Some(s))] if s.contains("42%") && s.contains("crate 3/7"))
        );
        // end → the token clears; with nothing else active the line goes empty.
        feed(
            &mgr,
            "rust",
            Incoming::Notification {
                method: "$/progress".into(),
                params: serde_json::json!({ "token":"idx", "value": { "kind":"end" } }),
            },
        );
        assert!(matches!(mgr.poll().as_slice(), [LspEvent::Progress(None)]));
        assert!(mgr.progress.is_empty());
    }

    #[test]
    fn progress_without_kind_is_ignored() {
        // A `$/progress` on a partial-result token carries a list chunk (no `kind`) — not work-done
        // progress. We send no partial-result tokens yet, so it's dropped.
        let mut mgr = manager();
        feed(
            &mgr,
            "rust",
            Incoming::Notification {
                method: "$/progress".into(),
                params: serde_json::json!({ "token":"pr", "value": [ {"x":1} ] }),
            },
        );
        assert!(mgr.poll().is_empty());
        assert!(mgr.progress.is_empty());
    }

    #[test]
    fn server_exit_clears_progress() {
        let mut mgr = manager();
        mgr.progress.push(ProgressItem {
            lang: "rust".into(),
            token: "idx".into(),
            title: "Indexing".into(),
            message: None,
            percentage: None,
        });
        feed_exit(&mgr, "rust");
        let events = mgr.poll();
        assert!(mgr.progress.is_empty(), "progress not cleared on exit");
        assert!(events.iter().any(|e| matches!(e, LspEvent::Progress(None))));
    }

    #[test]
    fn semantic_tokens_response_decodes_with_the_connection_legend() {
        // A `.../full` response is decoded in poll against the Running connection's legend into a
        // SemanticTokens event for the requested uri.
        let mut mgr = manager();
        mgr.state.insert(
            "rust".into(),
            ClientState::Running(ServerCaps {
                semantic_tokens: true,
                semantic_legend: editor_lsp::SemanticLegend {
                    token_types: vec!["function".into()],
                    token_modifiers: Vec::new(),
                },
                ..Default::default()
            }),
        );
        mgr.pending
            .insert(("rust".into(), 1), pend(Pending::SemanticTokens));
        feed(
            &mgr,
            "rust",
            Incoming::Response {
                id: 1,
                result: serde_json::json!({ "data": [0, 0, 4, 0, 0] }),
                error: None,
            },
        );
        let events = mgr.poll();
        assert!(
            matches!(events.as_slice(), [LspEvent::SemanticTokens { uri, tokens }]
                if uri == "file:///x.rs" && tokens.len() == 1 && tokens[0].token_type == "function"),
            "semantic tokens response did not decode into a SemanticTokens event"
        );
    }

    #[test]
    fn semantic_tokens_refresh_surfaces_an_event() {
        let mut mgr = manager();
        feed(
            &mgr,
            "rust",
            Incoming::ServerRequest {
                id: serde_json::json!(3),
                method: "workspace/semanticTokens/refresh".into(),
                params: serde_json::Value::Null,
            },
        );
        assert!(mgr
            .poll()
            .iter()
            .any(|e| matches!(e, LspEvent::SemanticTokensRefresh { lang } if lang == "rust")));
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
