//! The imperative host API: what a plugin may *do*. Plugins edit only through
//! [`editor_core::Transaction`]s (never raw rope access) so undo, multi-cursor, and
//! external-sync invariants hold from the plugin side too (plan §9).

use std::path::{Path, PathBuf};

use editor_core::{DocId, Selections, Transaction, Workspace};

use crate::decoration::DecorationSet;

/// A styled run of text within a panel line. `style` is a semantic key the theme maps to
/// colors (e.g. "dir", "file", "match", "dim").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: String,
}

impl Span {
    pub fn new(text: impl Into<String>, style: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            style: style.into(),
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Span::new(text, "text")
    }
}

/// One rendered row of a panel, with an optional opaque payload the plugin gets back on
/// click (e.g. a path, or a "file:line" locator).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelLine {
    pub spans: Vec<Span>,
    pub payload: Option<String>,
    /// Indent depth (for tree panels).
    pub depth: usize,
}

impl PanelLine {
    pub fn new(spans: Vec<Span>) -> Self {
        PanelLine {
            spans,
            payload: None,
            depth: 0,
        }
    }

    pub fn payload(mut self, p: impl Into<String>) -> Self {
        self.payload = Some(p.into());
        self
    }

    pub fn depth(mut self, d: usize) -> Self {
        self.depth = d;
        self
    }
}

/// The full content of a panel: a title and a list of rows, plus a selected row index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelContent {
    pub lines: Vec<PanelLine>,
    pub selected: usize,
}

/// A directory entry surfaced to plugins (explorer/quick-open) so they don't touch `std::fs`
/// unless granted the capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// The imperative surface a plugin drives. Implemented by the app's editor state; the same
/// object is passed to native and (via a marshaling shim) external plugins.
pub trait Host {
    /// Read-only access to open documents, tabs, and the project root.
    fn workspace(&self) -> &Workspace;

    /// The active document, if any.
    fn active_doc(&self) -> Option<DocId> {
        self.workspace().active_doc()
    }

    /// The project root.
    fn root(&self) -> &Path {
        &self.workspace().root
    }

    /// Apply an edit as a transaction (records undo, updates multi-cursor).
    fn apply_transaction(&mut self, doc: DocId, txn: Transaction);

    /// Replace a document's selection set.
    fn set_selections(&mut self, doc: DocId, selections: Selections);

    /// Open (or focus) a file in a tab.
    fn open_path(&mut self, path: &Path);

    /// List a directory, honoring ignore rules. Capability-gated for external plugins.
    fn read_dir(&self, path: &Path) -> Vec<DirEntry>;

    /// The 0-based line numbers of `doc` that have an uncommitted git change (added / modified /
    /// or a deletion marker), sorted ascending. Empty when the git gutter is off, the file is
    /// clean/untracked, or the host doesn't track git — the default returns none, so only a host
    /// that computes a change map (the app) overrides it. Read-only: it drives change navigation
    /// (the `git.*` builtin), not mutation.
    fn changed_lines(&self, _doc: DocId) -> Vec<usize> {
        Vec::new()
    }

    /// Publish `doc`'s decorations for a named `layer` (e.g. `"find.match"`, `"lsp.diag"`) — the
    /// styled char spans + gutter marks the pure renderer paints. Replaces the whole set for that
    /// layer; `clear_decorations` drops it. Default no-ops, so a host that doesn't render (tests,
    /// external guests) need not implement them; the app overrides both.
    fn set_decorations(&mut self, _doc: DocId, _layer: &str, _decos: DecorationSet) {}

    /// Drop a previously-published decoration `layer` for `doc`.
    fn clear_decorations(&mut self, _doc: DocId, _layer: &str) {}

    /// Set a panel's rendered content.
    fn set_panel(&mut self, panel_id: &str, content: PanelContent);

    /// Update a status-bar item's text.
    fn set_status(&mut self, item_id: &str, text: String);

    /// Show a transient notification to the user.
    fn notify(&mut self, message: String);

    /// Execute another registered command by id (composability).
    fn execute(&mut self, command_id: &str);
}
