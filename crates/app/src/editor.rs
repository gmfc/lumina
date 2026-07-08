//! `EditorState` — the app-side state plugins mutate through `Host`, and the app renders.
//!
//! Kept separate from `App` (terminal + registry) so we can split-borrow: `App` dispatches
//! `registry.dispatch_command(id, &mut self.editor)` without aliasing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use editor_core::{DocId, Document, Selections, Transaction, Workspace};
use editor_plugin::event::Event;
use editor_plugin::host::DirEntry;
use editor_plugin::{Host, PanelContent};

/// Which region has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Sidebar,
}

/// A modal overlay drawn on top of the body, capturing input while active. The same
/// machinery backs the find widget (Phase 6) and command palette (Phase 7).
#[derive(Debug, Clone)]
pub enum Overlay {
    /// Closing a dirty tab: save / discard / cancel.
    ConfirmClose { tab: usize },
}

/// Everything rendered + mutated by plugins.
pub struct EditorState {
    pub workspace: Workspace,
    pub sidebar_width: u16,
    pub sidebar_visible: bool,
    pub focus: Focus,
    pub status_message: Option<String>,
    /// Rendered panel content, keyed by panel id (set by plugins).
    pub panels: HashMap<String, PanelContent>,
    /// Status-bar item text, keyed by item id.
    pub status_items: HashMap<String, String>,
    /// Events queued during a dispatch, drained + broadcast by `App`.
    pub pending_events: Vec<Event>,
    /// Command ids queued via `Host::execute`, run by `App` after the current dispatch.
    pub pending_commands: Vec<String>,
    /// Paths requested via `Host::open_path`, opened by `App` (it owns file IO policy).
    pub pending_opens: Vec<PathBuf>,
    /// Active modal overlay, if any.
    pub overlay: Option<Overlay>,
    /// Per-document syntax highlighters (created lazily for supported languages).
    pub highlighters: HashMap<DocId, editor_syntax::DocHighlighter>,
    /// Active in-file find/replace widget, if open.
    pub find: Option<crate::find::FindState>,
    /// Active fuzzy picker (command palette / quick open / goto line), if open.
    pub picker: Option<crate::picker::Picker>,
}

impl EditorState {
    pub fn new(root: PathBuf) -> EditorState {
        EditorState {
            workspace: Workspace::new(root),
            sidebar_width: 30,
            sidebar_visible: true,
            focus: Focus::Editor,
            status_message: None,
            panels: HashMap::new(),
            status_items: HashMap::new(),
            pending_events: Vec::new(),
            pending_commands: Vec::new(),
            pending_opens: Vec::new(),
            overlay: None,
            highlighters: HashMap::new(),
            find: None,
            picker: None,
        }
    }

    /// Refresh the active document's syntax highlighting for the visible line range.
    /// Cheap when nothing changed (the highlighter caches by revision + range).
    pub fn update_highlights(&mut self, viewport_height: usize) {
        let Some(id) = self.workspace.active_doc() else {
            return;
        };
        let Some(doc) = self.workspace.documents.get(id) else {
            return;
        };
        let Some(lang) = doc.language.clone() else {
            return;
        };
        if !editor_syntax::is_supported(&lang) {
            return;
        }
        let rev = doc.revision;
        let first = doc.view.scroll_line;
        let last = (first + viewport_height).min(doc.len_lines().saturating_sub(1));
        let rope = doc.text.clone(); // O(1): ropey is copy-on-write

        let entry = self.highlighters.entry(id);
        let hl = entry.or_insert_with(|| {
            editor_syntax::DocHighlighter::new(&lang).expect("language checked as supported")
        });
        hl.ensure(&rope, rev, first, last);
    }

    pub fn active_document(&self) -> Option<&Document> {
        self.workspace.active_document()
    }

    pub fn active_document_mut(&mut self) -> Option<&mut Document> {
        self.workspace.active_document_mut()
    }

    /// Queue an event for `App` to broadcast to plugins.
    pub fn emit(&mut self, event: Event) {
        self.pending_events.push(event);
    }
}

impl Host for EditorState {
    fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    fn apply_transaction(&mut self, doc: DocId, txn: Transaction) {
        if let Some(d) = self.workspace.documents.get_mut(doc) {
            let before = d.selections.clone();
            let inverse = txn.apply(d);
            d.dirty = true;
            d.history.record(
                txn,
                inverse,
                before,
                d.selections.clone(),
                editor_core::GroupBreak::Force,
            );
        }
        self.emit(Event::DidChange(doc));
    }

    fn set_selections(&mut self, doc: DocId, selections: Selections) {
        if let Some(d) = self.workspace.documents.get_mut(doc) {
            d.selections = selections;
        }
        self.emit(Event::DidChangeCursor(doc));
    }

    fn open_path(&mut self, path: &Path) {
        self.pending_opens.push(path.to_path_buf());
    }

    fn read_dir(&self, path: &Path) -> Vec<DirEntry> {
        // Honor ignore rules; hidden by default off is handled by the explorer plugin.
        let mut entries = Vec::new();
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                let p = e.path();
                let is_dir = p.is_dir();
                entries.push(DirEntry { path: p, is_dir });
            }
        }
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
        });
        entries
    }

    fn set_panel(&mut self, panel_id: &str, content: PanelContent) {
        self.panels.insert(panel_id.to_string(), content);
    }

    fn set_status(&mut self, item_id: &str, text: String) {
        self.status_items.insert(item_id.to_string(), text);
    }

    fn notify(&mut self, message: String) {
        self.status_message = Some(message);
    }

    fn execute(&mut self, command_id: &str) {
        self.pending_commands.push(command_id.to_string());
    }
}
