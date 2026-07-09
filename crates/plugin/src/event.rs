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
}
