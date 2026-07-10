//! `EditorState` — the app-side state plugins mutate through `Host`, and the app renders.
//!
//! Kept separate from `App` (terminal + registry) so we can split-borrow: `App` dispatches
//! `registry.dispatch_command(id, &mut self.editor)` without aliasing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use editor_core::{DocId, Document, Selections, Transaction, Workspace};
use editor_plugin::event::Event;
use editor_plugin::host::DirEntry;
use editor_plugin::{CommandInfo, DecorationSet, Host, PanelContent, PickerRequest, Prompt};

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
    /// Paths requested via `Host::open_path`/`open_path_at`, opened by `App` (it owns file IO
    /// policy) on the next drain. The optional line positions the caret (project search / goto).
    pub pending_opens: Vec<(PathBuf, Option<usize>)>,
    /// Sender to the app's bounded worker channel, so `Host::spawn_job` can run plugin work off
    /// the main thread and fold the result back as `Event::JobComplete`. Set at construction.
    pub job_tx: Option<crate::worker::WorkerTx>,
    /// Active modal overlay, if any.
    pub overlay: Option<Overlay>,
    /// Per-document syntax highlighters (created lazily for supported languages).
    pub highlighters: HashMap<DocId, editor_syntax::DocHighlighter>,
    /// Per-document, per-layer decorations (styled spans + gutter marks) published by plugins
    /// via `Host::set_decorations`. The renderer merges these layers on top of syntax; keeping
    /// them here (not on the plugin) keeps render a pure function of state (invariant #8).
    pub decorations: HashMap<DocId, HashMap<String, DecorationSet>>,
    /// The active modal input prompt (find/replace today), owned by a plugin and rendered
    /// generically. `Some` while a prompt is up; the app routes keys to its owner.
    pub prompt: Option<Prompt>,
    /// Active fuzzy picker (command palette / quick open / goto line), if open.
    pub picker: Option<crate::picker::Picker>,
    /// Snapshot of every command (built-in + contributed) a palette plugin can enumerate through
    /// `Host::commands`, taken after plugins register. Mirrors the registry across the split-borrow
    /// wall (the palette plugin sees only `&mut EditorState`).
    pub command_catalog: Vec<CommandInfo>,
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
            job_tx: None,
            overlay: None,
            highlighters: HashMap::new(),
            decorations: HashMap::new(),
            prompt: None,
            picker: None,
            command_catalog: Vec::new(),
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
                    doc.rope().clone(),
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
            // Normalize at the port boundary: a plugin may hand us a set built with single+push,
            // and downstream edit code relies on it being sorted/non-overlapping (invariant #2).
            d.set_selections(selections);
        }
        self.emit(Event::DidChangeCursor(doc));
    }

    fn open_path(&mut self, path: &Path) {
        self.pending_opens.push((path.to_path_buf(), None));
    }

    fn open_path_at(&mut self, path: &Path, line: usize) {
        self.pending_opens.push((path.to_path_buf(), Some(line)));
    }

    fn spawn_job(&mut self, id: String, work: Box<dyn FnOnce() -> Vec<u8> + Send + 'static>) {
        // Run the plugin's closure on an OS thread and fold the result back into the single-
        // threaded loop as a WorkerMsg the drain turns into Event::JobComplete. The app owns the
        // threading + bounded channel; the plugin owns the work. No channel ⇒ silent no-op.
        let Some(tx) = self.job_tx.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let payload = work();
            let _ = tx.send(crate::worker::WorkerMsg::JobComplete { id, payload });
        });
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

    fn changed_lines(&self, doc: DocId) -> Vec<usize> {
        let mut lines: Vec<usize> = self
            .git_hunks
            .get(&doc)
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default();
        lines.sort_unstable();
        lines
    }

    fn commands(&self) -> Vec<CommandInfo> {
        self.command_catalog.clone()
    }

    fn project_files(&self) -> Vec<DirEntry> {
        // Ignore-honoring walk of the project root (files only), capped so a huge tree can't
        // stall the picker. The app owns this policy so builtins need no `ignore` dependency.
        let mut out = Vec::new();
        let walker = ignore::WalkBuilder::new(&self.workspace.root)
            .hidden(false)
            .git_ignore(true)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.flatten().take(10_000) {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                out.push(DirEntry {
                    path: entry.path().to_path_buf(),
                    is_dir: false,
                });
            }
        }
        out
    }

    fn open_picker(&mut self, request: PickerRequest) {
        let to_items = |v: Vec<editor_plugin::PickerItem>| -> Vec<crate::picker::PickerItem> {
            v.into_iter()
                .map(|i| crate::picker::PickerItem {
                    id: i.id,
                    label: i.label,
                })
                .collect()
        };
        self.picker = Some(
            crate::picker::Picker::unified(
                &request.title,
                to_items(request.items),
                to_items(request.commands),
                request.start_in_commands,
            )
            .owned_by(request.owner, request.token),
        );
    }

    fn set_prompt(&mut self, prompt: Prompt) {
        self.prompt = Some(prompt);
    }

    fn dismiss_prompt(&mut self) {
        self.prompt = None;
    }

    fn set_decorations(&mut self, doc: DocId, layer: &str, decos: DecorationSet) {
        // An empty set is a clear: don't leave a dead layer the renderer must skip every frame.
        if decos.is_empty() {
            self.clear_decorations(doc, layer);
            return;
        }
        self.decorations
            .entry(doc)
            .or_default()
            .insert(layer.to_string(), decos);
    }

    fn clear_decorations(&mut self, doc: DocId, layer: &str) {
        if let Some(layers) = self.decorations.get_mut(&doc) {
            layers.remove(layer);
            if layers.is_empty() {
                self.decorations.remove(&doc);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use editor_plugin::{Decoration, DecorationSet};

    #[test]
    fn set_and_clear_decorations() {
        let mut ed = EditorState::new(std::path::PathBuf::from("."));
        let id = ed.workspace.open_document(Document::from_str("hello"));
        let set = DecorationSet::spans(vec![Decoration::new((0, 3), "find.match")]);

        ed.set_decorations(id, "find", set.clone());
        assert_eq!(ed.decorations[&id].get("find"), Some(&set));

        // Publishing an empty set clears the layer — and the doc entry when it was the last one.
        ed.set_decorations(id, "find", DecorationSet::default());
        assert!(!ed.decorations.contains_key(&id));

        // clear_decorations removes just the named layer, keeping the rest.
        ed.set_decorations(id, "find", set.clone());
        ed.set_decorations(id, "sel", set);
        ed.clear_decorations(id, "find");
        let layers = &ed.decorations[&id];
        assert!(!layers.contains_key("find") && layers.contains_key("sel"));
    }
}
