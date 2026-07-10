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
    /// The bottom terminal panel — keystrokes are forwarded to the active shell.
    Panel,
}

/// A modal overlay drawn on top of the body, capturing input while active. The same
/// machinery backs the find widget (Phase 6) and command palette (Phase 7).
#[derive(Debug, Clone)]
pub enum Overlay {
    /// Closing a dirty tab: save / discard / cancel.
    ConfirmClose { tab: usize },
    /// A dismissable information popup (e.g. LSP hover).
    Info(String),
    /// Rename prompt for the symbol at `(line, character)` in `path` (LSP rename).
    RenameInput {
        path: PathBuf,
        language: String,
        line: u32,
        character: u32,
        buffer: String,
    },
    /// Save As prompt: type a path for the active document (plan §1.5).
    SaveAsInput { buffer: String },
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
    /// LSP diagnostics per document (from the language server).
    pub diagnostics: HashMap<DocId, Vec<editor_lsp::Diagnostic>>,
    /// Precomputed bracket-match highlight for the active doc: `(bracket, partner)` char
    /// offsets, refreshed after cursor moves so the pure renderer just reads it (plan §1.3).
    pub bracket_match: Option<(usize, usize)>,
    /// Active caret-anchored completion popup, if any (plan §2.1).
    pub completion: Option<crate::completion::CompletionState>,
    /// Locations backing the current `Locations` picker (references / symbols, plan §2.3).
    pub nav_locations: Vec<editor_lsp::Location>,
    /// Per-document git change map for the gutter (plan §4.1), computed off-thread.
    pub git_hunks: HashMap<DocId, crate::git::LineStatuses>,
    /// The optional Vim modal-editing layer. `Some` when `vim = true` (or the user
    /// toggled it on); the renderer reads its mode for the status badge.
    pub vim: Option<crate::vim::VimState>,
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
            diagnostics: HashMap::new(),
            bracket_match: None,
            completion: None,
            nav_locations: Vec::new(),
            git_hunks: HashMap::new(),
            vim: None,
        }
    }

    /// Recompute the bracket-match highlight for the primary caret of the active document.
    /// Highlights the bracket the caret is on, or (failing that) the one just before it, plus
    /// its partner. Cheap — a single bracket scan; run once per frame before draw so the
    /// renderer stays a pure function of state (invariant #2).
    pub fn update_bracket_match(&mut self) {
        self.bracket_match = self.active_document().and_then(|doc| {
            let head = doc.selections.primary().head;
            let at = |p: usize| editor_core::motion::matching_bracket(doc, p).map(|q| (p, q));
            at(head).or_else(|| head.checked_sub(1).and_then(at))
        });
    }

    /// Refresh the active document's syntax highlighting for the visible line range.
    /// Cheap when nothing changed (the highlighter caches by revision + range).
    pub fn update_highlights(&mut self, viewport_height: usize) {
        let Some(id) = self.workspace.active_doc() else {
            return;
        };
        // Take the parse inputs (and drain the buffered edits) while we hold the doc, so the
        // highlighter borrow below doesn't alias the workspace.
        let Some((lang, rev, first, last, rope, edits, edits_valid)) =
            self.workspace.documents.get_mut(id).and_then(|doc| {
                let lang = doc.language.clone()?;
                if !editor_syntax::is_supported(&lang) {
                    return None;
                }
                let first = doc.view.scroll_line;
                let last = (first + viewport_height).min(doc.len_lines().saturating_sub(1));
                let edits = std::mem::take(&mut doc.syntax_edits);
                let edits_valid = std::mem::replace(&mut doc.syntax_edits_valid, true);
                Some((
                    lang,
                    doc.revision,
                    first,
                    last,
                    doc.text.clone(),
                    edits,
                    edits_valid,
                ))
            })
        else {
            return;
        };

        let hl = self.highlighters.entry(id).or_insert_with(|| {
            editor_syntax::DocHighlighter::new(&lang).expect("language checked as supported")
        });
        hl.ensure(&rope, rev, &edits, edits_valid, first, last);
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
