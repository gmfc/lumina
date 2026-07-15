//! `EditorState` — the app-side state plugins mutate through `Host`, and the app renders.
//!
//! Kept separate from `App` (terminal + registry) so we can split-borrow: `App` dispatches
//! `registry.dispatch_command(id, &mut self.editor)` without aliasing.

use std::collections::HashMap;
use std::path::PathBuf;

use editor_core::{DocId, Document, Workspace};
use editor_plugin::event::Event;
use editor_plugin::{CommandInfo, DecorationSet, PanelContent, Popup, Prompt};

mod host;

/// Which region has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Editor,
    Sidebar,
    /// The bottom terminal panel — keystrokes are forwarded to the active shell.
    Panel,
    /// The bottom LSP panel — keys scroll the status/log view.
    LspPanel,
}

/// Which tab the shared bottom dock is displaying. The terminal and LSP tabs coexist in one dock
/// region with a tab strip; `dock_active` picks which is shown (display clamps it to an open tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockTab {
    Terminal,
    Lsp,
}

/// App-owned UI state for the LSP dock tab (the terminal tab's lifecycle is plugin-owned).
#[derive(Default)]
pub(crate) struct LspPanelUi {
    pub(crate) minimized: bool,
    /// First visible row of the (scrollable) status list.
    pub(crate) scroll: u16,
}

/// One entry in a right-click context menu: a labelled action that runs `command`. `first_in_group`
/// marks the first item of a group so the renderer draws a divider above it.
#[derive(Debug, Clone)]
pub(crate) struct ContextMenuItem {
    pub(crate) label: String,
    pub(crate) command: String,
    pub(crate) first_in_group: bool,
}

/// A modal overlay drawn on top of the body, capturing input while active. The generic prompt +
/// picker ports (which the find/palette plugins drive) replaced most bespoke overlays; only
/// confirm-close, the LSP hover info box, save-as, and the right-click menu remain here.
#[derive(Debug, Clone)]
pub(crate) enum Overlay {
    /// Closing a dirty tab: save / discard / cancel.
    ConfirmClose { tab: usize },
    /// A dismissable information popup (e.g. LSP hover).
    Info(String),
    /// Save As prompt: type a path for the active document (plan §1.5).
    SaveAsInput { buffer: String },
    /// The right-click context menu, anchored at screen `(x, y)`; `selected` is the highlighted row.
    ContextMenu {
        x: u16,
        y: u16,
        items: Vec<ContextMenuItem>,
        selected: usize,
    },
}

/// Everything rendered + mutated by plugins.
pub(crate) struct EditorState {
    pub(crate) workspace: Workspace,
    pub(crate) sidebar_width: u16,
    pub(crate) sidebar_visible: bool,
    pub(crate) focus: Focus,
    pub(crate) status_message: Option<String>,
    /// Rendered panel content, keyed by panel id (set by plugins).
    pub(crate) panels: HashMap<String, PanelContent>,
    /// Status-bar item text, keyed by item id.
    pub(crate) status_items: HashMap<String, String>,
    /// Events queued during a dispatch, drained + broadcast by `App`.
    pub(crate) pending_events: Vec<Event>,
    /// Command ids queued via `Host::execute`, run by `App` after the current dispatch.
    pub(crate) pending_commands: Vec<String>,
    /// Paths requested via `Host::open_path`/`open_path_at`, opened by `App` (it owns file IO
    /// policy) on the next drain. The optional line positions the caret (project search / goto).
    pub(crate) pending_opens: Vec<(PathBuf, Option<usize>)>,
    /// LSP navigation jumps requested via `Host::open_location`: `(path, line, utf16_char)`. The
    /// app opens the path and resolves the UTF-16 column to a char offset on the next drain.
    pub(crate) pending_locations: Vec<(PathBuf, u32, u32)>,
    /// Multi-file rename edits requested via `Host::apply_workspace_edit`; the app opens each file
    /// and applies the edits as history-recorded transactions on the next drain (effect-queue).
    pub(crate) pending_workspace_edits: Vec<editor_plugin::LspWorkspaceEdit>,
    /// Sender to the app's bounded worker channel, so `Host::spawn_job` can run plugin work off
    /// the main thread and fold the result back as `Event::JobComplete`. Set at construction.
    pub(crate) job_tx: Option<crate::worker::WorkerTx>,
    /// Set by `Host::toggle_theme`; the app flips its (app-owned) theme on the next drain.
    pub(crate) pending_theme_toggle: bool,
    /// LSP requests queued via `Host::lsp_request`; the app resolves the cursor position and
    /// forwards them to the (app-owned) `LspManager` on the next drain (effect-queue idiom).
    pub(crate) pending_lsp_requests: Vec<editor_plugin::LspRequestKind>,
    /// Mirror of `App.lsp.is_enabled()` so `Host::lsp_enabled` can answer across the split-borrow
    /// wall. Set at construction / config reload (the server set is config-stable per session).
    pub(crate) lsp_enabled: bool,
    /// The app-owned PTY sessions, keyed by the `TerminalId` the `terminal` plugin allocated via
    /// `Host::terminal_open`. The plugin owns the dock lifecycle (which ids, order, active); this
    /// map owns the concrete vt100/pty state the app feeds, resizes, and renders.
    pub(crate) terminals: HashMap<editor_plugin::TerminalId, crate::terminal::Terminal>,
    /// Monotonic allocator for `TerminalId`s.
    pub(crate) next_terminal_id: u64,
    /// The dock lifecycle the `terminal` plugin publishes via `Host::set_terminal_view`; the pure
    /// renderer + key/mouse routing read it (invariant #8).
    pub(crate) terminal_view: editor_plugin::TerminalView,
    /// Resolved default shell for new terminals (from `config.terminal_shell`), so the Host can
    /// spawn without reaching `App.config`. Set at construction / config reload.
    pub(crate) terminal_shell: String,
    /// Dock content height (rows) when expanded, from `config.terminal_height`. App-owned render
    /// param (not part of the plugin's lifecycle), fed to the layout.
    pub(crate) terminal_height: u16,
    /// Whether the LSP dock tab is open. The dock is visible when this or `terminal_view.open` is
    /// set; the two tabs share the one bottom region.
    pub(crate) lsp_open: bool,
    /// Which dock tab is displayed (clamped to an open tab by `App::dock_active_tab`).
    pub(crate) dock_active: DockTab,
    /// App-owned UI state for the LSP dock tab.
    pub(crate) lsp_panel: LspPanelUi,
    /// Active modal overlay, if any.
    pub(crate) overlay: Option<Overlay>,
    /// Per-document syntax highlighters (created lazily for supported languages).
    pub(crate) highlighters: HashMap<DocId, editor_syntax::DocHighlighter>,
    /// Per-document, per-layer decorations (styled spans + gutter marks) published by plugins
    /// via `Host::set_decorations`. The renderer merges these layers on top of syntax; keeping
    /// them here (not on the plugin) keeps render a pure function of state (invariant #8).
    pub(crate) decorations: HashMap<DocId, HashMap<String, DecorationSet>>,
    /// The active modal input prompt (find/replace today), owned by a plugin and rendered
    /// generically. `Some` while a prompt is up; the app routes keys to its owner.
    pub(crate) prompt: Option<Prompt>,
    /// Active fuzzy picker (command palette / quick open / goto line), if open.
    pub(crate) picker: Option<crate::picker::Picker>,
    /// Snapshot of every command (built-in + contributed) a palette plugin can enumerate through
    /// `Host::commands`, taken after plugins register. Mirrors the registry across the split-borrow
    /// wall (the palette plugin sees only `&mut EditorState`).
    pub(crate) command_catalog: Vec<CommandInfo>,
    /// Precomputed bracket-match highlight for the active doc: `(bracket, partner)` char
    /// offsets, refreshed after cursor moves so the pure renderer just reads it (plan §1.3).
    pub(crate) bracket_match: Option<(usize, usize)>,
    /// Active caret-anchored popup (the completion list), published by a plugin and rendered
    /// generically. `Some` while a popup is up; the app routes nav keys to its owner.
    pub(crate) popup: Option<Popup>,
    /// Per-document git change map for the gutter (plan §4.1), computed off-thread.
    pub(crate) git_hunks: HashMap<DocId, crate::git::LineStatuses>,
    /// The Vim render mirror published by the `vim` plugin via `Host::set_vim_view`: the mode (for
    /// the status badge + visual-selection shading) and any pending-command hint. `Some` while the
    /// vim layer is enabled. The whole modal state machine lives in the plugin.
    pub(crate) vim_view: Option<editor_plugin::VimView>,
    /// The editor's visible height in rows, mirrored from the layout each frame so the `vim` plugin
    /// can read it through `Host::viewport_height` for page motions / recentering.
    pub(crate) page_height: usize,
    /// The system clipboard (arboard + OSC 52 + an in-process register). App-owned I/O, kept here
    /// so the `clipboard` plugin can reach it through `Host::clipboard_read`/`clipboard_write`
    /// across the split-borrow wall (the plugin only sees `&mut EditorState`).
    pub(crate) clipboard: crate::clipboard::Clipboard,
}

impl EditorState {
    pub(crate) fn new(root: PathBuf) -> EditorState {
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
            pending_locations: Vec::new(),
            pending_workspace_edits: Vec::new(),
            job_tx: None,
            pending_theme_toggle: false,
            pending_lsp_requests: Vec::new(),
            lsp_enabled: false,
            terminals: HashMap::new(),
            next_terminal_id: 1,
            terminal_view: editor_plugin::TerminalView::default(),
            terminal_shell: crate::terminal::default_shell(None),
            terminal_height: 12,
            lsp_open: false,
            dock_active: DockTab::Terminal,
            lsp_panel: LspPanelUi::default(),
            overlay: None,
            highlighters: HashMap::new(),
            decorations: HashMap::new(),
            prompt: None,
            picker: None,
            command_catalog: Vec::new(),
            bracket_match: None,
            popup: None,
            git_hunks: HashMap::new(),
            vim_view: None,
            page_height: 20,
            clipboard: crate::clipboard::Clipboard::new(),
        }
    }

    /// Recompute the bracket-match highlight for the primary caret of the active document.
    /// Highlights the bracket the caret is on, or (failing that) the one just before it, plus
    /// its partner. Cheap — a single bracket scan; run once per frame before draw so the
    /// renderer stays a pure function of state (invariant #2).
    pub(crate) fn update_bracket_match(&mut self) {
        self.bracket_match = self.active_document().and_then(|doc| {
            let head = doc.selections.primary().head;
            let at = |p: usize| editor_core::motion::matching_bracket(doc, p).map(|q| (p, q));
            at(head).or_else(|| head.checked_sub(1).and_then(at))
        });
    }

    /// Refresh the active document's syntax highlighting for the visible line range.
    /// Cheap when nothing changed (the highlighter caches by revision + range).
    pub(crate) fn update_highlights(&mut self, viewport_height: usize) {
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

    pub(crate) fn active_document(&self) -> Option<&Document> {
        self.workspace.active_document()
    }

    pub(crate) fn active_document_mut(&mut self) -> Option<&mut Document> {
        self.workspace.active_document_mut()
    }

    /// Queue an event for `App` to broadcast to plugins.
    pub(crate) fn emit(&mut self, event: Event) {
        self.pending_events.push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_plugin::{Decoration, DecorationSet, Host};

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
