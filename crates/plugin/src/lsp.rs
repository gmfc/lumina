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
}
