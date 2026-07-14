//! LSP integration (plan §10): manages a language server per language, forwards document
//! open/change notifications, and correlates request responses (diagnostics, hover,
//! go-to-definition, completion, rename) into high-level [`LspEvent`]s the app acts on.
//!
//! Servers are configured in `config.toml` (`[lsp] rust = "rust-analyzer"`); with none
//! configured the manager is inert, so CI and default runs never require a server.
//!
//! The manager itself is split across sibling submodules — each an `impl LspManager` block over a
//! single concern (lifecycle, response correlation, server-initiated messages, diagnostics,
//! progress, document sync, requests) — that share the private struct fields declared here.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use editor_lsp::client::{
    parse_capabilities, parse_code_actions, parse_code_lens_resolve, parse_code_lenses,
    parse_completion, parse_completion_item_additional_edits, parse_diagnostic_report,
    parse_document_highlights, parse_document_symbols, parse_folding_ranges, parse_hover,
    parse_inlay_hints, parse_locations, parse_semantic_tokens, parse_signature_help,
    parse_text_edits, parse_workspace_edit, parse_workspace_symbols,
};
use editor_lsp::{
    Cap, CodeAction, CodeLens, CompletionList, DiagnosticsUpdate, DocumentHighlight,
    DocumentSymbol, Incoming, Location, LspClient, LspHandle, PullReport, ResponseError,
    ServerCaps, SignatureHelp, TextEdit, WorkspaceEdit,
};

mod diagnostics;
mod documents;
mod lifecycle;
mod progress;
mod registry;
mod requests;
mod response;
mod server_msgs;
mod status;
mod watchers;

pub(crate) use status::{LangState, LangStatus};

// Re-export the items moved into the submodules back into the `lsp` namespace, so that every
// submodule's `use super::*` (and the in-crate test module) reaches them, and so `LspManager`'s
// fields below can name their types. Methods need no re-export — inherent `impl LspManager` blocks
// in the submodules attach to the struct wherever it is visible.
pub(crate) use lifecycle::{ClientMsg, ClientState};
pub(crate) use progress::ProgressItem;
pub(crate) use response::{is_cancelable, Pending, PendingEntry};

// Re-exports reached only from the in-crate test module (each helper's non-test callers live in
// its own submodule and reference it directly).
#[cfg(test)]
pub(crate) use diagnostics::diag_overlaps;
#[cfg(test)]
pub(crate) use lifecycle::{breaker_tripped, restart_backoff, CRASH_LIMIT};
#[cfg(test)]
pub(crate) use response::format_signature;
#[cfg(test)]
pub(crate) use server_msgs::{configuration_response, workspace_folders_response};

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
    /// Resolved code lenses (§6.4) for the document at `uri` (all carry a `title`).
    CodeLenses {
        uri: String,
        lenses: Vec<CodeLens>,
    },
    /// Foldable regions (§7.3) for the document at `uri`.
    FoldingRanges {
        uri: String,
        ranges: Vec<editor_lsp::FoldingRange>,
    },
    /// The server asked (`workspace/codeLens/refresh`) that lenses be recomputed; the app
    /// re-requests for this language's open docs.
    CodeLensRefresh {
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
    /// Explicit `[lsp]` overrides: `language → server command` (args split on whitespace). An
    /// override is honored verbatim and always wins over the built-in registry (§10, user config).
    overrides: HashMap<String, Vec<String>>,
    /// Whether zero-config discovery is on: when a language has no override, consult the built-in
    /// [`registry`] and probe `$PATH`. Opt-in (production enables it); off by default so unit tests
    /// and the headless harness never auto-spawn a real server.
    discover: bool,
    /// Memoized per-language resolution: `Some(argv)` = an installed server to spawn, `None` =
    /// probed and nothing installed (so we don't re-scan `$PATH` every tick). Overrides resolve to
    /// themselves. Cleared only on construction — a mid-session install is picked up on restart.
    resolved: HashMap<String, Option<Vec<String>>>,
    /// The most recent error message per language (handshake/spawn/crash), shown in the LSP panel's
    /// status row. Set at each `LspEvent::Error` push site; overwritten by the next error.
    last_error: HashMap<String, String>,
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
    /// Resolved code lenses per document URI (§6.4), accumulated as `codeLens/resolve` responses
    /// arrive (only title-bearing lenses). Replaced on the next `textDocument/codeLens`; cleared on
    /// close/crash.
    code_lens: HashMap<String, Vec<CodeLens>>,
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
        overrides: HashMap<String, Vec<String>>,
        client_version: String,
    ) -> LspManager {
        let (tx, rx) = channel();
        LspManager {
            tx,
            rx,
            overrides,
            discover: false,
            resolved: HashMap::new(),
            last_error: HashMap::new(),
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
            code_lens: HashMap::new(),
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
                Incoming::Diagnostics(u) => self.on_diagnostics(&lang, u, &mut out),
                Incoming::Response { id, result, error } => {
                    self.on_response(&lang, id, result, error, &mut out)
                }
                Incoming::ServerRequest { id, method, params } => {
                    self.on_server_request(&lang, id, method, params, &mut out)
                }
                Incoming::Notification { method, params } => {
                    self.on_notification(&lang, &method, &params, &mut out)
                }
            }
        }
        out
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

#[cfg(test)]
mod tests;
