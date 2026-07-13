//! The imperative host API: what a plugin may *do*. Plugins edit only through
//! [`editor_core::Transaction`]s (never raw rope access) so undo, multi-cursor, and
//! external-sync invariants hold from the plugin side too (plan §9).

use std::path::{Path, PathBuf};

use editor_core::{DocId, Selections, Transaction, Workspace};

use crate::decoration::DecorationSet;
use crate::overlay::{Popup, Prompt};
use crate::picker::{CommandInfo, PickerRequest};

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

    /// Open (or focus) a file and position the caret at 0-based `line`. Defaults to opening at
    /// the top (`open_path`); the app overrides it to jump (used by project search / goto).
    fn open_path_at(&mut self, path: &Path, _line: usize) {
        self.open_path(path);
    }

    /// Run `work` off the main thread and deliver its result back as [`crate::Event::JobComplete`]
    /// tagged with `id`. The plugin builds the closure (it owns any grep/fs deps); the host owns
    /// the threading + bounded channel. Default no-op, so a host without a worker loop ignores it.
    fn spawn_job(&mut self, _id: String, _work: Box<dyn FnOnce() -> Vec<u8> + Send + 'static>) {}

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

    /// Show a modal input widget the app renders and forwards keys to. The owning plugin
    /// re-publishes it as its state changes; `dismiss_prompt` closes it. Default no-ops so a
    /// host that doesn't render (tests, external guests) need not implement them.
    fn set_prompt(&mut self, _prompt: Prompt) {}

    /// Close the active prompt, if any.
    fn dismiss_prompt(&mut self) {}

    /// Publish (or clear with `None`) the caret-anchored popup — the completion list today. The
    /// app renders it and, while it's up, routes navigation keys to its owner's
    /// [`crate::Plugin::on_popup_key`]. Default no-op.
    fn set_popup(&mut self, _popup: Option<Popup>) {}

    /// Every command the palette can run (built-in + contributed), mirrored onto the host so a
    /// palette plugin can enumerate them without reaching the registry (unreachable through
    /// `Host`). Default empty; the app fills it from a snapshot taken after plugins register.
    fn commands(&self) -> Vec<CommandInfo> {
        Vec::new()
    }

    /// The project's files (ignore-honoring walk of the workspace root), for quick-open. Default
    /// empty; the app owns the `ignore`-crate walk policy so plugins need no filesystem deps.
    fn project_files(&self) -> Vec<DirEntry> {
        Vec::new()
    }

    /// Open the app's generic fuzzy picker from a data-described request. The app builds, filters,
    /// renders, and captures keys generically, then routes activation to the request's `owner`.
    /// Default no-op.
    fn open_picker(&mut self, _request: PickerRequest) {}

    /// Set a panel's rendered content.
    fn set_panel(&mut self, panel_id: &str, content: PanelContent);

    /// Update a status-bar item's text.
    fn set_status(&mut self, item_id: &str, text: String);

    /// Show a transient notification to the user.
    fn notify(&mut self, message: String);

    /// Toggle the editor's light/dark appearance. The theme is app render state, so the app
    /// applies it on the next drain (an effect-queue, like `open_path`). Default no-op.
    fn toggle_theme(&mut self) {}

    /// Fire-and-forget a language-server request at the active document's primary cursor. The app
    /// owns the transport, the UTF-16 cursor math, and the response handling; the plugin only
    /// expresses intent (queued like `open_path`). Default no-op.
    fn lsp_request(&mut self, _kind: crate::lsp::LspRequestKind) {}

    /// Whether a language server is configured (so a plugin can cleanly no-op its LSP commands
    /// when there's nothing to talk to). Default `false`.
    fn lsp_enabled(&self) -> bool {
        false
    }

    /// Convert an LSP `(line, utf16_char)` position in `doc` to a char offset — the one piece of
    /// UTF-16↔char mapping a diagnostics/goto plugin needs, kept app-side so the conversion table
    /// stays out of the kernel. Default `0`.
    fn lsp_pos_to_offset(&self, _doc: DocId, _line: u32, _character: u32) -> usize {
        0
    }

    /// Spawn a PTY terminal rooted at `cwd` and return its [`crate::terminal::TerminalId`], or
    /// `None` if a shell couldn't be started. The app owns the concrete PTY/vt100 session (keyed by
    /// the returned id); the plugin only tracks the id in its dock lifecycle. Default `None`.
    fn terminal_open(&mut self, _cwd: &Path) -> Option<crate::terminal::TerminalId> {
        None
    }

    /// Close (and kill) the PTY terminal with `id`. Default no-op.
    fn terminal_close(&mut self, _id: crate::terminal::TerminalId) {}

    /// Publish the terminal dock lifecycle (open/minimized/active/tab-order) for the app to render.
    /// A pure mirror the renderer reads, like `set_decorations`/`set_popup`. Default no-op.
    fn set_terminal_view(&mut self, _view: crate::terminal::TerminalView) {}

    /// Move keyboard focus to the terminal dock (`true`) or back to the editor (`false`). Focus is
    /// app-owned; the terminal plugin drives it as it opens/closes the dock. Default no-op.
    fn set_terminal_focus(&mut self, _focused: bool) {}

    /// Open `path` and place the caret at the LSP `(line, character)` (a UTF-16 column), resolved
    /// to a char offset app-side once the document is open. The app owns file IO, so this is a
    /// deferred effect (applied on the next drain), like [`Host::open_path`]. Default: open the
    /// path and drop the precise position.
    fn open_location(&mut self, path: &Path, _line: u32, _character: u32) {
        self.open_path(path);
    }

    /// Show `text` in a dismissable info box (LSP hover today). App-owned overlay render state; a
    /// plugin publishes into it through here. Default no-op.
    fn show_info(&mut self, _text: String) {}

    /// Apply a multi-file edit set (an LSP rename result). The app owns file IO + the UTF-16↔char
    /// mapping, so this is a deferred effect: it opens each file and applies the edits as
    /// history-recorded transactions on the next drain. Default no-op.
    fn apply_workspace_edit(&mut self, _edit: crate::lsp::LspWorkspaceEdit) {}

    /// Read the clipboard (system clipboard, falling back to an in-process register). The app owns
    /// the clipboard I/O (system daemon + OSC 52); a clipboard plugin reads through this for paste.
    /// `&mut` because system access is stateful. Default empty.
    fn clipboard_read(&mut self) -> String {
        String::new()
    }

    /// Write `text` to every clipboard sink (system + OSC 52 + register). Default no-op.
    fn clipboard_write(&mut self, _text: String) {}

    /// Execute another registered command by id (composability).
    fn execute(&mut self, command_id: &str);
}
