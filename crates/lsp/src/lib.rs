//! `editor_lsp` — a minimal Language Server Protocol client (plan §10).
//!
//! You write the transport (JSON-RPC over the server's stdio); `lsp-types` provides the
//! message structs. This crate implements message framing, char↔UTF-16 position conversion
//! (LSP counts UTF-16 code units), and a client that spawns a server, runs the initialize
//! handshake, streams document changes, and surfaces `publishDiagnostics`.
//!
//! The transport + position code is deterministic and unit-tested; the spawning client is
//! integration-only (needs a real server binary), so CI never depends on one.

pub mod client;
pub mod position;
pub mod transport;

pub use client::{LspClient, LspHandle};

/// A message from a server to the client.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// A `textDocument/publishDiagnostics` notification (special-cased: it is by far the most
    /// common inbound notification and has a dedicated parser).
    Diagnostics(DiagnosticsUpdate),
    /// A response to one of our requests, correlated by `id`.
    Response {
        id: i64,
        result: serde_json::Value,
        /// `Some(err)` when the server replied with a JSON-RPC `error` object instead of a
        /// result. Kept distinct from a `null` result so a failed request (rename, goto, …) can
        /// be surfaced to the user rather than silently degrading to "no result". The `code`
        /// drives the error matrix (§9.5) — cancellations are dropped, real failures surfaced.
        error: Option<ResponseError>,
    },
    /// A server→client **request** (has both `method` and `id`). Every one must be answered
    /// (§1.3) — silence deadlocks servers that await the reply. `id` is kept as a raw JSON value
    /// because server ids may be strings and must be echoed verbatim.
    ServerRequest {
        id: serde_json::Value,
        method: String,
        params: serde_json::Value,
    },
    /// A server→client notification other than diagnostics (`method`, no `id`).
    Notification {
        method: String,
        params: serde_json::Value,
    },
}

/// A source location in (line, UTF-16 char) coordinates — the result of go-to-definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub uri: String,
    pub line: u32,
    pub character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// A document symbol flattened to a jump target: its name, `SymbolKind`, and start position
/// in the current file (line, UTF-16 char). Hierarchy is flattened with a depth for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: u8,
    pub line: u32,
    pub character: u32,
    pub depth: usize,
}

/// A completion candidate: what the popup shows and what gets inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    /// LSP `CompletionItemKind` (1..=25), if the server sent one — drives the kind glyph.
    pub kind: Option<u8>,
}

/// A single text edit in (line, UTF-16 char) coordinates (used by rename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start_line: u32,
    pub start_char16: u32,
    pub end_line: u32,
    pub end_char16: u32,
    pub new_text: String,
}

/// A set of edits grouped by document URI — a `WorkspaceEdit` (used by rename).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceEdit {
    pub changes: Vec<(String, Vec<TextEdit>)>,
}

/// Severity of a diagnostic (simplified from `lsp_types::DiagnosticSeverity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

/// A diagnostic mapped into the editor's terms: a range in (line, UTF-16 char) coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: u32,
    pub start_char16: u32,
    pub end_line: u32,
    pub end_char16: u32,
    pub severity: Severity,
    pub message: String,
    /// The producing tool (e.g. `rustc`, `clippy`), shown as a prefix.
    pub source: Option<String>,
    /// The diagnostic code (e.g. `E0425`), shown as a suffix. LSP allows a string or number.
    pub code: Option<String>,
}

/// A batch of diagnostics for one document URI (as sent by `publishDiagnostics`).
#[derive(Debug, Clone)]
pub struct DiagnosticsUpdate {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// A JSON-RPC error object from a response. The `code` drives the client's error matrix (§9.5):
/// `RequestCancelled`/`ContentModified`/`ServerCancelled` are dropped silently (not real
/// failures), while `RequestFailed` and everything else is surfaced to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
}

impl ResponseError {
    /// The reply to a request the client itself cancelled — expected, drop it.
    pub const REQUEST_CANCELLED: i64 = -32800;
    /// The result would be stale (the document changed) — not a user error, drop it.
    pub const CONTENT_MODIFIED: i64 = -32801;
    /// The server shed load — safe to re-send once.
    pub const SERVER_CANCELLED: i64 = -32802;
    /// A legitimate failure with a user-relevant message — surface it.
    pub const REQUEST_FAILED: i64 = -32803;

    /// Whether this error is a cancellation/staleness signal that should be dropped silently
    /// rather than shown to the user.
    pub fn is_droppable(&self) -> bool {
        matches!(
            self.code,
            Self::REQUEST_CANCELLED | Self::CONTENT_MODIFIED | Self::SERVER_CANCELLED
        )
    }
}

/// The position encoding negotiated for a connection. LSP defaults to UTF-16; a server may
/// answer UTF-8 (rust-analyzer, clangd). Stored per connection; PR1 only implements UTF-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf16,
    Utf8,
}

/// `TextDocumentSyncKind`: how the server wants document changes. Stored on the caps; PR1
/// always sends full text (`didChange` with no range) regardless — incremental is a later PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncKind {
    None,
    #[default]
    Full,
    Incremental,
}

/// The capability a feature request needs — one per issuable request method. Used to gate a
/// request against the server's advertised `ServerCapabilities` (a request the server can't
/// serve is dropped silently rather than eliciting `-32601` noise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Hover,
    Definition,
    TypeDefinition,
    Implementation,
    References,
    DocumentSymbol,
    Completion,
    Rename,
    Formatting,
    SignatureHelp,
    DocumentHighlight,
}

/// An occurrence of the symbol under the cursor, in (line, UTF-16 char) coordinates.
/// `kind`: 1 Text, 2 Read, 3 Write (per LSP `DocumentHighlightKind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentHighlight {
    pub line: u32,
    pub start_char16: u32,
    pub end_line: u32,
    pub end_char16: u32,
    pub kind: u8,
}

/// A resolved signature-help view: the active signature's label and the char range within it of
/// the active parameter (for highlighting). Simplified from `lsp_types::SignatureHelp` to exactly
/// what the UI renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelp {
    pub label: String,
    /// `(start, end)` char offsets into `label` of the active parameter, if resolvable.
    pub active_param: Option<(usize, usize)>,
}

/// The subset of `ServerCapabilities` Lumina currently gates on. Grows as features land
/// (YAGNI): today only the requests the client actually issues are represented.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerCaps {
    /// `None` means the server did not answer → UTF-16 default (§2.2).
    pub position_encoding: Option<PositionEncoding>,
    pub sync_kind: SyncKind,
    pub hover: bool,
    pub definition: bool,
    pub type_definition: bool,
    pub implementation: bool,
    pub references: bool,
    pub document_symbol: bool,
    pub completion: bool,
    pub rename: bool,
    pub formatting: bool,
    pub signature_help: bool,
    pub document_highlight: bool,
}

impl ServerCaps {
    /// Whether the server advertised support for the feature behind `cap`.
    pub fn allows(&self, cap: Cap) -> bool {
        match cap {
            Cap::Hover => self.hover,
            Cap::Definition => self.definition,
            Cap::TypeDefinition => self.type_definition,
            Cap::Implementation => self.implementation,
            Cap::References => self.references,
            Cap::DocumentSymbol => self.document_symbol,
            Cap::Completion => self.completion,
            Cap::Rename => self.rename,
            Cap::Formatting => self.formatting,
            Cap::SignatureHelp => self.signature_help,
            Cap::DocumentHighlight => self.document_highlight,
        }
    }
}
