//! `editor_lsp` — a minimal Language Server Protocol client (plan §10).
//!
//! You write the transport (JSON-RPC over the server's stdio); `lsp-types` provides the
//! message structs. This crate implements message framing, char↔UTF-16 position conversion
//! (LSP counts UTF-16 code units), and a client that spawns a server, runs the initialize
//! handshake, streams document changes, and surfaces `publishDiagnostics`.
//!
//! The transport + position code is deterministic and unit-tested; the spawning client is
//! integration-only (needs a real server binary), so CI never depends on one.
#![forbid(unsafe_code)]

pub mod client;
pub mod position;
pub mod transport;

pub use client::{LspClient, LspHandle};

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
