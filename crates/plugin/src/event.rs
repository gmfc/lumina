//! The event bus. Plugins are mostly reactive; the dispatcher fans state changes out to
//! subscribers here (plan §6A). Events carry the affected [`DocId`].

use editor_core::DocId;

/// A notification broadcast to plugins after the corresponding state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A document was opened in a tab.
    DidOpen(DocId),
    /// A document's text changed (edit applied).
    DidChange(DocId),
    /// A document was saved to disk.
    DidSave(DocId),
    /// The active document's selection set changed.
    DidChangeCursor(DocId),
    /// A clean buffer was reloaded from an external on-disk change (plan §6).
    ExternalReload(DocId),
    /// Configuration was reloaded.
    DidChangeConfig,
    /// The active tab changed.
    DidChangeActive(Option<DocId>),
    /// A background job (spawned via [`crate::Host::spawn_job`]) finished. `id` is the
    /// plugin's correlation id (e.g. carrying a generation so stale results drop); `payload`
    /// is the job's serialized result, decoded by the owning plugin.
    JobComplete { id: String, payload: Vec<u8> },
    /// The language server published diagnostics for `doc` (translated from `editor-lsp` at the
    /// app boundary into primitive [`crate::LspDiagnostic`]s). `None` doc = an update for a URI
    /// with no open document.
    LspDiagnostics {
        doc: Option<DocId>,
        diagnostics: Vec<crate::lsp::LspDiagnostic>,
    },
    /// The language server answered a completion request with these items (translated from
    /// `editor-lsp` at the app boundary). Delivered to the completion plugin, which anchors and
    /// filters them into a caret popup.
    LspCompletion {
        items: Vec<crate::lsp::LspCompletionItem>,
        /// The server truncated the list — re-request as the user types instead of filtering
        /// locally.
        is_incomplete: bool,
    },
    /// The language server resolved a single navigation target (go-to-definition / implementation /
    /// type-definition). Delivered to the navigation plugin, which jumps there via
    /// [`crate::Host::open_location`].
    LspGoto(crate::lsp::LspLocation),
    /// The language server returned a set of navigation targets to choose from (references /
    /// document symbols). `title` names the picker; each item carries its jump target + label.
    LspLocations {
        title: String,
        items: Vec<crate::lsp::LspNavItem>,
    },
    /// The language server answered a hover request with this (already-rendered) text. Delivered
    /// to the hover plugin, which shows it in a dismissable info box.
    LspHover(String),
    /// Signature help while typing a call: the active signature line with its active parameter
    /// marked, or `None` to clear it. Delivered to the signature-help plugin (statusline).
    LspSignatureHelp(Option<String>),
    /// Occurrences of the symbol under the cursor. Delivered to the document-highlight plugin,
    /// which paints them as a decoration layer. Empty clears the highlights.
    LspHighlights(Vec<crate::lsp::LspHighlight>),
    /// Full-document semantic tokens for `doc` (§7.1), delivered to the semantic-tokens plugin,
    /// which paints them as a decoration layer over tree-sitter. Empty clears the layer. `None`
    /// doc = a response for a URI with no open document (dropped).
    LspSemanticTokens {
        doc: Option<DocId>,
        tokens: Vec<crate::lsp::LspSemanticToken>,
    },
    /// Inlay hints for `doc` (§7.2), delivered to the inlay-hints plugin, which paints them as
    /// inline virtual text. Empty clears the layer. `None` doc = a response for a URI with no open
    /// document (dropped).
    LspInlayHints {
        doc: Option<DocId>,
        hints: Vec<crate::lsp::LspInlayHint>,
    },
    /// Resolved code lenses for `doc` (§6.4), delivered to the code-lens plugin, which paints them
    /// as inline virtual text. Empty clears the layer.
    LspCodeLenses {
        doc: Option<DocId>,
        lenses: Vec<crate::lsp::LspCodeLens>,
    },
    /// Foldable regions for `doc` (§7.3), delivered to the folding plugin, which marks each fold
    /// start in the gutter. Empty clears the markers.
    LspFoldingRanges {
        doc: Option<DocId>,
        ranges: Vec<crate::lsp::LspFoldingRange>,
    },
    /// Code actions offered for the cursor/selection. Delivered to the code-action plugin, which
    /// shows them in a picker and applies the chosen one. Empty means none were offered.
    LspCodeActions(Vec<crate::lsp::LspCodeAction>),
    /// The language server computed a rename's edits (a `WorkspaceEdit`, translated to primitive
    /// paths app-side). Delivered to the rename plugin, which applies them via
    /// [`crate::Host::apply_workspace_edit`].
    LspWorkspaceEdit(crate::lsp::LspWorkspaceEdit),
}
