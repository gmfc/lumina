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
        /// `Some(message)` when the server replied with a JSON-RPC `error` object instead of a
        /// result. Kept distinct from a `null` result so a failed request (rename, goto, …) can
        /// be surfaced to the user rather than silently degrading to "no result".
        error: Option<String>,
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
}

/// A batch of diagnostics for one document URI (as sent by `publishDiagnostics`).
#[derive(Debug, Clone)]
pub struct DiagnosticsUpdate {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
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
        }
    }
}
