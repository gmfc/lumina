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

/// A message from a server to the client: either a diagnostics notification or a response to
/// one of our requests (correlated by `id`).
#[derive(Debug, Clone)]
pub enum Incoming {
    Diagnostics(DiagnosticsUpdate),
    Response {
        id: i64,
        result: serde_json::Value,
        /// `Some(message)` when the server replied with a JSON-RPC `error` object instead of a
        /// result. Kept distinct from a `null` result so a failed request (rename, goto, …) can
        /// be surfaced to the user rather than silently degrading to "no result".
        error: Option<String>,
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
