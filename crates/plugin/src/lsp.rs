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
    /// Signature help at the primary cursor (parameter hints while typing a call).
    SignatureHelp,
    /// Occurrences of the symbol at the primary cursor (read/write highlights).
    DocumentHighlight,
    /// Search workspace symbols by the given query (server-side matching).
    WorkspaceSymbols(String),
    /// Code actions (quickfix / refactor / source) for the selection or cursor.
    CodeAction,
    /// Resolve an accepted completion item to fetch its lazy `additionalTextEdits` (auto-imports).
    ResolveCompletion {
        label: String,
        data: serde_json::Value,
    },
    /// Run a command (server `workspace/executeCommand`, or a client-command shim like
    /// `editor.action.triggerSuggest`).
    ExecuteCommand {
        command: String,
        arguments: serde_json::Value,
    },
}

/// A code action offered to the user: a title plus the edit to apply on selection — the primitive
/// twin of `editor_lsp::CodeAction`. Command-only actions are not modeled yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCodeAction {
    pub title: String,
    /// The edit to apply (possibly empty for a command-only action).
    pub edit: LspWorkspaceEdit,
    /// A command to run after the edit (command id + arguments), if any.
    pub command: Option<(String, serde_json::Value)>,
}

/// An occurrence of the symbol under the cursor, in (line, UTF-16 char) coordinates. `kind`:
/// 1 Text, 2 Read, 3 Write — the primitive twin of `editor_lsp::DocumentHighlight`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspHighlight {
    pub line: u32,
    pub start_char16: u32,
    pub end_line: u32,
    pub end_char16: u32,
    pub kind: u8,
}

/// One semantic token (§7.1) — an absolute range in (line, UTF-16 char) coordinates plus its
/// resolved type/modifier names. The primitive twin of `editor_lsp::SemanticToken`; the plugin
/// resolves the UTF-16 columns to char offsets via [`crate::Host::lsp_pos_to_offset`] and maps the
/// `token_type`/`modifiers` to a theme scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspSemanticToken {
    pub line: u32,
    pub start_char16: u32,
    pub length: u32,
    pub token_type: String,
    pub modifiers: Vec<String>,
}

/// One inlay hint (§7.2) — the primitive twin of `editor_lsp::InlayHint`. Virtual text at a
/// (line, UTF-16 char) position; the plugin resolves the column to a char offset and publishes it
/// as an inline decoration. `kind`: 1 Type, 2 Parameter, 0 unspecified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspInlayHint {
    pub line: u32,
    pub char16: u32,
    pub label: String,
    pub kind: u8,
    pub pad_left: bool,
    pub pad_right: bool,
}

/// One resolved code lens (§6.4) — the primitive twin of a title-bearing `editor_lsp::CodeLens`.
/// Rendered as inline virtual text; only lenses with a resolved `title` reach the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCodeLens {
    pub line: u32,
    pub char16: u32,
    pub title: String,
}

/// One foldable region (§7.3) — the primitive twin of `editor_lsp::FoldingRange`. `kind` is
/// `comment`/`imports`/`region` or `None`. Line-only (fold char columns are ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspFoldingRange {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: Option<String>,
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
    /// Extra edits applied atomically on accept (auto-imports); resolved uris are app-side.
    pub additional_edits: Vec<LspTextEdit>,
    /// `insert_text` is a snippet (tab-stops / placeholders) rather than literal text.
    pub is_snippet: bool,
    /// Opaque payload for `completionItem/resolve` (to fetch late additional_edits on accept).
    pub data: Option<serde_json::Value>,
    /// A command to run after inserting (command id + arguments), via the shim.
    pub command: Option<(String, serde_json::Value)>,
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
    /// The producing tool (e.g. `rustc`), shown as a prefix in the diagnostic display.
    pub source: Option<String>,
    /// The diagnostic code (e.g. `E0425`), shown as a suffix.
    pub code: Option<String>,
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
