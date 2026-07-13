//! Primitive LSP request intent — the crossterm/lsp-types-free way a plugin asks the editor to
//! issue a language-server request. The kernel (and `editor-builtins`) must not depend on
//! `editor-lsp`/`lsp-types`, so a plugin expresses only the *intent*; the app owns the transport,
//! the UTF-16 cursor math, and the response handling (see [`crate::Host::lsp_request`]).

/// What to ask the language server for, at the active document's primary cursor (except
/// `DocumentSymbols`, which is whole-file). `Rename` carries the new identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspRequestKind {
    Hover,
    Definition,
    Implementation,
    TypeDefinition,
    Completion,
    References,
    DocumentSymbols,
    Rename(String),
    /// Whole-document formatting; the app applies the returned edits to the active document.
    Formatting,
}

/// Diagnostic severity — the primitive twin of `editor_lsp::Severity`, so a plugin can own
/// diagnostics without depending on `editor-lsp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

/// A completion candidate — the primitive twin of `editor_lsp::CompletionItem`, so the completion
/// plugin owns the item list without depending on `editor-lsp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    /// LSP `CompletionItemKind` (1..=25), if the server sent one — drives the kind glyph.
    pub kind: Option<u8>,
}

/// A diagnostic in (line, UTF-16 char) coordinates — the primitive twin of `editor_lsp::Diagnostic`.
/// The app converts `editor-lsp` diagnostics into these at the poll boundary and hands them to
/// plugins as [`crate::Event::LspDiagnostics`]; the plugin resolves the UTF-16 columns to char
/// offsets through [`crate::Host::lsp_pos_to_offset`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub line: u32,
    pub start_char16: u32,
    pub end_line: u32,
    pub end_char16: u32,
    pub severity: LspSeverity,
    pub message: String,
}

/// A resolved navigation target — the primitive twin of `editor_lsp::Location`, with the URI
/// already resolved to a filesystem path app-side. A navigation plugin jumps here through
/// [`crate::Host::open_location`] without touching `editor-lsp` or URI parsing. `character` is a
/// UTF-16 column, resolved to a char offset app-side when the jump is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub path: String,
    pub line: u32,
    pub character: u32,
}

/// A row in a navigation picker (references / document symbols): where to jump, plus the label the
/// app already formatted for it (a `file:line:col` for references, an indented name for symbols).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspNavItem {
    pub location: LspLocation,
    pub label: String,
}

/// A single text replacement in (line, UTF-16 char) coordinates — the primitive twin of
/// `editor_lsp::TextEdit`. The app resolves the UTF-16 columns to char offsets when it applies the
/// edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspTextEdit {
    pub start_line: u32,
    pub start_char16: u32,
    pub end_line: u32,
    pub end_char16: u32,
    pub new_text: String,
}

/// A set of edits grouped by file — the primitive twin of `editor_lsp::WorkspaceEdit` (a rename
/// result). URIs are resolved to filesystem paths app-side; the plugin only forwards this to
/// [`crate::Host::apply_workspace_edit`], which opens each file and applies the edits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LspWorkspaceEdit {
    pub changes: Vec<(String, Vec<LspTextEdit>)>,
}
