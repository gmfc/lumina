//! The `App`: terminal lifecycle, the input loop, and the command dispatcher.
//!
//! `App` owns the plugin `Registry` and the `EditorState` as separate fields so dispatch
//! can split-borrow (`registry.dispatch_command(id, &mut self.editor)`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyEventKind, MouseButton, MouseEventKind};
use editor_core::view::{screen_to_char, PaneGeometry};
use editor_core::{edit, motion};
use editor_core::{Change, Document, Selection, Transaction};
use editor_plugin::{Host, Registry};
use ratatui::DefaultTerminal;

use crate::editor::{EditorState, Focus};
use crate::files;
use crate::find::FindState;
use crate::input::Command;
use crate::keymap::{Chord, Keymap};
use crate::picker::{Picker, PickerItem, PickerKind};
use crate::ui::{self, Regions};

/// Tracks click cadence for double/triple-click detection.
struct ClickState {
    at: Instant,
    char_pos: usize,
    count: u8,
}

/// The position-based LSP requests, which all resolve the primary cursor the same way.
enum LspRequest {
    Hover,
    Definition,
    Completion,
    References,
}

pub struct App {
    pub editor: EditorState,
    pub registry: Registry,
    pub quit: bool,
    /// Last body height in rows (for PageUp/PageDown).
    pub page_height: usize,
    /// Screen regions from the last rendered frame (for mouse hit-testing).
    pub regions: Regions,
    /// The active color theme (syntax + chrome).
    pub theme: crate::theme::Theme,
    /// The chord keymap (defaults + config overrides).
    pub keymap: crate::keymap::Keymap,
    /// Pending chord prefix (for multi-key chords like Ctrl+K Ctrl+S).
    pub pending: Vec<crate::keymap::Chord>,
    /// User configuration.
    pub config: crate::config::Config,
    /// System clipboard (with OSC 52 + internal-register fallbacks).
    clipboard: crate::clipboard::Clipboard,
    /// Char offset where the current drag began (selection anchor).
    drag_anchor: Option<usize>,
    /// Index of the tab currently being dragged to reorder, if any.
    tab_drag: Option<usize>,
    /// Last click for multi-click detection.
    last_click: Option<ClickState>,
    // --- Phase 8: background workers + external sync ---
    /// Sender handed to background workers (search, watcher).
    worker_tx: std::sync::mpsc::Sender<crate::worker::WorkerMsg>,
    /// Receiver drained each tick by the main loop.
    worker_rx: std::sync::mpsc::Receiver<crate::worker::WorkerMsg>,
    /// The filesystem debouncer; kept alive so the watch persists.
    _watcher: Option<Box<dyn std::any::Any>>,
    /// The user config file path, watched for hot-reload (plan §6).
    config_path: Option<PathBuf>,
    /// Content hashes of our own pending saves, to suppress save-echo (plan §6).
    pending_self_writes: std::collections::HashMap<PathBuf, u64>,
    /// Auto-scroll to the first externally-changed line on reload (follow mode).
    follow_mode: bool,
    /// Active project-search UI, if open.
    search: Option<crate::search::SearchState>,
    /// The last query we actually ran (to distinguish Enter=run vs Enter=open).
    last_search_run: String,
    // --- Phase 10: LSP ---
    /// Language-server manager (inert unless a server is configured).
    lsp: crate::lsp::LspManager,
    /// Last document revision sent to the LSP, per DocId (change debounce).
    lsp_sent_revision: std::collections::HashMap<editor_core::DocId, u64>,
    /// The bottom terminal dock (tabs of shell sessions).
    pub panel: crate::terminal::TerminalPanel,
}

impl App {
    pub fn new(arg: Option<String>) -> Result<App> {
        let (root, open_file) = resolve_arg(arg);
        let mut editor = EditorState::new(root);
        let config = crate::config::Config::load();

        // Built-in plugins + any external (script) plugins from the plugins dirs. Both
        // register through the same Registry — the external tier has no special path.
        let mut plugins = editor_builtins::all_builtins_with(config.icons);
        for dir in external_plugin_dirs(&editor.workspace.root) {
            plugins.extend(editor_plugin::runtime::load_dir(&dir));
        }
        let mut registry = Registry::with_plugins(plugins);
        registry.activate_all(&mut editor);

        if let Some(file) = open_file {
            match files::load(&file) {
                Ok(mut doc) => {
                    doc.set_caret(0);
                    editor.workspace.open_document(doc);
                }
                Err(e) => {
                    editor.status_message = Some(format!("Could not open {}: {e}", file.display()));
                }
            }
        } else if let Some(session) = crate::session::load(&editor.workspace.root) {
            // Restore the previous session for this project root (plan §6).
            for entry in &session.files {
                if let Ok(mut doc) = files::load(&entry.path) {
                    let pos = doc.clamp(entry.cursor);
                    doc.set_caret(pos);
                    doc.view.scroll_line = entry.scroll;
                    editor.workspace.open_document(doc);
                }
            }
            editor.workspace.focus_tab(
                session
                    .active
                    .min(editor.workspace.tabs.len().saturating_sub(1)),
            );
        }

        let truecolor = crate::theme::truecolor_supported();
        let mut theme = crate::theme::Theme::default_dark(truecolor);
        theme.load_user_overrides();

        editor.sidebar_width = config.sidebar_width;
        let panel = crate::terminal::TerminalPanel::new(config.terminal_height);
        let follow_mode = config.follow_mode;
        let lsp = crate::lsp::LspManager::new(&editor.workspace.root, config.lsp_servers.clone());
        let keymap = build_keymap(&config);

        // Background worker channel + directory watcher on the project root (plan §6). Also
        // watch the config dir (non-recursively) so edits to config.toml hot-reload.
        let (worker_tx, worker_rx) = crate::worker::channel();
        let config_path = crate::config::Config::path();
        let config_dir = config_path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let watcher = crate::worker::spawn_watcher(
            editor.workspace.root.clone(),
            config_dir,
            config.poll_watch,
            worker_tx.clone(),
        );
        if watcher.is_none() {
            editor.status_message =
                Some("File watching unavailable (edits won't auto-reload)".into());
        }

        Ok(App {
            editor,
            registry,
            quit: false,
            page_height: 20,
            regions: Regions::default(),
            theme,
            keymap,
            pending: Vec::new(),
            config,
            clipboard: crate::clipboard::Clipboard::new(),
            drag_anchor: None,
            tab_drag: None,
            last_click: None,
            worker_tx,
            worker_rx,
            _watcher: watcher,
            config_path,
            pending_self_writes: std::collections::HashMap::new(),
            follow_mode,
            search: None,
            last_search_run: String::new(),
            lsp,
            lsp_sent_revision: std::collections::HashMap::new(),
            panel,
        })
    }

    /// Notify the LSP of changes to the active document (debounced by revision), and open
    /// documents the server hasn't seen. Inert unless a server is configured.
    fn update_lsp(&mut self) {
        if !self.lsp.is_enabled() {
            return;
        }
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(doc) = self.editor.workspace.documents.get(id) else {
            return;
        };
        let (Some(path), Some(lang)) = (doc.path.clone(), doc.language.clone()) else {
            return;
        };
        let rev = doc.revision;
        let text = doc.to_string();
        match self.lsp_sent_revision.get(&id) {
            None => {
                self.lsp.did_open(&path, &lang, &text);
                self.lsp_sent_revision.insert(id, rev);
            }
            Some(&sent) if sent != rev => {
                self.lsp.did_change(&path, &lang, &text);
                self.lsp_sent_revision.insert(id, rev);
            }
            _ => {}
        }
    }

    /// LSP position of the primary cursor: `(path, language, line, utf16_char)`.
    fn lsp_position(&self) -> Option<(PathBuf, String, u32, u32)> {
        let doc = self.editor.active_document()?;
        let path = doc.path.clone()?;
        let lang = doc.language.clone()?;
        let head = doc.selections.primary().head;
        let (line, col) = doc.char_to_line_col(head);
        let text = doc.line_text(line);
        let text = text.trim_end_matches(['\n', '\r']);
        let char16 = editor_lsp::position::char_col_to_utf16(text, col);
        Some((path, lang, line as u32, char16))
    }

    /// Open the rename prompt, prefilled with the identifier under the cursor.
    fn open_rename(&mut self) {
        let Some((path, language, line, character)) = self.lsp_position() else {
            return;
        };
        let buffer = self
            .editor
            .active_document()
            .map(|d| {
                let head = d.selections.primary().head;
                let (s, e) = motion::word_at(d, head);
                d.text.slice(s..e).to_string()
            })
            .unwrap_or_default();
        self.editor.overlay = Some(crate::editor::Overlay::RenameInput {
            path,
            language,
            line,
            character,
            buffer,
        });
    }

    /// Act on a high-level LSP event (response or notification).
    fn handle_lsp_event(&mut self, event: crate::lsp::LspEvent) {
        use crate::lsp::LspEvent;
        match event {
            LspEvent::Diagnostics(update) => {
                if let Some(path) = crate::lsp::path_from_uri(&update.uri) {
                    if let Some(id) = self.editor.workspace.find_by_path(&path) {
                        self.editor.diagnostics.insert(id, update.diagnostics);
                    }
                }
            }
            LspEvent::Hover(text) => {
                self.editor.overlay = Some(crate::editor::Overlay::Info(text));
            }
            LspEvent::Goto(loc) => self.goto_location(loc),
            LspEvent::Completion(items) => self.open_completion(items),
            LspEvent::Rename(edit) => self.apply_workspace_edit(edit),
            LspEvent::References(locs) => {
                let entries = locs
                    .into_iter()
                    .map(|l| {
                        let label = location_label(&l);
                        (l, label)
                    })
                    .collect();
                self.open_locations_picker(entries, "References");
            }
            LspEvent::DocumentSymbols(syms) => {
                let uri = self
                    .editor
                    .active_document()
                    .and_then(|d| d.path.as_ref())
                    .map(|p| crate::lsp::uri_for(p));
                let Some(uri) = uri else {
                    return;
                };
                let entries = syms
                    .into_iter()
                    .map(|s| {
                        let label = format!("{}{}", "  ".repeat(s.depth), s.name);
                        let loc = editor_lsp::Location {
                            uri: uri.clone(),
                            line: s.line,
                            character: s.character,
                            end_line: s.line,
                            end_character: s.character,
                        };
                        (loc, label)
                    })
                    .collect();
                self.open_locations_picker(entries, "Symbols");
            }
        }
    }

    /// Open a picker over LSP locations; selecting one jumps there (plan §2.3). The concrete
    /// `Location`s are parked on `EditorState::nav_locations`, indexed by the picker item id.
    fn open_locations_picker(&mut self, entries: Vec<(editor_lsp::Location, String)>, title: &str) {
        if entries.is_empty() {
            self.editor.status_message = Some(format!("No {title}"));
            return;
        }
        let mut locs = Vec::with_capacity(entries.len());
        let items: Vec<crate::picker::PickerItem> = entries
            .into_iter()
            .enumerate()
            .map(|(i, (loc, label))| {
                locs.push(loc);
                crate::picker::PickerItem {
                    id: i.to_string(),
                    label,
                }
            })
            .collect();
        self.editor.nav_locations = locs;
        self.editor.picker = Some(crate::picker::Picker::new(
            crate::picker::PickerKind::Locations,
            title,
            items,
        ));
    }

    /// Open a definition location and place the cursor on it.
    fn goto_location(&mut self, loc: editor_lsp::Location) {
        let Some(path) = crate::lsp::path_from_uri(&loc.uri) else {
            return;
        };
        self.open_path(&path);
        if let Some(doc) = self.editor.active_document_mut() {
            let off = lsp_pos_to_char(doc, loc.line, loc.character);
            doc.set_caret(off);
        }
    }

    /// Apply an LSP `WorkspaceEdit` (rename) across the affected documents as transactions.
    fn apply_workspace_edit(&mut self, edit: editor_lsp::WorkspaceEdit) {
        let mut count = 0usize;
        for (uri, edits) in edit.changes {
            let Some(path) = crate::lsp::path_from_uri(&uri) else {
                continue;
            };
            if self.editor.workspace.find_by_path(&path).is_none() {
                self.open_path(&path);
            }
            let Some(id) = self.editor.workspace.find_by_path(&path) else {
                continue;
            };
            let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
                continue;
            };
            let mut changes: Vec<editor_core::transaction::Change> = edits
                .iter()
                .map(|te| {
                    let start = lsp_pos_to_char(doc, te.start_line, te.start_char16);
                    let end = lsp_pos_to_char(doc, te.end_line, te.end_char16);
                    let (lo, hi) = (start.min(end), start.max(end));
                    editor_core::transaction::Change {
                        at: lo,
                        removed: doc.text.slice(lo..hi).to_string(),
                        inserted: te.new_text.clone(),
                    }
                })
                .collect();
            changes.sort_by_key(|c| c.at);
            let txn = editor_core::Transaction::from_changes(changes);
            if txn.is_empty() {
                continue;
            }
            let before = doc.selections.clone();
            let inverse = txn.apply(doc);
            doc.dirty = true;
            let after = doc.selections.clone();
            doc.history
                .record(txn, inverse, before, after, editor_core::GroupBreak::Force);
            count += 1;
        }
        if count > 0 {
            self.editor.status_message = Some(format!("Renamed across {count} file(s)"));
        }
    }

    /// The active project-search state (read by the renderer).
    pub fn search(&self) -> Option<&crate::search::SearchState> {
        self.search.as_ref()
    }

    /// Reload the config file and rebuild the keymap (the `config.reload` command).
    fn reload_config(&mut self) {
        self.config = crate::config::Config::load();
        self.editor.sidebar_width = self.config.sidebar_width;
        self.panel.height = self.config.terminal_height.clamp(3, 60);
        self.keymap = build_keymap(&self.config);
        self.editor.status_message = Some("Configuration reloaded".into());
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Prime the git gutter for any files restored at startup (plan §4.1).
        self.refresh_git_all();
        while !self.quit {
            self.editor.update_highlights(self.page_height);
            self.editor.update_bracket_match();
            self.update_lsp();
            terminal.draw(|f| ui::draw(f, self))?;
            // Reconcile each PTY's size to the panel region we just laid out.
            self.sync_terminals();

            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    CtEvent::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
                    CtEvent::Mouse(m) => self.on_mouse(m),
                    CtEvent::Paste(s) => self.on_paste(s),
                    CtEvent::Resize(..) => {}
                    _ => {}
                }
            }
            // Drain background worker messages (FS/LSP/parse/terminal output).
            self.drain_workers();
            self.ensure_cursor_visible();
        }
        self.save_session();
        Ok(())
    }

    /// Persist the open files + cursor/scroll for this project root (plan §6).
    fn save_session(&self) {
        let ws = &self.editor.workspace;
        let files: Vec<crate::session::SessionEntry> = ws
            .tabs
            .iter()
            .filter_map(|&id| ws.documents.get(id))
            .filter_map(|doc| {
                doc.path.clone().map(|path| crate::session::SessionEntry {
                    path,
                    cursor: doc.selections.primary().head,
                    scroll: doc.view.scroll_line,
                })
            })
            .collect();
        let session = crate::session::Session {
            files,
            active: ws.active_tab,
        };
        crate::session::save(&ws.root, &session);
    }

    /// Keep the primary cursor within the viewport by adjusting the active doc's scroll.
    fn ensure_cursor_visible(&mut self) {
        let height = self.page_height.max(1);
        if let Some(doc) = self.editor.active_document_mut() {
            let head = doc.selections.primary().head;
            let line = doc.char_to_line(head);
            doc.view.scroll_to_line(line, height);
        }
    }

    fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::keymap::{Chord, Resolve};

        // Modal captures, in priority order.
        if self.handle_modal_key(key) {
            return;
        }
        // Terminal focus: forward keystrokes to the shell (except panel-management chords).
        if self.editor.focus == Focus::Panel && self.handle_terminal_key(key) {
            return;
        }
        // Completion popup: navigation / accept / dismiss keys are consumed here; anything
        // else falls through to edit the buffer, then re-syncs the popup below (plan §2.1).
        if self.editor.completion.is_some() && self.completion_key(key) {
            return;
        }
        // Sidebar focus: arrows/enter drive the explorer; Esc returns to the editor.
        if self.editor.focus == Focus::Sidebar && self.handle_sidebar_key(key) {
            return;
        }

        // Chord keymap resolution (defaults + config overrides).
        self.pending.push(Chord::from_event(key));
        match self.keymap.resolve(&self.pending) {
            Resolve::Command(id) => {
                self.pending.clear();
                self.editor.status_message = None;
                self.exec_id(&id);
            }
            Resolve::Pending => {
                // Keep the prefix armed; show it in the status bar.
                self.editor.status_message = Some(format!("{} …", chords_label(&self.pending)));
            }
            Resolve::None => self.text_entry_fallback(key),
        }

        // Keep the completion popup in sync with the edit just made, or drop it if a modal
        // opened on top of it.
        if self.editor.completion.is_some() {
            if self.editor.overlay.is_some()
                || self.editor.picker.is_some()
                || self.editor.find.is_some()
                || self.search.is_some()
            {
                self.editor.completion = None;
            } else {
                self.refresh_completion();
            }
        }
    }

    /// Route a key to an active modal (overlay / picker / search / find), in priority
    /// order. Returns `true` when a modal consumed the key.
    fn handle_modal_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.editor.overlay.is_some() {
            self.overlay_key(key);
        } else if self.editor.picker.is_some() {
            self.picker_key(key);
        } else if self.search.is_some() {
            self.search_key(key);
        } else if self.editor.find.is_some() {
            self.find_key(key);
        } else {
            return false;
        }
        true
    }

    /// Handle a key while the sidebar is focused. Returns `true` when it was consumed;
    /// `false` lets it fall through to normal chord resolution.
    fn handle_sidebar_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        if key.code == KeyCode::Esc {
            self.editor.focus = Focus::Editor;
            return true;
        }
        if let Some(id) = sidebar_command(key) {
            self.registry.dispatch_command(id, &mut self.editor);
            self.drain_workers();
            return true;
        }
        false
    }

    /// Forward a key to the active terminal. `terminal.*` management chords (e.g. the toggle)
    /// are still honored, so there is always a keyboard way to close / switch / minimize the
    /// panel from inside it; everything else becomes shell input. Returns `true` when handled.
    fn handle_terminal_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        // A focused-but-empty or collapsed panel shouldn't hold the keyboard.
        if self.panel.active_terminal().is_none() || !self.panel.open || self.panel.minimized {
            self.editor.focus = Focus::Editor;
            return false;
        }
        let chord = Chord::from_event(key);
        if let crate::keymap::Resolve::Command(id) =
            self.keymap.resolve(std::slice::from_ref(&chord))
        {
            if id.starts_with("terminal.") {
                self.pending.clear();
                self.exec_id(&id);
                return true;
            }
        }
        let app_cursor = self
            .panel
            .active_terminal()
            .map(|t| t.application_cursor())
            .unwrap_or(false);
        if let Some(bytes) = crate::terminal::key_to_bytes(&key, app_cursor) {
            if let Some(t) = self.panel.active_terminal_mut() {
                t.send_input(&bytes);
            }
        }
        true
    }

    /// Route a bracketed paste to the terminal when it is focused, else into the document.
    fn on_paste(&mut self, s: String) {
        if self.editor.focus == Focus::Panel && self.panel.open && !self.panel.minimized {
            if let Some(t) = self.panel.active_terminal_mut() {
                t.send_input(s.as_bytes());
                return;
            }
        }
        self.dispatch(Command::Paste(s));
    }

    /// Fallback for a key that resolved to nothing: printable text entry into the editor.
    fn text_entry_fallback(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let single = self.pending.len() == 1;
        self.pending.clear();
        if !(single && self.editor.focus == Focus::Editor) {
            return;
        }
        if let KeyCode::Char(c) = key.code {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            if !ctrl && !alt {
                self.dispatch(Command::InsertChar(c));
            }
        }
    }

    /// Execute a command id: built-in editor command, app-level action, or plugin command.
    fn exec_id(&mut self, id: &str) {
        if let Some(cmd) = crate::commands::command_for_id(id) {
            self.dispatch(cmd);
            return;
        }
        match id {
            "config.reload" => self.reload_config(),
            "view.toggleTheme" => self.toggle_theme(),
            other => {
                if !self.registry.dispatch_command(other, &mut self.editor) {
                    self.editor.status_message = Some(format!("Unknown command: {other}"));
                } else {
                    self.drain_workers();
                }
            }
        }
    }

    /// Handle a key while the confirm-close overlay is open.
    fn overlay_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(overlay) = self.editor.overlay.clone() else {
            return;
        };
        match overlay {
            crate::editor::Overlay::ConfirmClose { tab } => match key.code {
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.editor.workspace.focus_tab(tab);
                    self.save_active();
                    self.editor.workspace.close_tab(tab);
                    self.editor.overlay = None;
                }
                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('y') => {
                    self.editor.workspace.close_tab(tab);
                    self.editor.overlay = None;
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c') => {
                    self.editor.overlay = None;
                }
                _ => {}
            },
            crate::editor::Overlay::Info(_) => {
                // Any key dismisses an info popup.
                self.editor.overlay = None;
            }
            crate::editor::Overlay::RenameInput {
                path,
                language,
                line,
                character,
                mut buffer,
            } => match key.code {
                KeyCode::Esc => self.editor.overlay = None,
                KeyCode::Enter => {
                    self.editor.overlay = None;
                    if !buffer.is_empty() {
                        self.lsp
                            .request_rename(&path, &language, line, character, &buffer);
                    }
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    self.editor.overlay = Some(crate::editor::Overlay::RenameInput {
                        path,
                        language,
                        line,
                        character,
                        buffer,
                    });
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    buffer.push(c);
                    self.editor.overlay = Some(crate::editor::Overlay::RenameInput {
                        path,
                        language,
                        line,
                        character,
                        buffer,
                    });
                }
                _ => {}
            },
            crate::editor::Overlay::SaveAsInput { mut buffer } => match key.code {
                KeyCode::Esc => self.editor.overlay = None,
                KeyCode::Enter => {
                    self.editor.overlay = None;
                    self.save_as_to(&buffer);
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput { buffer });
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    buffer.push(c);
                    self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput { buffer });
                }
                _ => {}
            },
        }
    }

    /// Handle a key while the find/replace widget is open.
    fn find_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Esc => {
                self.editor.find = None;
            }
            KeyCode::Enter if alt => self.replace_current(),
            KeyCode::Char('a' | 'A') if alt => self.replace_all(),
            KeyCode::Char('c' | 'C') if alt => {
                toggle_and(&mut self.editor.find, |f| {
                    f.case_sensitive = !f.case_sensitive
                });
                self.recompute_find();
            }
            KeyCode::Char('w' | 'W') if alt => {
                toggle_and(&mut self.editor.find, |f| f.whole_word = !f.whole_word);
                self.recompute_find();
            }
            KeyCode::Char('r' | 'R') if alt => {
                toggle_and(&mut self.editor.find, |f| f.regex = !f.regex);
                self.recompute_find();
            }
            KeyCode::Enter if shift => {
                toggle_and(&mut self.editor.find, |f| f.select_prev());
                self.focus_current_match();
            }
            KeyCode::Up => {
                toggle_and(&mut self.editor.find, |f| f.select_prev());
                self.focus_current_match();
            }
            KeyCode::Enter | KeyCode::Down => {
                toggle_and(&mut self.editor.find, |f| f.select_next());
                self.focus_current_match();
            }
            KeyCode::Tab => toggle_and(&mut self.editor.find, |f| f.toggle_field()),
            KeyCode::Backspace => {
                toggle_and(&mut self.editor.find, |f| f.backspace());
                self.recompute_find();
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                toggle_and(&mut self.editor.find, |f| f.input_char(c));
                self.recompute_find();
            }
            _ => {}
        }
    }

    /// Open the find (or find+replace) widget, prefilling from the current selection.
    fn open_find(&mut self, replace_mode: bool) {
        let mut fs = FindState::new(replace_mode);
        if let Some(doc) = self.editor.active_document() {
            let sel = doc.selections.primary();
            fs.origin = sel.from();
            if !sel.is_empty() {
                fs.query = doc.text.slice(sel.from()..sel.to()).to_string();
            }
        }
        self.editor.find = Some(fs);
        self.recompute_find();
    }

    /// Recompute matches against the active document and move to the current one.
    fn recompute_find(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let text = {
            let Some(doc) = self.editor.workspace.documents.get(id) else {
                return;
            };
            doc.text.to_string()
        };
        if let Some(find) = &mut self.editor.find {
            let origin = find.origin;
            find.recompute(&text, origin);
        }
        self.focus_current_match();
    }

    /// Select the current match in the editor so it scrolls into view + highlights.
    fn focus_current_match(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let m = self.editor.find.as_ref().and_then(|f| f.current_match());
        if let (Some((s, e)), Some(doc)) = (m, self.editor.workspace.documents.get_mut(id)) {
            doc.selections.set_single(Selection::new(s, e));
        }
    }

    /// Replace the current match with the (capture-expanded) replacement.
    fn replace_current(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some((s, e)) = self.editor.find.as_ref().and_then(|f| f.current_match()) else {
            return;
        };
        let matched = {
            let Some(doc) = self.editor.workspace.documents.get(id) else {
                return;
            };
            doc.text.slice(s..e).to_string()
        };
        let repl = self
            .editor
            .find
            .as_ref()
            .map(|f| f.replacement_for(&matched))
            .unwrap_or_default();
        let txn = {
            let doc = &self.editor.workspace.documents[id];
            Transaction::replace(doc, s..e, &repl)
        };
        self.editor.apply_transaction(id, txn);
        self.recompute_find();
    }

    /// Replace every match in one undoable transaction (plan §6).
    fn replace_all(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let matches = self
            .editor
            .find
            .as_ref()
            .map(|f| f.matches.clone())
            .unwrap_or_default();
        if matches.is_empty() {
            return;
        }
        let mut changes = Vec::with_capacity(matches.len());
        {
            let doc = &self.editor.workspace.documents[id];
            for &(s, e) in &matches {
                let matched = doc.text.slice(s..e).to_string();
                let inserted = self
                    .editor
                    .find
                    .as_ref()
                    .map(|f| f.replacement_for(&matched))
                    .unwrap_or_default();
                changes.push(Change {
                    at: s,
                    removed: matched,
                    inserted,
                });
            }
        }
        let n = changes.len();
        self.editor
            .apply_transaction(id, Transaction::from_changes(changes));
        self.recompute_find();
        self.editor.status_message = Some(format!("Replaced {n} occurrence(s)"));
    }

    // --- picker (palette / quick open / goto line) -----------------------------

    /// Open the command palette: built-in commands + plugin-contributed commands.
    fn open_palette(&mut self) {
        let mut items: Vec<PickerItem> = crate::commands::palette_entries()
            .iter()
            .map(|(id, title)| PickerItem {
                id: id.to_string(),
                label: title.to_string(),
            })
            .collect();
        for spec in self.registry.commands() {
            items.push(PickerItem {
                id: spec.id.clone(),
                label: spec.title.clone(),
            });
        }
        self.editor.picker = Some(Picker::new(PickerKind::Command, "Command", items));
    }

    /// Open quick-open: fuzzy-filter files under the project root (ignore-walked).
    fn open_quick_open(&mut self) {
        let root = self.editor.workspace.root.clone();
        let mut items = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.flatten().take(10_000) {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let path = entry.path();
                let label = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned();
                items.push(PickerItem {
                    id: path.to_string_lossy().into_owned(),
                    label,
                });
            }
        }
        self.editor.picker = Some(Picker::new(PickerKind::File, "Go to File", items));
    }

    fn open_goto_line(&mut self) {
        self.editor.picker = Some(Picker::new(PickerKind::GotoLine, "Go to Line", Vec::new()));
    }

    fn picker_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(picker) = &mut self.editor.picker else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.editor.picker = None,
            KeyCode::Up => picker.move_selection(-1),
            KeyCode::Down => picker.move_selection(1),
            KeyCode::Backspace => picker.backspace(),
            KeyCode::Enter => self.activate_picker(),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(crossterm::event::KeyModifiers::CONTROL) =>
            {
                picker.input_char(c)
            }
            _ => {}
        }
    }

    fn activate_picker(&mut self) {
        let Some(picker) = self.editor.picker.take() else {
            return;
        };
        match picker.kind {
            PickerKind::Command => {
                if let Some(item) = picker.selected_item() {
                    let id = item.id.clone();
                    self.exec_id(&id);
                }
            }
            PickerKind::File => {
                if let Some(item) = picker.selected_item() {
                    let path = std::path::PathBuf::from(&item.id);
                    self.open_path(&path);
                }
            }
            PickerKind::GotoLine => {
                if let Ok(line) = picker.query.trim().parse::<usize>() {
                    self.goto_line(line.saturating_sub(1));
                }
            }
            PickerKind::Locations => {
                if let Some(loc) = picker
                    .selected_item()
                    .and_then(|item| item.id.parse::<usize>().ok())
                    .and_then(|i| self.editor.nav_locations.get(i).cloned())
                {
                    self.goto_location(loc);
                }
            }
        }
    }

    /// Open a caret-anchored completion popup from server `items` (plan §2.1). Anchors at the
    /// start of the identifier under the caret so the popup filters on what's already typed.
    fn open_completion(&mut self, items: Vec<editor_lsp::CompletionItem>) {
        if items.is_empty() {
            return;
        }
        let Some(doc) = self.editor.active_document() else {
            return;
        };
        let head = doc.selections.primary().head;
        let mut anchor = head;
        while anchor > 0 {
            let ch = doc.text.char(anchor - 1);
            if ch.is_alphanumeric() || ch == '_' {
                anchor -= 1;
            } else {
                break;
            }
        }
        let prefix = doc.text.slice(anchor..head).to_string();
        let state = crate::completion::CompletionState::new(items, anchor, prefix);
        if !state.is_empty() {
            self.editor.completion = Some(state);
        }
    }

    /// Handle a key while the completion popup is open. Returns `true` when it fully consumed
    /// the key (navigation / accept / dismiss); `false` lets the key edit the buffer normally,
    /// after which `refresh_completion` re-syncs the popup to the new text.
    fn completion_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Down => {
                if let Some(c) = self.editor.completion.as_mut() {
                    c.move_sel(1);
                }
                true
            }
            KeyCode::Up => {
                if let Some(c) = self.editor.completion.as_mut() {
                    c.move_sel(-1);
                }
                true
            }
            KeyCode::Esc => {
                self.editor.completion = None;
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                let insert = self
                    .editor
                    .completion
                    .as_ref()
                    .and_then(|c| c.selected_item().map(|it| it.insert_text.clone()));
                self.editor.completion = None;
                if let Some(insert) = insert {
                    self.insert_completion(&insert);
                }
                true
            }
            _ => false,
        }
    }

    /// After a buffer edit while the popup is open, recompute the typed prefix and re-filter,
    /// dismissing when the caret leaves the identifier or nothing matches.
    fn refresh_completion(&mut self) {
        let Some(anchor) = self.editor.completion.as_ref().map(|c| c.anchor) else {
            return;
        };
        let prefix = self.editor.active_document().and_then(|doc| {
            let head = doc.selections.primary().head;
            if head < anchor {
                return None;
            }
            let p = doc.text.slice(anchor..head).to_string();
            if p.chars().any(|c| !(c.is_alphanumeric() || c == '_')) {
                return None;
            }
            Some(p)
        });
        match prefix {
            None => self.editor.completion = None,
            Some(prefix) => {
                if let Some(c) = self.editor.completion.as_mut() {
                    c.prefix = prefix;
                    c.refilter();
                    if c.is_empty() {
                        self.editor.completion = None;
                    }
                }
            }
        }
    }

    /// Jump the caret to the next (`dir > 0`) or previous diagnostic in the active document,
    /// wrapping around the ends (plan §2.2).
    fn goto_diagnostic(&mut self, dir: isize) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(doc) = self.editor.workspace.documents.get(id) else {
            return;
        };
        let Some(diags) = self.editor.diagnostics.get(&id) else {
            return;
        };
        if diags.is_empty() {
            return;
        }
        let mut offs: Vec<usize> = diags
            .iter()
            .map(|d| lsp_pos_to_char(doc, d.line, d.start_char16))
            .collect();
        offs.sort_unstable();
        offs.dedup();
        let head = doc.selections.primary().head;
        let target = if dir > 0 {
            offs.iter().copied().find(|&o| o > head).unwrap_or(offs[0])
        } else {
            offs.iter()
                .rev()
                .copied()
                .find(|&o| o < head)
                .unwrap_or_else(|| *offs.last().unwrap())
        };
        if let Some(d) = self.editor.workspace.documents.get_mut(id) {
            d.set_caret(target);
        }
        self.ensure_cursor_visible();
        self.editor.update_bracket_match();
    }

    /// The diagnostic whose range covers the primary caret, for the status-line message
    /// (plan §2.2). Borrows `self`, so the pure renderer can display it directly.
    pub(crate) fn diagnostic_at_caret(&self) -> Option<(editor_lsp::Severity, &str)> {
        let id = self.editor.workspace.active_doc()?;
        let doc = self.editor.workspace.documents.get(id)?;
        let diags = self.editor.diagnostics.get(&id)?;
        let head = doc.selections.primary().head;
        diags.iter().find_map(|d| {
            let start = lsp_pos_to_char(doc, d.line, d.start_char16);
            let end = lsp_pos_to_char(doc, d.end_line, d.end_char16).max(start);
            (head >= start && head <= end).then_some((d.severity, d.message.as_str()))
        })
    }

    /// Insert a completion, replacing the identifier prefix already typed before the cursor.
    fn insert_completion(&mut self, insert: &str) {
        self.with_doc(|d| {
            let head = d.selections.primary().head;
            // Walk back over identifier chars to find the prefix to replace.
            let mut start = head;
            while start > 0 {
                let ch = d.text.char(start - 1);
                if ch.is_alphanumeric() || ch == '_' {
                    start -= 1;
                } else {
                    break;
                }
            }
            editor_core::edit::edit_selections(
                d,
                |_doc, sel| {
                    if sel.head == head {
                        (start..head, insert.to_string())
                    } else {
                        (sel.span(), insert.to_string())
                    }
                },
                editor_core::GroupBreak::Force,
            );
        });
    }

    fn goto_line(&mut self, line: usize) {
        self.with_doc(|d| {
            let l = line.min(d.len_lines().saturating_sub(1));
            let off = d.line_to_char(l);
            d.set_caret(off);
        });
    }

    // --- project search (Ctrl+Shift+F) -----------------------------------------

    fn open_search(&mut self) {
        let mut st = crate::search::SearchState::default();
        if let Some(t) = self.selection_text() {
            st.query = t;
        }
        self.search = Some(st);
    }

    /// Kick off a background project search for the current query.
    fn run_project_search(&mut self) {
        let (query, case) = match &self.search {
            Some(s) if !s.query.is_empty() => (s.query.clone(), s.case_sensitive),
            _ => return,
        };
        if let Some(s) = &mut self.search {
            s.running = true;
            s.results.clear();
        }
        self.last_search_run = query.clone();
        crate::worker::spawn_search(
            self.editor.workspace.root.clone(),
            query,
            case,
            self.worker_tx.clone(),
        );
    }

    fn search_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.search = None,
            KeyCode::Up => {
                if let Some(s) = &mut self.search {
                    s.move_selection(-1);
                }
            }
            KeyCode::Down => {
                if let Some(s) = &mut self.search {
                    s.move_selection(1);
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = &mut self.search {
                    s.query.pop();
                }
            }
            KeyCode::Enter => {
                let changed = self
                    .search
                    .as_ref()
                    .map(|s| s.query != self.last_search_run)
                    .unwrap_or(false);
                if changed {
                    self.run_project_search();
                } else {
                    self.open_search_hit();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(s) = &mut self.search {
                    s.query.push(c);
                }
            }
            _ => {}
        }
    }

    fn open_search_hit(&mut self) {
        let hit = self.search.as_ref().and_then(|s| s.selected_hit()).cloned();
        if let Some(hit) = hit {
            self.open_path(&hit.path);
            self.goto_line(hit.line.saturating_sub(1));
        }
    }

    // --- multi-cursor ----------------------------------------------------------

    /// Ctrl+D: first press selects the word under the cursor; each subsequent press adds a
    /// selection at the next occurrence of the current selection's text (wrapping).
    fn add_cursor_next_match(&mut self) {
        let Some(doc) = self.editor.active_document_mut() else {
            return;
        };
        let primary = doc.selections.primary();
        if primary.is_empty() {
            let (s, e) = motion::word_at(doc, primary.head);
            if s < e {
                doc.selections.set_single(Selection::new(s, e));
            }
            return;
        }
        let chars: Vec<char> = doc.text.chars().collect();
        let needle: Vec<char> = chars[primary.from()..primary.to()].to_vec();
        if needle.is_empty() {
            return;
        }
        let search_from = doc
            .selections
            .ranges()
            .iter()
            .map(|s| s.to())
            .max()
            .unwrap_or(0);
        if let Some((ms, me)) = find_next_occurrence(&chars, &needle, search_from) {
            // Skip if that range is already selected.
            if doc
                .selections
                .ranges()
                .iter()
                .any(|s| s.from() == ms && s.to() == me)
            {
                return;
            }
            doc.selections.push(Selection::new(ms, me));
            doc.selections.normalize();
            // Make the newly added match the primary so the viewport follows it.
            if let Some(idx) = doc.selections.ranges().iter().position(|s| s.head == me) {
                doc.selections.set_primary(idx);
            }
        }
    }

    /// Alt+Up / Alt+Down: add a caret one line above/below at the same display column.
    fn add_cursor_vertical(&mut self, dir: isize) {
        let Some(doc) = self.editor.active_document_mut() else {
            return;
        };
        let primary = doc.selections.primary();
        let (line, col) = doc.char_to_line_col(primary.head);
        let line_text = doc.line_text(line);
        let line_body = line_text.trim_end_matches(['\n', '\r']);
        let display_col = editor_core::view::char_to_display_col(line_body, col, doc.tab_width);
        let target = (line as isize + dir).clamp(0, doc.len_lines() as isize - 1) as usize;
        if target == line {
            return;
        }
        let target_text = doc.line_text(target);
        let target_body = target_text.trim_end_matches(['\n', '\r']);
        let ch = editor_core::view::display_col_to_char(target_body, display_col, doc.tab_width);
        let head = doc.line_to_char(target) + ch;
        doc.selections.push(Selection::caret(head));
        doc.selections.normalize();
        if let Some(idx) = doc.selections.ranges().iter().position(|s| s.head == head) {
            doc.selections.set_primary(idx);
        }
    }

    /// Toggle between the dark and light themes.
    fn toggle_theme(&mut self) {
        let truecolor = crate::theme::truecolor_supported();
        self.theme = if self.theme.is_dark() {
            crate::theme::Theme::default_light(truecolor)
        } else {
            crate::theme::Theme::default_dark(truecolor)
        };
        self.editor.status_message = Some(format!(
            "Theme: {}",
            if self.theme.is_dark() {
                "dark"
            } else {
                "light"
            }
        ));
    }

    // --- clipboard -------------------------------------------------------------

    fn selection_text(&self) -> Option<String> {
        let doc = self.editor.active_document()?;
        let sel = doc.selections.primary();
        if sel.is_empty() {
            None
        } else {
            Some(doc.text.slice(sel.from()..sel.to()).to_string())
        }
    }

    fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        let (col, row) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_left_down(col, row, m.modifiers),
            MouseEventKind::Down(MouseButton::Middle) => self.mouse_middle_down(col, row),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_left_drag(col, row),
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_anchor = None;
                self.tab_drag = None;
            }
            MouseEventKind::ScrollUp => self.scroll_at(col, row, -3),
            MouseEventKind::ScrollDown => self.scroll_at(col, row, 3),
            _ => {}
        }
    }

    /// Left button press: focus + place/add a cursor in the editor, or hit the tab bar
    /// or sidebar depending on which region was clicked.
    fn mouse_left_down(&mut self, col: u16, row: u16, mods: crossterm::event::KeyModifiers) {
        if in_rect(self.regions.editor, col, row) {
            self.editor.focus = Focus::Editor;
            if mods.contains(crossterm::event::KeyModifiers::ALT) {
                // Alt+Click adds a cursor (multi-cursor).
                if let Some(off) = self.editor_offset_at(col, row) {
                    self.with_doc(|d| {
                        d.selections.push(Selection::caret(off));
                        d.selections.normalize();
                    });
                }
            } else {
                self.editor_click(col, row);
            }
        } else if in_rect(self.regions.tabs, col, row) {
            self.tab_bar_click(col);
        } else if self.regions.sidebar.is_some_and(|r| in_rect(r, col, row)) {
            self.editor.focus = Focus::Sidebar;
            self.sidebar_click(col, row);
        } else if self
            .regions
            .panel_header
            .is_some_and(|r| in_rect(r, col, row))
        {
            self.panel_header_click(col, row);
        } else if self
            .regions
            .panel_content
            .is_some_and(|r| in_rect(r, col, row))
            && self.panel.active_terminal().is_some()
        {
            self.editor.focus = Focus::Panel;
        }
    }

    /// Middle-click on the tab bar closes that tab (VS Code parity).
    fn mouse_middle_down(&mut self, col: u16, row: u16) {
        if in_rect(self.regions.tabs, col, row) {
            if let Some((i, _)) = self.tab_at(col) {
                self.request_close(i);
            }
        }
    }

    /// Left button drag: reorder the dragged tab, or extend the editor selection.
    fn mouse_left_drag(&mut self, col: u16, row: u16) {
        if let Some(from) = self.tab_drag {
            self.drag_tab(from, col, row);
        } else if let Some(anchor) = self.drag_anchor {
            if let Some(off) = self.editor_offset_at(col, row) {
                self.with_doc(|d| {
                    d.selections.set_single(Selection::new(anchor, off));
                });
            }
        }
    }

    /// Continue an in-progress tab drag, reordering when the cursor crosses onto a new tab.
    fn drag_tab(&mut self, from: usize, col: u16, row: u16) {
        if !in_rect(self.regions.tabs, col, row) {
            return;
        }
        if let Some((to, _)) = self.tab_at(col) {
            if to != from {
                self.editor.workspace.move_tab(from, to);
                self.tab_drag = Some(to);
            }
        }
    }

    /// Char offset under a screen cell in the editor pane, or `None`.
    fn editor_offset_at(&self, col: u16, row: u16) -> Option<usize> {
        let doc = self.editor.active_document()?;
        let geo = self.editor_geometry(doc);
        screen_to_char(doc, &geo, col, row)
    }

    fn editor_geometry(&self, doc: &Document) -> PaneGeometry {
        let r = self.regions.editor;
        PaneGeometry {
            origin_x: r.x,
            origin_y: r.y,
            gutter: ui::gutter_width(doc),
            scroll_line: doc.view.scroll_line,
            tab_width: doc.tab_width,
            height: r.height,
        }
    }

    /// Handle a left click in the editor: place cursor, or select word/line on multi-click.
    fn editor_click(&mut self, col: u16, row: u16) {
        let Some(off) = self.editor_offset_at(col, row) else {
            return;
        };
        // Determine click count from timing + position.
        let now = Instant::now();
        let count = match &self.last_click {
            Some(c)
                if now.duration_since(c.at) < Duration::from_millis(400) && c.char_pos == off =>
            {
                (c.count % 3) + 1
            }
            _ => 1,
        };
        self.last_click = Some(ClickState {
            at: now,
            char_pos: off,
            count,
        });

        match count {
            2 => self.with_doc(|d| {
                let (s, e) = motion::word_at(d, off);
                d.selections.set_single(Selection::new(s, e));
            }),
            3 => self.with_doc(|d| {
                let (s, e) = motion::line_at(d, off);
                d.selections.set_single(Selection::new(s, e));
            }),
            _ => {
                self.drag_anchor = Some(off);
                self.with_doc(|d| d.selections.set_single(Selection::caret(off)));
            }
        }
    }

    /// Hit-test a tab-bar column, returning `(tab_index, on_close_marker)`.
    fn tab_at(&self, col: u16) -> Option<(usize, bool)> {
        // Tabs render as " name marker " segments; recompute their extents to hit-test.
        let ws = &self.editor.workspace;
        let mut x = self.regions.tabs.x;
        for (i, &id) in ws.tabs.iter().enumerate() {
            let doc = ws.documents.get(id)?;
            let name = doc
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into());
            let label_w = 1 + name.chars().count() + 1 + 1 + 1; // " name _ marker _ "
            let seg_end = x + label_w as u16;
            if col >= x && col < seg_end {
                // The marker (× / ●) sits near the segment's right edge.
                return Some((i, col >= seg_end.saturating_sub(2)));
            }
            x = seg_end;
        }
        None
    }

    fn tab_bar_click(&mut self, col: u16) {
        if let Some((i, on_close)) = self.tab_at(col) {
            if on_close {
                self.request_close(i);
            } else {
                self.editor.workspace.focus_tab(i);
                self.tab_drag = Some(i); // arm a potential drag-to-reorder
            }
        }
    }

    fn sidebar_click(&mut self, _col: u16, row: u16) {
        // Route to the explorer plugin's panel row, if present (Phase 4).
        if let Some(panel) = self.editor.panels.get("explorer.tree") {
            // Panel rows are drawn into the sidebar block's *inner* area (below the
            // " EXPLORER " title row), so hit-test against that content region — using the
            // outer region's top would select the row one line below the cursor.
            let inner_top = self
                .regions
                .sidebar_inner
                .or(self.regions.sidebar)
                .map(|r| r.y)
                .unwrap_or(0);
            let idx = row.saturating_sub(inner_top) as usize;
            if let Some(line) = panel.lines.get(idx) {
                if let Some(payload) = line.payload.clone() {
                    self.registry
                        .activate_panel_row("explorer.tree", &payload, &mut self.editor);
                }
            }
        }
    }

    fn scroll_editor(&mut self, delta: isize) {
        if let Some(doc) = self.editor.active_document_mut() {
            let max = doc.len_lines().saturating_sub(1);
            let next = (doc.view.scroll_line as isize + delta).clamp(0, max as isize);
            doc.view.scroll_line = next as usize;
        }
    }

    /// Route a wheel scroll: the terminal's scrollback when over the panel, else the editor.
    fn scroll_at(&mut self, col: u16, row: u16, delta: isize) {
        if self
            .regions
            .panel_content
            .is_some_and(|r| in_rect(r, col, row))
        {
            if let Some(t) = self.panel.active_terminal_mut() {
                t.scroll(delta);
            }
        } else {
            self.scroll_editor(delta);
        }
    }

    // --- terminal panel -------------------------------------------------------

    /// Toggle the dock: open + focus when closed or minimized, else close it.
    fn toggle_terminal(&mut self) {
        if self.panel.open && !self.panel.minimized {
            self.panel.open = false;
            self.editor.focus = Focus::Editor;
        } else {
            self.focus_terminal();
        }
    }

    /// Open (if needed), expand, and focus the panel, spawning a shell on first use.
    fn focus_terminal(&mut self) {
        self.panel.open = true;
        self.panel.minimized = false;
        if self.panel.terminals.is_empty() {
            self.spawn_terminal();
        }
        if self.panel.active_terminal().is_some() {
            self.editor.focus = Focus::Panel;
        } else {
            self.panel.open = false;
        }
    }

    /// Open a brand-new terminal tab and focus the panel.
    fn new_terminal(&mut self) {
        self.panel.open = true;
        self.panel.minimized = false;
        self.spawn_terminal();
        if self.panel.active_terminal().is_some() {
            self.editor.focus = Focus::Panel;
        }
    }

    /// Close the active terminal tab; close the dock and return to the editor if it was last.
    fn close_terminal(&mut self) {
        if self.panel.close_active() {
            self.panel.open = false;
            self.editor.focus = Focus::Editor;
        }
    }

    /// Collapse the panel to its header row, or restore it.
    fn minimize_terminal(&mut self) {
        if !self.panel.open {
            return;
        }
        self.panel.toggle_minimized();
        if self.panel.minimized {
            self.editor.focus = Focus::Editor;
        } else if self.panel.active_terminal().is_some() {
            self.editor.focus = Focus::Panel;
        }
    }

    /// Spawn a shell into a new tab, sized to the current panel region.
    fn spawn_terminal(&mut self) {
        let cwd = self.editor.workspace.root.clone();
        let shell = crate::terminal::default_shell(self.config.terminal_shell.as_deref());
        let (rows, cols) = self.panel_content_size();
        let tx = self.worker_tx.clone();
        if !self.panel.open_new(&cwd, &shell, rows, cols, tx) {
            self.editor.status_message = Some("Failed to start terminal".into());
        }
    }

    /// A terminal's `(rows, cols)`, from the last-laid-out panel region with a pre-draw fallback.
    fn panel_content_size(&self) -> (u16, u16) {
        if let Some(rect) = self.regions.panel_content {
            if rect.width > 0 && rect.height > 0 {
                return (rect.height, rect.width);
            }
        }
        (self.panel.height, self.regions.editor.width.max(80))
    }

    /// Resize every PTY to the drawn content region (a cheap no-op when unchanged).
    fn sync_terminals(&mut self) {
        let Some(rect) = self.regions.panel_content else {
            return;
        };
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        for t in &mut self.panel.terminals {
            t.resize(rect.height, rect.width);
        }
    }

    /// Handle a left click on the panel header (minimize control, a tab, its close mark, or `+`).
    fn panel_header_click(&mut self, col: u16, row: u16) {
        let Some(header) = self.regions.panel_header else {
            return;
        };
        if !in_rect(header, col, row) {
            return;
        }
        let mut x = header.x;
        for (label, hit) in self.panel.header_segments() {
            let w = label.chars().count() as u16;
            let seg_end = x.saturating_add(w);
            if col >= x && col < seg_end {
                self.activate_header_hit(hit, col, seg_end);
                return;
            }
            x = seg_end;
        }
    }

    /// Act on a header segment: the close mark sits in the tab's last two columns.
    fn activate_header_hit(&mut self, hit: crate::terminal::HeaderHit, col: u16, seg_end: u16) {
        use crate::terminal::HeaderHit;
        match hit {
            HeaderHit::Minimize => self.minimize_terminal(),
            HeaderHit::New => self.new_terminal(),
            HeaderHit::Tab(i) => {
                self.panel.select(i);
                if col >= seg_end.saturating_sub(2) {
                    self.close_terminal();
                } else {
                    self.panel.open = true;
                    self.panel.minimized = false;
                    if self.panel.active_terminal().is_some() {
                        self.editor.focus = Focus::Panel;
                    }
                }
            }
        }
    }

    fn drain_workers(&mut self) {
        // Apply any queued opens/commands/events produced during dispatch.
        let opens: Vec<PathBuf> = std::mem::take(&mut self.editor.pending_opens);
        for path in opens {
            self.open_path(&path);
        }
        let cmds: Vec<String> = std::mem::take(&mut self.editor.pending_commands);
        for id in cmds {
            self.registry.dispatch_command(&id, &mut self.editor);
        }
        let events = std::mem::take(&mut self.editor.pending_events);
        for ev in events {
            self.registry.broadcast(&ev, &mut self.editor);
        }

        // LSP responses/notifications: diagnostics, hover, goto, completion, rename.
        for event in self.lsp.poll() {
            self.handle_lsp_event(event);
        }

        self.drain_worker_channel();
    }

    /// Drain background worker messages (FS watch, git, project search) into state.
    fn drain_worker_channel(&mut self) {
        use crate::worker::WorkerMsg;
        // Cap terminal bytes processed per tick so a flooding shell (e.g. `yes`) can't starve
        // the render/input loop — the UI stays responsive, so Ctrl+C (which stops the flood)
        // remains reachable. Anything past the budget stays queued for the next ticks.
        const TERM_BYTE_BUDGET: usize = 1 << 20; // 1 MiB
        let mut term_bytes = 0usize;
        while let Ok(msg) = self.worker_rx.try_recv() {
            match msg {
                WorkerMsg::DiskChanged { path } => self.on_disk_changed(&path),
                WorkerMsg::GitStatus { path, statuses } => {
                    if let Some(id) = self.editor.workspace.find_by_path(&path) {
                        self.editor.git_hunks.insert(id, statuses);
                    }
                }
                WorkerMsg::SearchComplete { query, hits } => self.on_search_complete(query, hits),
                WorkerMsg::TerminalOutput { id, bytes } => {
                    term_bytes += bytes.len();
                    if let Some(t) = self.panel.terminal_mut(id) {
                        t.feed(&bytes);
                    }
                    if term_bytes >= TERM_BYTE_BUDGET {
                        break;
                    }
                }
                WorkerMsg::TerminalExited { id } => {
                    if let Some(t) = self.panel.terminal_mut(id) {
                        t.mark_exited();
                    }
                }
            }
        }
    }

    /// Fold a completed project search into the open search panel, if it's still the live query.
    fn on_search_complete(&mut self, query: String, hits: Vec<crate::search::SearchHit>) {
        if let Some(search) = &mut self.search {
            if search.query == query {
                search.results = hits;
                search.selected = 0;
                search.running = false;
            }
        }
    }

    /// Reconcile an external on-disk change against the buffer (plan §6 decision matrix).
    fn on_disk_changed(&mut self, path: &std::path::Path) {
        // A change to the user config file → hot-reload keymap/settings (plan §6).
        if self.config_path.as_deref() == Some(path) {
            self.reload_config();
            return;
        }

        // Not one of our open docs → refresh the tree and move on.
        let Some(id) = self.editor.workspace.find_by_path(path) else {
            self.editor
                .pending_events
                .push(editor_plugin::event::Event::DidChangeConfig);
            // Also nudge the explorer to rescan on any tree change.
            return;
        };

        let Ok(bytes) = std::fs::read(path) else {
            // Deleted mid-race, or unreadable → flag deletion, keep the buffer.
            if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
                doc.deleted_on_disk = true;
                doc.dirty = true; // a save re-creates it
            }
            return;
        };
        let fp = crate::files::fingerprint(&bytes);

        // Our own save echoing back → drop it.
        if self.pending_self_writes.get(path) == Some(&fp.hash) {
            self.pending_self_writes.remove(path);
            if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
                doc.disk = fp;
            }
            return;
        }

        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        // No real change (hash matches last-loaded) → just refresh the fingerprint.
        if doc.disk.hash == fp.hash {
            doc.disk = fp;
            return;
        }

        if doc.dirty {
            // Never clobber unsaved work — flag a conflict for the user to resolve.
            doc.external_conflict = Some(fp);
            return;
        }

        // Clean buffer → reload, following the cursor/scroll through the diff.
        let new_text = String::from_utf8_lossy(&bytes).into_owned();
        let old_text = doc.to_string();
        let heads: Vec<usize> = doc.selections.ranges().iter().map(|s| s.head).collect();
        let mapped: Vec<usize> = heads
            .iter()
            .map(|&h| crate::sync::map_offset(&old_text, &new_text, h))
            .collect();

        doc.set_text_str(&new_text);
        let clamped: Vec<editor_core::Selection> = mapped
            .iter()
            .map(|&m| editor_core::Selection::caret(doc.clamp(m)))
            .collect();
        doc.selections = editor_core::Selections::from_iter(clamped);
        doc.disk = fp;
        doc.dirty = false;
        doc.externally_reloaded = true;

        if self.follow_mode {
            let line = crate::sync::first_changed_line(&old_text, &new_text);
            doc.view.scroll_line = line.saturating_sub(2);
        }
        self.editor
            .pending_events
            .push(editor_plugin::event::Event::ExternalReload(id));
        // The file changed under us (e.g. an agent wrote it) — refresh its git gutter.
        self.request_git_status(id);
    }

    /// The single dispatcher — the only place editor state mutates (plan §5).
    pub fn dispatch(&mut self, cmd: Command) {
        self.editor.status_message = None;
        let page = self.page_height;

        match cmd {
            Command::Quit => self.quit = true,

            // --- motion / selection ---
            Command::Move(m) => self.with_doc(|d| edit::move_selections(d, m, page, false)),
            Command::Extend(m) => self.with_doc(|d| edit::move_selections(d, m, page, true)),
            Command::SelectAll => self.with_doc(|d| {
                let len = d.len_chars();
                d.selections = editor_core::Selections::single(Selection::new(0, len));
            }),
            Command::SelectWord => self.with_doc(edit::select_word),
            Command::SelectLine => self.with_doc(edit::select_line),

            // --- editing ---
            Command::InsertChar(c) => {
                let (pairs, indent) = (self.config.auto_pairs, self.config.auto_indent);
                let table = editor_core::PairTable::default();
                self.with_doc(|d| edit::insert_char_smart(d, c, &table, pairs, indent));
            }
            Command::InsertNewline => {
                let indent = self.config.auto_indent;
                let table = editor_core::PairTable::default();
                self.with_doc(|d| edit::insert_newline_smart(d, &table, indent));
            }
            Command::InsertText(s) => {
                self.with_doc(|d| edit::insert_text(d, &s, editor_core::GroupBreak::Force))
            }
            Command::DeleteBackward => {
                let pairs = self.config.auto_pairs;
                let table = editor_core::PairTable::default();
                self.with_doc(|d| edit::delete_backward_smart(d, &table, pairs));
            }
            Command::DeleteForward => self.with_doc(edit::delete_forward),
            Command::DeleteWordBackward => self.with_doc(edit::delete_word_backward),
            Command::DuplicateLine => self.with_doc(edit::duplicate_line),
            Command::MoveLineUp => self.with_doc(|d| edit::move_lines(d, -1)),
            Command::MoveLineDown => self.with_doc(|d| edit::move_lines(d, 1)),
            Command::ToggleComment => {
                let token = self
                    .editor
                    .active_document()
                    .and_then(|d| d.language.as_deref())
                    .map(line_comment_token)
                    .unwrap_or("//");
                self.with_doc(|d| edit::toggle_comment(d, token));
            }
            Command::Indent => self.with_doc(edit::indent),
            Command::Outdent => self.with_doc(edit::outdent),

            // --- multi-cursor ---
            Command::AddCursorAtNextMatch => self.add_cursor_next_match(),
            Command::AddCursorAbove => self.add_cursor_vertical(-1),
            Command::AddCursorBelow => self.add_cursor_vertical(1),
            Command::Paste(s) => {
                let text = if s.is_empty() {
                    self.clipboard.get()
                } else {
                    s
                };
                self.with_doc(|d| edit::insert_text(d, &text, editor_core::GroupBreak::Force))
            }

            // --- history ---
            Command::Undo => self.with_doc(|d| {
                edit::undo(d);
            }),
            Command::Redo => self.with_doc(|d| {
                edit::redo(d);
            }),

            // --- files / tabs ---
            Command::Save => self.save_or_save_as(),
            Command::SaveAs => self.open_save_as(),
            Command::NewFile => self.new_file(),
            Command::OpenFile(p) => self.open_path(&p),
            Command::CloseTab => self.request_close(self.editor.workspace.active_tab),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::GotoTab(i) => self.editor.workspace.focus_tab(i),

            // --- search ---
            Command::FindOpen => self.open_find(false),
            Command::ReplaceOpen => self.open_find(true),
            Command::FindNext => {
                toggle_and(&mut self.editor.find, |f| f.select_next());
                self.focus_current_match();
            }
            Command::FindPrev => {
                toggle_and(&mut self.editor.find, |f| f.select_prev());
                self.focus_current_match();
            }
            Command::ReplaceCurrent => self.replace_current(),
            Command::ReplaceAll => self.replace_all(),
            Command::ProjectSearch => self.open_search(),

            // --- clipboard ---
            Command::Copy => {
                if let Some(t) = self.selection_text() {
                    self.clipboard.set(t);
                }
            }
            Command::Cut => {
                if let Some(t) = self.selection_text() {
                    self.clipboard.set(t);
                    self.with_doc(edit::delete_backward);
                }
            }

            // --- language server ---
            Command::Hover => self.lsp_request(LspRequest::Hover),
            Command::GotoDefinition => self.lsp_request(LspRequest::Definition),
            Command::Completion => self.lsp_request(LspRequest::Completion),
            Command::RenameSymbol => self.open_rename(),
            Command::NextDiagnostic => self.goto_diagnostic(1),
            Command::PrevDiagnostic => self.goto_diagnostic(-1),
            Command::FindReferences => self.lsp_request(LspRequest::References),
            Command::DocumentSymbols => self.request_document_symbols(),
            Command::NextHunk => self.goto_hunk(1),
            Command::PrevHunk => self.goto_hunk(-1),

            // --- ui ---
            Command::ToggleSidebar => self.editor.sidebar_visible = !self.editor.sidebar_visible,
            Command::FocusSidebar => self.editor.focus = Focus::Sidebar,
            Command::FocusEditor => self.editor.focus = Focus::Editor,
            Command::Palette => self.open_palette(),
            Command::QuickOpen => self.open_quick_open(),
            Command::GotoLine => self.open_goto_line(),

            // --- terminal panel ---
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::NewTerminal => self.new_terminal(),
            Command::CloseTerminal => self.close_terminal(),
            Command::MinimizeTerminal => self.minimize_terminal(),
            Command::NextTerminal => {
                if self.panel.open {
                    self.panel.next();
                }
            }
            Command::PrevTerminal => {
                if self.panel.open {
                    self.panel.prev();
                }
            }

            // A plugin-contributed command referenced by id.
            Command::Run(id) => {
                if !self.registry.dispatch_command(&id, &mut self.editor) {
                    self.editor.status_message = Some(format!("Unknown command: {id}"));
                }
            }
        }

        // Broadcast any events queued by the edit, and run queued commands/opens.
        self.drain_workers();
    }

    /// Run `f` on the active document if there is one.
    fn with_doc<F: FnOnce(&mut Document)>(&mut self, f: F) {
        if let Some(d) = self.editor.active_document_mut() {
            f(d);
        }
    }

    /// Issue an LSP request for the primary cursor position, if one resolves. The three
    /// position-based requests (hover / definition / completion) share this lookup.
    fn lsp_request(&mut self, req: LspRequest) {
        if let Some((p, l, line, ch)) = self.lsp_position() {
            match req {
                LspRequest::Hover => self.lsp.request_hover(&p, &l, line, ch),
                LspRequest::Definition => self.lsp.request_definition(&p, &l, line, ch),
                LspRequest::Completion => self.lsp.request_completion(&p, &l, line, ch),
                LspRequest::References => self.lsp.request_references(&p, &l, line, ch),
            };
        }
    }

    /// Request the symbols in the active document (no cursor position needed).
    fn request_document_symbols(&mut self) {
        let info = self.editor.active_document().and_then(|d| {
            let path = d.path.clone()?;
            let lang = d.language.clone()?;
            Some((path, lang))
        });
        if let Some((path, lang)) = info {
            self.lsp.request_document_symbols(&path, &lang);
        }
    }

    /// Close a tab, prompting first if it has unsaved changes (plan §6).
    fn request_close(&mut self, tab: usize) {
        let dirty = self
            .editor
            .workspace
            .tabs
            .get(tab)
            .and_then(|&id| self.editor.workspace.documents.get(id))
            .map(|d| d.dirty)
            .unwrap_or(false);
        if dirty {
            self.editor.overlay = Some(crate::editor::Overlay::ConfirmClose { tab });
        } else {
            self.editor.workspace.close_tab(tab);
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        let n = self.editor.workspace.tabs.len();
        if n == 0 {
            return;
        }
        let cur = self.editor.workspace.active_tab as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.editor.workspace.focus_tab(next);
    }

    fn open_path(&mut self, path: &std::path::Path) {
        if path.is_dir() {
            self.editor.workspace.root = path.to_path_buf();
            return;
        }
        if let Some(id) = self.editor.workspace.find_by_path(path) {
            self.editor.workspace.focus_doc(id);
            return;
        }
        match files::load(path) {
            Ok(mut doc) => {
                doc.set_caret(0);
                let id = self.editor.workspace.open_document(doc);
                self.editor.emit(editor_plugin::event::Event::DidOpen(id));
                self.request_git_status(id);
            }
            Err(e) => {
                self.editor.status_message = Some(format!("Open failed: {e}"));
            }
        }
    }

    /// Jump the caret to the start of the next (`dir > 0`) or previous git hunk in the active
    /// document, wrapping around (plan §4.2 navigation). A hunk starts at a changed line whose
    /// predecessor is unchanged.
    fn goto_hunk(&mut self, dir: isize) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(hunks) = self.editor.git_hunks.get(&id) else {
            return;
        };
        if hunks.is_empty() {
            return;
        }
        let mut starts: Vec<usize> = hunks
            .keys()
            .copied()
            .filter(|&l| l == 0 || !hunks.contains_key(&(l - 1)))
            .collect();
        starts.sort_unstable();
        let Some(doc) = self.editor.workspace.documents.get(id) else {
            return;
        };
        let cur = doc.char_to_line(doc.selections.primary().head);
        let target = if dir > 0 {
            starts
                .iter()
                .copied()
                .find(|&l| l > cur)
                .unwrap_or(starts[0])
        } else {
            starts
                .iter()
                .rev()
                .copied()
                .find(|&l| l < cur)
                .unwrap_or_else(|| *starts.last().unwrap())
        };
        let off = doc.line_to_char(target);
        if let Some(d) = self.editor.workspace.documents.get_mut(id) {
            d.set_caret(off);
        }
        self.ensure_cursor_visible();
        self.editor.update_bracket_match();
    }

    /// Recompute a document's git change map off the main thread (plan §4.1). No-op when the
    /// gutter is disabled or the doc has no path.
    fn request_git_status(&self, id: editor_core::DocId) {
        if !self.config.git_gutter {
            return;
        }
        let root = self.editor.workspace.root.clone();
        if let Some(path) = self
            .editor
            .workspace
            .documents
            .get(id)
            .and_then(|d| d.path.clone())
        {
            crate::worker::spawn_git(root, path, self.worker_tx.clone());
        }
    }

    /// Kick a git recompute for every open document (startup / config reload).
    fn refresh_git_all(&self) {
        for id in self.editor.workspace.tabs.clone() {
            self.request_git_status(id);
        }
    }

    /// Save the active document, falling back to the Save As prompt when it has no path yet
    /// (plan §1.5 — resolves the old "Save As not yet wired" gap).
    fn save_or_save_as(&mut self) {
        let has_path = self
            .editor
            .active_document()
            .map(|d| d.path.is_some())
            .unwrap_or(false);
        if has_path {
            self.save_active();
        } else {
            self.open_save_as();
        }
    }

    /// Open the Save As overlay, seeded with the current path (if any).
    fn open_save_as(&mut self) {
        if self.editor.active_document().is_none() {
            return;
        }
        let initial = self
            .editor
            .active_document()
            .and_then(|d| d.path.as_ref())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput { buffer: initial });
    }

    /// Point the active document at `raw` (resolved against the project root when relative),
    /// refresh its language, and write it (plan §1.5).
    fn save_as_to(&mut self, raw: &str) {
        let raw = raw.trim();
        if raw.is_empty() {
            return;
        }
        let mut path = PathBuf::from(raw);
        if path.is_relative() {
            path = self.editor.workspace.root.join(path);
        }
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
            doc.path = Some(path.clone());
            doc.language = files::language_for(&path);
        }
        // Drop any stale highlighter so it re-creates for the (possibly new) language.
        self.editor.highlighters.remove(&id);
        self.save_active();
    }

    /// Open a fresh, empty, untitled buffer (plan §1.5).
    fn new_file(&mut self) {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        self.editor.workspace.open_document(doc);
        self.editor.focus = Focus::Editor;
    }

    fn save_active(&mut self) {
        // Read hygiene settings before borrowing the document (different `self` fields).
        let (trim, final_nl) = (
            self.config.trim_trailing_whitespace,
            self.config.insert_final_newline,
        );
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        let Some(path) = doc.path.clone() else {
            self.editor.status_message = Some("No path (Save As not yet wired)".into());
            return;
        };
        // On-save hygiene runs as an undoable Transaction before the write (plan §1.4).
        if trim || final_nl {
            edit::apply_save_hygiene(doc, trim, final_nl);
        }
        match files::save(doc, &path) {
            Ok(fp) => {
                doc.dirty = false;
                doc.deleted_on_disk = false;
                // Record the hash we just wrote so the watch echo is suppressed (plan §6).
                self.pending_self_writes.insert(path.clone(), fp.hash);
                doc.disk = fp;
                doc.history.break_group();
                self.editor.status_message = Some(format!("Saved {}", path.display()));
                self.editor.emit(editor_plugin::event::Event::DidSave(id));
            }
            Err(e) => {
                self.editor.status_message = Some(format!("Save failed: {e}"));
            }
        }
        // Refresh the git gutter against the just-written file (plan §4.1).
        self.request_git_status(id);
    }
}

/// Run `f` on the find state if it's open.
fn toggle_and<F: FnOnce(&mut FindState)>(find: &mut Option<FindState>, f: F) {
    if let Some(fs) = find {
        f(fs);
    }
}

/// A `file:line:col` label for a location picker row (plan §2.3).
fn location_label(loc: &editor_lsp::Location) -> String {
    let file = crate::lsp::path_from_uri(&loc.uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| loc.uri.clone());
    format!("{file}:{}:{}", loc.line + 1, loc.character + 1)
}

/// Convert an LSP `(line, utf16_char)` position to a char offset in `doc`.
pub(crate) fn lsp_pos_to_char(doc: &Document, line: u32, char16: u32) -> usize {
    let line = (line as usize).min(doc.len_lines().saturating_sub(1));
    let text = doc.line_text(line);
    let text = text.trim_end_matches(['\n', '\r']);
    let col = editor_lsp::position::utf16_to_char_col(text, char16);
    doc.line_to_char(line) + col
}

/// The line-comment token for a language id (used by `edit.toggleComment`).
fn line_comment_token(lang: &str) -> &'static str {
    match lang {
        "python" | "toml" | "yaml" | "shell" | "ruby" => "#",
        _ => "//",
    }
}

/// Build the keymap from defaults, then layer config overrides on top.
fn build_keymap(config: &crate::config::Config) -> Keymap {
    let mut km = Keymap::from_pairs(crate::commands::default_bindings().iter().copied());
    for (chord, id) in &config.keybindings {
        km.bind(chord, id);
    }
    km
}

/// Human label for a pending chord prefix (shown in the status bar).
fn chords_label(chords: &[Chord]) -> String {
    use crossterm::event::KeyCode;
    chords
        .iter()
        .map(|c| {
            let mut s = String::new();
            if c.ctrl {
                s.push_str("Ctrl+");
            }
            if c.alt {
                s.push_str("Alt+");
            }
            if c.shift {
                s.push_str("Shift+");
            }
            match c.code {
                KeyCode::Char(ch) => s.push(ch),
                other => s.push_str(&format!("{other:?}")),
            }
            s
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Directories to scan for external plugins: the user config dir and the project-local
/// `.lumina/plugins` folder.
fn external_plugin_dirs(root: &std::path::Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(pd) = directories::ProjectDirs::from("", "", "lumina") {
        dirs.push(pd.config_dir().join("plugins"));
    }
    dirs.push(root.join(".lumina").join("plugins"));
    dirs
}

/// Find the next occurrence of `needle` in `chars` at/after `from`, wrapping to the start.
fn find_next_occurrence(chars: &[char], needle: &[char], from: usize) -> Option<(usize, usize)> {
    let n = chars.len();
    let m = needle.len();
    if m == 0 || m > n {
        return None;
    }
    let span = n - m + 1; // number of valid start positions
    for off in 0..span {
        let i = (from + off) % span;
        if &chars[i..i + m] == needle {
            return Some((i, i + m));
        }
    }
    None
}

/// True if screen cell `(col, row)` falls within `rect`.
fn in_rect(rect: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// Map a key to an explorer command id when the sidebar is focused (keyboard parity).
fn sidebar_command(key: crossterm::event::KeyEvent) -> Option<&'static str> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Up => Some("explorer.up"),
        KeyCode::Down => Some("explorer.down"),
        KeyCode::Right => Some("explorer.expand"),
        KeyCode::Left => Some("explorer.collapse"),
        KeyCode::Enter => Some("explorer.activate"),
        _ => None,
    }
}

/// Resolve the CLI arg into (project root, optional file to open).
fn resolve_arg(arg: Option<String>) -> (PathBuf, Option<PathBuf>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match arg {
        None => (cwd, None),
        Some(a) => {
            let p = PathBuf::from(&a);
            if p.is_dir() {
                (p, None)
            } else if p.is_file() {
                let root = p.parent().map(|x| x.to_path_buf()).unwrap_or(cwd);
                (root, Some(p))
            } else {
                // Non-existent path: treat as a new file under cwd.
                (cwd, Some(p))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::Motion;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_file(contents: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut p = std::env::temp_dir();
        p.push(format!("lumina_test_{}_{}.txt", std::process::id(), n));
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn app_with(path: &std::path::Path) -> App {
        App::new(Some(path.to_string_lossy().into_owned())).unwrap()
    }

    #[test]
    fn opens_file_into_a_tab() {
        let path = temp_file("hello\nworld\n");
        let app = app_with(&path);
        assert_eq!(app.editor.workspace.tabs.len(), 1);
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "hello\nworld\n"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn type_undo_redo_roundtrip() {
        let path = temp_file("");
        let mut app = app_with(&path);
        app.dispatch(Command::InsertChar('h'));
        app.dispatch(Command::InsertChar('i'));
        assert_eq!(app.editor.active_document().unwrap().to_string(), "hi");
        app.dispatch(Command::Undo);
        assert_eq!(app.editor.active_document().unwrap().to_string(), "");
        app.dispatch(Command::Redo);
        assert_eq!(app.editor.active_document().unwrap().to_string(), "hi");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn auto_pairs_multi_cursor_dispatch() {
        // plan §1.1 acceptance: three cursors, typing `(` yields three `()` with a caret
        // inside each, routed through the real Command dispatch (auto_pairs on by default).
        let path = temp_file("a\nb\nc\n");
        let mut app = app_with(&path);
        {
            let doc = app.editor.active_document_mut().unwrap();
            doc.selections = editor_core::Selections::from_iter([
                Selection::caret(0),
                Selection::caret(2),
                Selection::caret(4),
            ]);
        }
        app.dispatch(Command::InsertChar('('));
        let doc = app.editor.active_document().unwrap();
        assert_eq!(doc.to_string(), "()a\n()b\n()c\n");
        assert_eq!(doc.selections.len(), 3);
        for s in doc.selections.ranges() {
            assert!(s.is_empty(), "caret stays a caret");
            assert_eq!(doc.text.char(s.head), ')', "caret sits before the closer");
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn auto_indent_newline_dispatch() {
        // Auto-indent in isolation (auto-pairs off so the braces aren't auto-closed): a
        // newline after `{` indents the fresh line, and typing `}` on it dedents (plan §1.2).
        let path = temp_file("");
        let mut app = app_with(&path);
        app.config.auto_pairs = false;
        for c in "fn f() {".chars() {
            app.dispatch(Command::InsertChar(c));
        }
        app.dispatch(Command::InsertNewline);
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "fn f() {\n    "
        );
        app.dispatch(Command::InsertChar('}'));
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "fn f() {\n}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn new_file_opens_clean_untitled_buffer() {
        let path = temp_file("hello\n");
        let mut app = app_with(&path);
        let before = app.editor.workspace.tabs.len();
        app.dispatch(Command::NewFile);
        assert_eq!(app.editor.workspace.tabs.len(), before + 1);
        let doc = app.editor.active_document().unwrap();
        assert_eq!(doc.to_string(), "");
        assert!(!doc.dirty, "a fresh buffer is not dirty");
        assert!(doc.path.is_none(), "untitled has no path");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_as_writes_new_path_and_updates_document() {
        let src = temp_file("x\n");
        let mut app = app_with(&src);
        let target = std::env::temp_dir().join(format!("lumina_saveas_{}.rs", std::process::id()));
        std::fs::remove_file(&target).ok();
        // New untitled buffer with content, then Save As to the target path.
        app.dispatch(Command::NewFile);
        app.dispatch(Command::InsertText("fn main() {}".into()));
        app.save_as_to(&target.to_string_lossy());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {}");
        let doc = app.editor.active_document().unwrap();
        assert_eq!(doc.path.as_deref(), Some(target.as_path()));
        assert_eq!(doc.language.as_deref(), Some("rust")); // .rs → language picked up
        assert!(!doc.dirty, "clean after save");
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&target).ok();
    }

    #[test]
    fn new_file_and_save_as_key_bindings_resolve() {
        use crossterm::event::KeyModifiers;
        let path = temp_file("hello\n");
        let mut app = app_with(&path);
        let before = app.editor.workspace.tabs.len();
        // ctrl+n → new untitled buffer.
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.editor.workspace.tabs.len(), before + 1);
        assert!(app.editor.active_document().unwrap().path.is_none());
        // ctrl+k ctrl+s (multi-chord) → Save As overlay, and does NOT clobber plain ctrl+s.
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(
            app.editor.overlay,
            Some(crate::editor::Overlay::SaveAsInput { .. })
        ));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_untitled_falls_back_to_save_as_prompt() {
        let path = temp_file("hello\n");
        let mut app = app_with(&path);
        app.dispatch(Command::NewFile); // untitled, no path
        app.dispatch(Command::Save); // should open the Save As overlay, not error
        assert!(matches!(
            app.editor.overlay,
            Some(crate::editor::Overlay::SaveAsInput { .. })
        ));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn bracket_match_precomputed_into_state() {
        let path = temp_file("a(b)c\n");
        let mut app = app_with(&path);
        // Caret on the '(' at offset 1 → highlight it and its partner ')'.
        app.editor.active_document_mut().unwrap().set_caret(1);
        app.editor.update_bracket_match();
        assert_eq!(app.editor.bracket_match, Some((1, 3)));
        // Caret just after the ')' (offset 4) → matches via the bracket before the caret.
        app.editor.active_document_mut().unwrap().set_caret(4);
        app.editor.update_bracket_match();
        assert_eq!(app.editor.bracket_match, Some((3, 1)));
        // Caret not adjacent to any bracket → None.
        app.editor.active_document_mut().unwrap().set_caret(0);
        app.editor.update_bracket_match();
        assert_eq!(app.editor.bracket_match, None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_trims_trailing_whitespace_when_enabled() {
        let path = temp_file("foo   \nbar\t\n");
        let mut app = app_with(&path);
        app.config.trim_trailing_whitespace = true;
        app.dispatch(Command::Save);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo\nbar\n");
        assert!(!app.editor.active_document().unwrap().dirty);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_hygiene_preserves_crlf_on_disk() {
        let path = temp_file("foo  \r\nbar");
        let mut app = app_with(&path);
        app.config.trim_trailing_whitespace = true;
        app.config.insert_final_newline = true;
        app.dispatch(Command::Save);
        // Trimmed + final newline added, but the CRLF line ending is preserved.
        assert_eq!(std::fs::read(&path).unwrap(), b"foo\r\nbar\r\n");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn edit_then_save_persists_atomically() {
        let path = temp_file("abc");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.dispatch(Command::InsertText("XYZ".into()));
        app.dispatch(Command::Save);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "abcXYZ");
        assert!(!app.editor.active_document().unwrap().dirty);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn large_file_scrolls_to_follow_cursor() {
        let body: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let path = temp_file(&body);
        let mut app = app_with(&path);
        app.page_height = 40;
        // Jump to end of document; the viewport must scroll to keep the cursor visible.
        app.dispatch(Command::Move(Motion::DocEnd));
        app.ensure_cursor_visible();
        let doc = app.editor.active_document().unwrap();
        let head_line = doc.char_to_line(doc.selections.primary().head);
        let top = doc.view.scroll_line;
        assert!(
            head_line >= top && head_line < top + 40,
            "cursor off-screen"
        );
        assert!(top > 0, "did not scroll for a large file");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn quit_sets_flag() {
        let path = temp_file("x");
        let mut app = app_with(&path);
        app.dispatch(Command::Quit);
        assert!(app.quit);
        std::fs::remove_file(&path).ok();
    }

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
    use ratatui::layout::Rect;

    fn temp_dir_with_files() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("lumina_dir_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha").unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), "beta").unwrap();
        dir
    }

    #[test]
    fn explorer_is_registered_and_lists_files() {
        let dir = temp_dir_with_files();
        let app = app_with(&dir);
        // The explorer plugin populated its sidebar panel at activation.
        let panel = app.editor.panels.get("explorer.tree").expect("no panel");
        let names: Vec<String> = panel
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(names.iter().any(|t| t.contains("a.txt")));
        assert!(names.iter().any(|t| t.contains("sub")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explorer_opens_a_file_on_activate() {
        let dir = temp_dir_with_files();
        let mut app = app_with(&dir);
        let payload = dir.join("a.txt").to_string_lossy().into_owned();
        app.registry
            .activate_panel_row("explorer.tree", &payload, &mut app.editor);
        app.drain_workers();
        assert_eq!(app.editor.workspace.tabs.len(), 1);
        assert_eq!(app.editor.active_document().unwrap().to_string(), "alpha");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dirty_close_prompts_then_discards() {
        let path = temp_file("data");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.dispatch(Command::InsertChar('!'));
        assert!(app.editor.active_document().unwrap().dirty);
        app.dispatch(Command::CloseTab);
        // A dirty tab prompts instead of closing.
        assert!(app.editor.overlay.is_some());
        assert_eq!(app.editor.workspace.tabs.len(), 1);
        // Discard closes it without saving.
        app.overlay_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(app.editor.overlay.is_none());
        assert_eq!(app.editor.workspace.tabs.len(), 0);
        // The file on disk was not modified.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
        std::fs::remove_file(&path).ok();
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn click_places_cursor() {
        let path = temp_file("hello\nworld");
        let mut app = app_with(&path);
        app.regions.editor = Rect::new(0, 0, 80, 24); // gutter is 4 for a 2-line doc
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 6, 0));
        // col 6 = text col 2 on line 0 -> char offset 2.
        assert_eq!(
            app.editor
                .active_document()
                .unwrap()
                .selections
                .primary()
                .head,
            2
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn double_click_selects_word() {
        let path = temp_file("foo bar baz");
        let mut app = app_with(&path);
        app.regions.editor = Rect::new(0, 0, 80, 24);
        // Two clicks at the same position within the double-click window.
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 8, 0)); // inside "bar"
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 8, 0));
        let sel = app.editor.active_document().unwrap().selections.primary();
        assert_eq!((sel.from(), sel.to()), (4, 7)); // "bar"
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rust_file_gets_syntax_highlighting() {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!("lumina_hl_{}_{}.rs", std::process::id(), n));
        std::fs::write(&path, "fn main() {\n    let x = 42;\n}\n").unwrap();
        let mut app = app_with(&path);
        app.page_height = 24;
        app.editor.update_highlights(app.page_height);
        let id = app.editor.workspace.active_doc().unwrap();
        let hl = app.editor.highlighters.get(&id).expect("no highlighter");
        assert!(
            hl.line_spans(0)
                .iter()
                .any(|s| s.capture.starts_with("keyword")),
            "expected a keyword span on the fn line"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn find_highlights_and_cycles() {
        let path = temp_file("foo bar foo baz foo");
        let mut app = app_with(&path);
        app.dispatch(Command::FindOpen);
        for c in "foo".chars() {
            app.find_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let find = app.editor.find.as_ref().unwrap();
        assert_eq!(find.matches.len(), 3);
        // Cursor started at 0 -> current match is the first.
        assert_eq!(find.current_match(), Some((0, 3)));
        app.find_key(KeyEvent::from(KeyCode::Enter)); // next
        assert_eq!(
            app.editor.find.as_ref().unwrap().current_match(),
            Some((8, 11))
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replace_all_is_one_undo() {
        let path = temp_file("cat cat cat");
        let mut app = app_with(&path);
        app.dispatch(Command::ReplaceOpen);
        for c in "cat".chars() {
            app.find_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.find_key(KeyEvent::from(KeyCode::Tab)); // focus replace field
        for c in "dog".chars() {
            app.find_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.replace_all();
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "dog dog dog"
        );
        // One undo reverts the whole replace-all.
        app.editor.find = None;
        app.dispatch(Command::Undo);
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "cat cat cat"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn palette_lists_builtin_and_plugin_commands() {
        let dir = temp_dir_with_files();
        let mut app = app_with(&dir);
        app.open_palette();
        let picker = app.editor.picker.as_ref().unwrap();
        let labels: Vec<&str> = picker.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"File: Save"));
        // Plugin-contributed command titles are present too (explorer).
        assert!(labels.iter().any(|l| l.starts_with("Explorer:")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn goto_line_moves_cursor() {
        let path = temp_file("l0\nl1\nl2\nl3");
        let mut app = app_with(&path);
        app.open_goto_line();
        for c in "3".chars() {
            app.picker_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.picker_key(KeyEvent::from(KeyCode::Enter));
        let doc = app.editor.active_document().unwrap();
        assert_eq!(doc.char_to_line(doc.selections.primary().head), 2); // line 3 (0-based 2)
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn keymap_ctrl_s_saves() {
        use crossterm::event::KeyModifiers;
        let path = temp_file("abc");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.dispatch(Command::InsertChar('!'));
        assert!(app.editor.active_document().unwrap().dirty);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(!app.editor.active_document().unwrap().dirty);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc!");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn keymap_typed_char_inserts() {
        let path = temp_file("");
        let mut app = app_with(&path);
        app.on_key(KeyEvent::from(KeyCode::Char('x')));
        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.editor.active_document().unwrap().to_string(), "xy");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn clipboard_copy_paste() {
        let path = temp_file("hello");
        let mut app = app_with(&path);
        app.dispatch(Command::SelectAll);
        app.dispatch(Command::Copy);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.dispatch(Command::Paste(String::new()));
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "hellohello"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn workspace_edit_applies_rename_across_occurrences() {
        let path = temp_file("let foo = foo + 1;");
        let mut app = app_with(&path);
        let uri = crate::lsp::uri_for(&path);
        let edit = editor_lsp::WorkspaceEdit {
            changes: vec![(
                uri,
                vec![
                    editor_lsp::TextEdit {
                        start_line: 0,
                        start_char16: 4,
                        end_line: 0,
                        end_char16: 7,
                        new_text: "bar".into(),
                    },
                    editor_lsp::TextEdit {
                        start_line: 0,
                        start_char16: 10,
                        end_line: 0,
                        end_char16: 13,
                        new_text: "bar".into(),
                    },
                ],
            )],
        };
        app.apply_workspace_edit(edit);
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "let bar = bar + 1;"
        );
    }

    #[test]
    fn completion_replaces_typed_prefix() {
        let path = temp_file("pri");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.insert_completion("println!");
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "println!"
        );
    }

    fn ci(label: &str, kind: u8) -> editor_lsp::CompletionItem {
        editor_lsp::CompletionItem {
            label: label.to_string(),
            detail: None,
            insert_text: label.to_string(),
            kind: Some(kind),
        }
    }

    #[test]
    fn completion_popup_navigates_and_accepts() {
        let path = temp_file("pr");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.open_completion(vec![ci("print", 3), ci("println", 3), ci("procedure", 3)]);
        assert!(app.editor.completion.is_some());
        // Down selects the 2nd item ("println"); Enter accepts and replaces the typed "pr".
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.editor.completion.is_none());
        assert_eq!(app.editor.active_document().unwrap().to_string(), "println");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn completion_filters_as_you_type_then_dismisses() {
        let path = temp_file("p");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.open_completion(vec![ci("print", 3), ci("procedure", 3), ci("foo", 3)]);
        assert_eq!(app.editor.completion.as_ref().unwrap().filtered.len(), 2); // print, procedure
        app.on_key(KeyEvent::from(KeyCode::Char('r'))); // "pr"
        assert_eq!(app.editor.active_document().unwrap().to_string(), "pr");
        assert_eq!(app.editor.completion.as_ref().unwrap().filtered.len(), 2);
        app.on_key(KeyEvent::from(KeyCode::Char('i'))); // "pri" → only print
        assert_eq!(app.editor.completion.as_ref().unwrap().filtered.len(), 1);
        // A non-identifier char leaves the word and dismisses the popup.
        app.on_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.editor.completion.is_none());
        std::fs::remove_file(&path).ok();
    }

    fn diag(line: u32, sc: u32, el: u32, ec: u32, msg: &str) -> editor_lsp::Diagnostic {
        editor_lsp::Diagnostic {
            line,
            start_char16: sc,
            end_line: el,
            end_char16: ec,
            severity: editor_lsp::Severity::Error,
            message: msg.to_string(),
        }
    }

    #[test]
    fn hunk_navigation_cycles_over_change_starts() {
        let path = temp_file("a\nb\nc\nd\ne\nf\ng\n");
        let mut app = app_with(&path);
        let id = app.editor.workspace.active_doc().unwrap();
        let mut hunks = crate::git::LineStatuses::new();
        hunks.insert(1, crate::git::LineStatus::Modified);
        hunks.insert(2, crate::git::LineStatus::Modified); // same hunk as line 1
        hunks.insert(5, crate::git::LineStatus::Added); // separate hunk
        app.editor.git_hunks.insert(id, hunks);
        let line = |a: &App| {
            let d = a.editor.active_document().unwrap();
            d.char_to_line(d.selections.primary().head)
        };
        app.dispatch(Command::NextHunk);
        assert_eq!(line(&app), 1);
        app.dispatch(Command::NextHunk);
        assert_eq!(line(&app), 5);
        app.dispatch(Command::NextHunk); // wraps to the first hunk
        assert_eq!(line(&app), 1);
        app.dispatch(Command::PrevHunk); // wraps to the last hunk
        assert_eq!(line(&app), 5);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn git_status_message_stored_per_doc() {
        let path = temp_file("a\nb\n");
        let mut app = app_with(&path);
        let id = app.editor.workspace.active_doc().unwrap();
        let mut statuses = crate::git::LineStatuses::new();
        statuses.insert(1, crate::git::LineStatus::Modified);
        app.worker_tx
            .send(crate::worker::WorkerMsg::GitStatus {
                path: path.clone(),
                statuses,
            })
            .unwrap();
        app.drain_workers();
        assert_eq!(
            app.editor.git_hunks.get(&id).and_then(|m| m.get(&1)),
            Some(&crate::git::LineStatus::Modified)
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn references_open_picker_and_jump() {
        let path = temp_file("aaa\nbbb\nccc\n");
        let mut app = app_with(&path);
        let uri = crate::lsp::uri_for(&path);
        let loc = editor_lsp::Location {
            uri,
            line: 2,
            character: 0,
            end_line: 2,
            end_character: 1,
        };
        app.handle_lsp_event(crate::lsp::LspEvent::References(vec![loc]));
        assert!(matches!(
            app.editor.picker.as_ref().map(|p| p.kind),
            Some(crate::picker::PickerKind::Locations)
        ));
        assert_eq!(app.editor.nav_locations.len(), 1);
        // Accepting the row jumps the caret to line 3 (offset 8).
        app.picker_key(KeyEvent::from(KeyCode::Enter));
        let doc = app.editor.active_document().unwrap();
        assert_eq!(doc.char_to_line(doc.selections.primary().head), 2);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn diagnostic_nav_and_caret_message() {
        let path = temp_file("aaa\nbbb\nccc\n");
        let mut app = app_with(&path);
        let id = app.editor.workspace.active_doc().unwrap();
        app.editor.diagnostics.insert(
            id,
            vec![diag(0, 0, 0, 1, "first"), diag(2, 0, 2, 1, "third")],
        );
        // Caret at 0 covers the first diagnostic; its message renders at the caret.
        assert_eq!(app.diagnostic_at_caret().map(|(_, m)| m), Some("first"));
        // Next jumps to the line-3 diagnostic (offset 8).
        app.dispatch(Command::NextDiagnostic);
        assert_eq!(
            app.editor
                .active_document()
                .unwrap()
                .selections
                .primary()
                .head,
            8
        );
        assert_eq!(app.diagnostic_at_caret().map(|(_, m)| m), Some("third"));
        // Next past the last wraps to the first; Prev from there wraps to the last.
        app.dispatch(Command::NextDiagnostic);
        assert_eq!(
            app.editor
                .active_document()
                .unwrap()
                .selections
                .primary()
                .head,
            0
        );
        app.dispatch(Command::PrevDiagnostic);
        assert_eq!(
            app.editor
                .active_document()
                .unwrap()
                .selections
                .primary()
                .head,
            8
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn completion_esc_dismisses_without_editing() {
        let path = temp_file("pr");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.open_completion(vec![ci("print", 3)]);
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.editor.completion.is_none());
        assert_eq!(app.editor.active_document().unwrap().to_string(), "pr");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_file_change_triggers_hot_reload() {
        let path = temp_file("");
        let mut app = app_with(&path);
        // Point the watched config path at an arbitrary file and simulate a change event.
        let cfg = std::env::temp_dir().join(format!("lumina_cfg_{}.toml", std::process::id()));
        app.config_path = Some(cfg.clone());
        app.editor.status_message = None;
        app.on_disk_changed(&cfg);
        assert_eq!(
            app.editor.status_message.as_deref(),
            Some("Configuration reloaded"),
            "a change to the config file should hot-reload it"
        );
    }

    #[test]
    fn config_remaps_key() {
        use crossterm::event::KeyModifiers;
        let path = temp_file("");
        let mut app = app_with(&path);
        // Rebind ctrl+s to select-all, then press it.
        app.config
            .keybindings
            .push(("ctrl+s".into(), "edit.selectAll".into()));
        app.keymap = build_keymap(&app.config);
        app.dispatch(Command::InsertText("abc".into()));
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let sel = app.editor.active_document().unwrap().selections.primary();
        assert_eq!((sel.from(), sel.to()), (0, 3));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn external_clean_reload_follows_cursor() {
        let path = temp_file("a\nb\nTARGET\nd");
        let mut app = app_with(&path);
        // Put the cursor on the TARGET line.
        let off = "a\nb\n".chars().count();
        app.editor.active_document_mut().unwrap().set_caret(off);
        // Another process inserts two lines above.
        std::fs::write(&path, "a\nX\nY\nb\nTARGET\nd").unwrap();
        app.on_disk_changed(&path);
        let doc = app.editor.active_document().unwrap();
        assert_eq!(doc.to_string(), "a\nX\nY\nb\nTARGET\nd");
        assert!(!doc.dirty);
        assert!(doc.externally_reloaded);
        let line = doc.char_to_line(doc.selections.primary().head);
        assert_eq!(doc.line_text(line).trim_end(), "TARGET");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn external_change_on_dirty_flags_conflict() {
        let path = temp_file("original");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.dispatch(Command::InsertChar('!')); // now dirty
        std::fs::write(&path, "changed by someone else").unwrap();
        app.on_disk_changed(&path);
        let doc = app.editor.active_document().unwrap();
        assert!(doc.external_conflict.is_some());
        assert_eq!(doc.to_string(), "original!"); // NOT clobbered
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn own_save_echo_is_suppressed() {
        let path = temp_file("hello");
        let mut app = app_with(&path);
        app.dispatch(Command::Move(Motion::DocEnd));
        app.dispatch(Command::InsertText("!".into()));
        app.save_active(); // records pending_self_write
                           // The watcher echoes our own write back:
        app.on_disk_changed(&path);
        let doc = app.editor.active_document().unwrap();
        assert!(
            !doc.externally_reloaded,
            "own save should not trigger a reload"
        );
        assert_eq!(doc.to_string(), "hello!");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn project_search_finds_and_opens() {
        let dir = temp_dir_with_files();
        std::fs::write(dir.join("a.txt"), "find_me on this line\nother").unwrap();
        let mut app = app_with(&dir);
        app.open_search();
        for c in "find_me".chars() {
            app.search_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.search_key(KeyEvent::from(KeyCode::Enter)); // run
                                                        // Drain the worker channel until the search completes (bounded, with backoff).
        for _ in 0..200 {
            app.drain_workers();
            if app.search().map(|s| !s.running).unwrap_or(true) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app
            .search()
            .unwrap()
            .results
            .iter()
            .any(|h| h.text.contains("find_me")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_d_selects_word_then_adds_next_match() {
        let path = temp_file("foo bar foo baz foo");
        let mut app = app_with(&path);
        app.dispatch(Command::AddCursorAtNextMatch); // select "foo" under cursor
        assert_eq!(app.editor.active_document().unwrap().selections.len(), 1);
        let sel = app.editor.active_document().unwrap().selections.primary();
        assert_eq!((sel.from(), sel.to()), (0, 3));
        app.dispatch(Command::AddCursorAtNextMatch); // add next "foo"
        assert_eq!(app.editor.active_document().unwrap().selections.len(), 2);
        app.dispatch(Command::AddCursorAtNextMatch); // third "foo"
        assert_eq!(app.editor.active_document().unwrap().selections.len(), 3);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn multi_cursor_typing_edits_all() {
        let path = temp_file("foo bar foo baz foo");
        let mut app = app_with(&path);
        for _ in 0..3 {
            app.dispatch(Command::AddCursorAtNextMatch);
        }
        // Replace each selected "foo" by typing.
        app.dispatch(Command::InsertText("X".into()));
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "X bar X baz X"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_cursor_below_creates_two_carets() {
        let path = temp_file("aaa\nbbb\nccc");
        let mut app = app_with(&path);
        app.editor.active_document_mut().unwrap().set_caret(1); // col 1 line 0
        app.dispatch(Command::AddCursorBelow);
        let doc = app.editor.active_document().unwrap();
        assert_eq!(doc.selections.len(), 2);
        // Second caret is on line 1 at the same column.
        assert!(doc
            .selections
            .ranges()
            .iter()
            .any(|s| doc.char_to_line(s.head) == 1));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn alt_click_adds_cursor() {
        use crossterm::event::KeyModifiers;
        let path = temp_file("hello\nworld");
        let mut app = app_with(&path);
        app.regions.editor = Rect::new(0, 0, 80, 24);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 1,
            modifiers: KeyModifiers::ALT,
        });
        assert_eq!(app.editor.active_document().unwrap().selections.len(), 2);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn theme_toggles() {
        let path = temp_file("x");
        let mut app = app_with(&path);
        let was_dark = app.theme.is_dark();
        app.exec_id("view.toggleTheme");
        assert_ne!(app.theme.is_dark(), was_dark);
        std::fs::remove_file(&path).ok();
    }

    /// Create a project dir containing a `.lumina/plugins/<id>` plugin and a file to open.
    fn temp_project_with_plugin(
        id: &str,
        manifest: &str,
        script: &str,
        file_contents: &str,
    ) -> (PathBuf, PathBuf) {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("lumina_plugin_{}_{}", std::process::id(), n));
        let pdir = dir.join(".lumina").join("plugins").join(id);
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("plugin.toml"), manifest).unwrap();
        std::fs::write(pdir.join("main.rhai"), script).unwrap();
        let file = dir.join("doc.txt");
        std::fs::write(&file, file_contents).unwrap();
        (dir, file)
    }

    #[test]
    fn external_plugin_registers_and_edits_through_host() {
        let manifest = "id = \"shout\"\ncapabilities = [\"edit\"]\n\
                        [[commands]]\nid = \"shout.line\"\ntitle = \"Shout\"\n";
        let script = "fn on_command(id, ctx) { \
                      [ #{ action: \"replace_line\", text: ctx.line_text.to_upper() } ] }";
        let (dir, file) = temp_project_with_plugin("shout", manifest, script, "hello world");
        let mut app = app_with(&file);
        // The plugin registered its command through the same registry as built-ins.
        assert!(app.registry.command_ids().any(|c| c == "shout.line"));
        // Running it edits the buffer via a transaction (undoable).
        app.exec_id("shout.line");
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "HELLO WORLD"
        );
        app.dispatch(Command::Undo);
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "hello world"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capability_gating_blocks_ungranted_edit() {
        // Same plugin, but WITHOUT the "edit" capability → the edit action is dropped.
        let manifest = "id = \"shout\"\ncapabilities = []\n\
                        [[commands]]\nid = \"shout.line\"\ntitle = \"Shout\"\n";
        let script = "fn on_command(id, ctx) { \
                      [ #{ action: \"replace_line\", text: ctx.line_text.to_upper() } ] }";
        let (dir, file) = temp_project_with_plugin("shout", manifest, script, "hello world");
        let mut app = app_with(&file);
        app.exec_id("shout.line");
        assert_eq!(
            app.editor.active_document().unwrap().to_string(),
            "hello world",
            "plugin without the edit capability must not modify the buffer"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn external_plugin_draws_a_panel() {
        let manifest = "id = \"insp\"\ncapabilities = [\"ui\"]\n\
                        [[panels]]\nid = \"insp.panel\"\ntitle = \"Inspector\"\nlocation = \"sidebar\"\n";
        let script = "fn render_panel(id, ctx) { [ \"cursor line: \" + ctx.cursor_line ] }";
        let (dir, file) = temp_project_with_plugin("insp", manifest, script, "a\nb\nc");
        let mut app = app_with(&file);
        assert!(app.registry.panel_ids().any(|p| p == "insp.panel"));
        app.registry.render_panel("insp.panel", &mut app.editor);
        let panel = app.editor.panels.get("insp.panel").expect("panel not set");
        assert!(panel.lines[0].spans[0].text.contains("cursor line:"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wheel_scrolls_viewport() {
        let body: String = (0..100).map(|i| format!("l{i}\n")).collect();
        let path = temp_file(&body);
        let mut app = app_with(&path);
        app.regions.editor = Rect::new(0, 0, 80, 24);
        app.on_mouse(mouse(MouseEventKind::ScrollDown, 10, 10));
        assert_eq!(app.editor.active_document().unwrap().view.scroll_line, 3);
        app.on_mouse(mouse(MouseEventKind::ScrollUp, 10, 10));
        assert_eq!(app.editor.active_document().unwrap().view.scroll_line, 0);
        std::fs::remove_file(&path).ok();
    }

    // ---- rendering -----------------------------------------------------------

    fn render_to_string(app: &mut App, w: u16, h: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    #[test]
    fn renders_editor_with_all_decorations() {
        // A .rs file (so syntax highlighting runs) with a tab, a wide char, and a
        // repeated word to exercise selection, multi-cursor, find and diagnostics paths.
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!("lumina_render_{}_{}.rs", std::process::id(), n));
        std::fs::write(&path, "fn foo() {\n\tlet w = foo; // 世界\n    foo();\n}\n").unwrap();
        let mut app = app_with(&path);
        app.page_height = 12;
        app.editor.update_highlights(app.page_height);

        // Non-empty selection + a secondary caret (exercises selection-bg + secondary-cursor).
        app.editor.active_document_mut().unwrap().set_caret(0);
        app.dispatch(Command::AddCursorBelow);
        app.dispatch(Command::SelectWord);

        // Active find with matches (exercises the match-highlight path).
        app.dispatch(Command::FindOpen);
        for c in "foo".chars() {
            app.find_key(KeyEvent::from(KeyCode::Char(c)));
        }

        // A diagnostic on line 0 (exercises the gutter marker + underline).
        let id = app.editor.workspace.active_doc().unwrap();
        app.editor.diagnostics.insert(
            id,
            vec![editor_lsp::Diagnostic {
                line: 0,
                start_char16: 3,
                end_line: 0,
                end_char16: 6,
                severity: editor_lsp::Severity::Error,
                message: String::new(),
            }],
        );

        // Viewport taller than the 4-line doc → past-EOF tildes render too.
        let text = render_to_string(&mut app, 48, 12);
        assert!(text.contains('~'), "expected past-EOF tildes below the doc");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn renders_welcome_when_no_document_is_open() {
        let path = temp_file("x");
        let mut app = app_with(&path);
        app.dispatch(Command::CloseTab); // close the only (clean) tab
        assert!(app.editor.active_document().is_none());
        let text = render_to_string(&mut app, 40, 10);
        assert!(text.contains("lumina"), "welcome screen shows the app name");
        std::fs::remove_file(&path).ok();
    }

    // ---- mouse routing -------------------------------------------------------

    #[test]
    fn middle_click_closes_a_tab() {
        let p1 = temp_file("one");
        let p2 = temp_file("two");
        let mut app = app_with(&p1);
        app.open_path(&p2);
        assert_eq!(app.editor.workspace.tabs.len(), 2);
        app.regions.tabs = Rect::new(0, 0, 80, 1);
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Middle), 2, 0));
        assert_eq!(app.editor.workspace.tabs.len(), 1);
        std::fs::remove_file(&p1).ok();
        std::fs::remove_file(&p2).ok();
    }

    #[test]
    fn left_drag_extends_selection() {
        let path = temp_file("hello world");
        let mut app = app_with(&path);
        app.regions.editor = Rect::new(0, 0, 80, 24);
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 0)); // set drag anchor
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 12, 0)); // extend
        let sel = app.editor.active_document().unwrap().selections.primary();
        assert_ne!(
            sel.from(),
            sel.to(),
            "drag should build a non-empty selection"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tab_click_then_drag_reorders() {
        let p1 = temp_file("one");
        let p2 = temp_file("two");
        let mut app = app_with(&p1);
        app.open_path(&p2);
        app.regions.tabs = Rect::new(0, 0, 80, 1);
        // Press a tab (arms the drag), then drag along the bar (reorders if over a new tab).
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 0));
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 0));
        assert_eq!(app.editor.workspace.tabs.len(), 2); // reorder never drops a tab
        std::fs::remove_file(&p1).ok();
        std::fs::remove_file(&p2).ok();
    }

    #[test]
    fn mouse_up_clears_drag_state() {
        let path = temp_file("hello");
        let mut app = app_with(&path);
        app.regions.editor = Rect::new(0, 0, 80, 24);
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 0));
        app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 0));
        assert!(app.drag_anchor.is_none());
        assert!(app.tab_drag.is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn sidebar_click_focuses_sidebar() {
        let dir = temp_dir_with_files();
        let mut app = app_with(&dir);
        app.regions.sidebar = Some(Rect::new(0, 0, 20, 24));
        app.regions.editor = Rect::new(20, 0, 60, 24);
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 1));
        assert_eq!(app.editor.focus, Focus::Sidebar);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sidebar_click_hits_the_row_under_the_cursor() {
        use ratatui::{backend::TestBackend, Terminal};
        let dir = temp_dir_with_files();
        let mut app = app_with(&dir);
        app.editor.focus = Focus::Sidebar;

        // Render a real frame so `Regions` reflect the laid-out sidebar, including the
        // " EXPLORER " title row that the block reserves above the panel content.
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        // Locate the screen row where the `sub` directory is actually drawn.
        let sidebar = app.regions.sidebar.expect("sidebar should be visible");
        let buf = terminal.backend().buffer();
        let mut sub_row = None;
        for y in sidebar.y..(sidebar.y + sidebar.height) {
            let mut line = String::new();
            for x in sidebar.x..(sidebar.x + sidebar.width) {
                line.push_str(buf[(x, y)].symbol());
            }
            if line.contains("sub") {
                sub_row = Some(y);
                break;
            }
        }
        let sub_row = sub_row.expect("`sub` directory should be visible in the sidebar");

        // Click exactly where `sub` is drawn. It must toggle that directory open — not
        // open the file rendered on the line below it.
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            sidebar.x + 2,
            sub_row,
        ));
        app.drain_workers();

        assert_eq!(
            app.editor.workspace.tabs.len(),
            0,
            "clicking `sub` must not open the file on the row below it",
        );
        let panel = app.editor.panels.get("explorer.tree").unwrap();
        let names: Vec<String> = panel
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(
            names.iter().any(|t| t.contains("b.txt")),
            "clicking `sub` should expand it to reveal b.txt",
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- key routing ---------------------------------------------------------

    #[test]
    fn modal_keys_route_to_active_modal() {
        let path = temp_file("hello world");
        let mut app = app_with(&path);
        // find
        app.dispatch(Command::FindOpen);
        assert!(app.editor.find.is_some());
        app.on_key(KeyEvent::from(KeyCode::Char('h')));
        app.on_key(KeyEvent::from(KeyCode::Esc));
        // picker
        app.open_palette();
        assert!(app.editor.picker.is_some());
        app.on_key(KeyEvent::from(KeyCode::Esc));
        // search
        app.open_search();
        assert!(app.search.is_some());
        app.on_key(KeyEvent::from(KeyCode::Esc));
        // overlay (confirm-close prompt on a dirty tab)
        app.dispatch(Command::Move(Motion::DocEnd));
        app.dispatch(Command::InsertChar('!'));
        app.dispatch(Command::CloseTab);
        assert!(app.editor.overlay.is_some());
        app.on_key(KeyEvent::from(KeyCode::Esc));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn sidebar_keys_drive_explorer_then_escape() {
        let dir = temp_dir_with_files();
        let mut app = app_with(&dir);
        app.editor.focus = Focus::Sidebar;
        // Arrow/enter keys map to explorer commands via the sidebar keymap.
        app.on_key(KeyEvent::from(KeyCode::Down)); // explorer.down
        app.on_key(KeyEvent::from(KeyCode::Up)); // explorer.up
        app.on_key(KeyEvent::from(KeyCode::Right)); // explorer.expand
        app.on_key(KeyEvent::from(KeyCode::Left)); // explorer.collapse
        app.on_key(KeyEvent::from(KeyCode::Enter)); // explorer.activate
                                                    // revealActiveFile has no arrow binding; drive it directly through the registry.
        app.registry
            .dispatch_command("explorer.revealActiveFile", &mut app.editor);
        // Esc returns focus to the editor.
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.editor.focus, Focus::Editor);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lsp_commands_request_at_cursor() {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!("lumina_lsp_{}_{}.rs", std::process::id(), n));
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let mut app = app_with(&path);
        // A .rs doc resolves lsp_position, so each command reaches its request arm.
        app.dispatch(Command::Hover);
        app.dispatch(Command::GotoDefinition);
        app.dispatch(Command::Completion);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn plugin_actions_dispatch_all_kinds() {
        // A Rhai plugin that returns one of every action kind, with the capabilities to
        // exercise each arm of the runtime's action dispatcher.
        let manifest = "id = \"multi\"\ncapabilities = [\"edit\", \"ui\", \"fs:read\"]\n\
                        [[commands]]\nid = \"multi.go\"\ntitle = \"Multi\"\n";
        let script = "fn on_command(id, ctx) { [ \
                      #{ action: \"insert\", text: \"I\" }, \
                      #{ action: \"replace_selection\", text: \"R\" }, \
                      #{ action: \"replace_line\", text: \"L\" }, \
                      #{ action: \"notify\", message: \"hi\" }, \
                      #{ action: \"run\", command: \"view.toggleTheme\" }, \
                      #{ action: \"set_panel\", panel: \"multi.panel\", lines: [\"x\", \"y\"] } \
                      ] }";
        let (dir, file) = temp_project_with_plugin("multi", manifest, script, "hello world");
        let mut app = app_with(&file);
        assert!(app.registry.command_ids().any(|c| c == "multi.go"));
        app.exec_id("multi.go");
        // The set_panel action ran (its panel is now populated); the others ran without error.
        assert!(app.editor.panels.contains_key("multi.panel"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explorer_commands_navigate_toggle_and_reveal() {
        let dir = temp_dir_with_files();
        let mut app = app_with(&dir);
        // Open a file so the active document has a path for reveal-active-file to resolve.
        app.open_path(&dir.join("a.txt"));
        // Drive every explorer command through the registry (its run_command dispatcher).
        for id in [
            "explorer.down",
            "explorer.up",
            "explorer.expand",
            "explorer.collapse",
            "explorer.activate",
            "explorer.revealActiveFile",
        ] {
            app.registry.dispatch_command(id, &mut app.editor);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- terminal panel ------------------------------------------------------

    #[test]
    fn terminal_panel_layout_and_header_render_without_a_shell() {
        // Force the dock open without spawning a shell, exercising the layout split, the header
        // controls, and the empty-content branch — all PTY-free, so it runs everywhere.
        let path = temp_file("x");
        let mut app = app_with(&path);
        assert!(!app.panel.open);

        app.panel.open = true;
        let text = render_to_string(&mut app, 60, 16);
        assert!(text.contains('▾'), "header shows the minimize control");
        assert!(text.contains('+'), "header shows the new-terminal control");

        // Minimized → only the header row is laid out (no content region recorded).
        app.panel.minimized = true;
        let _ = render_to_string(&mut app, 60, 16);
        assert!(app.regions.panel_header.is_some());
        assert!(app.regions.panel_content.is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn minimize_and_close_return_focus_to_editor() {
        // State transitions without a live shell: minimize/restore + close bookkeeping.
        let path = temp_file("x");
        let mut app = app_with(&path);
        app.panel.open = true;
        app.editor.focus = Focus::Panel;

        app.minimize_terminal();
        assert!(app.panel.minimized);
        assert_eq!(app.editor.focus, Focus::Editor);

        app.minimize_terminal();
        assert!(!app.panel.minimized);

        // Closing with no terminals collapses the dock and restores editor focus.
        app.close_terminal();
        assert!(!app.panel.open);
        assert_eq!(app.editor.focus, Focus::Editor);
        std::fs::remove_file(&path).ok();
    }

    /// Drive the terminal panel end-to-end against a real PTY + `/bin/sh`: render, spawn, type,
    /// switch tabs, scroll, and close. Unix-only (ConPTY/`cmd` behavior differs on Windows) and
    /// guarded so a runner without a usable PTY skips cleanly rather than failing.
    #[cfg(unix)]
    #[test]
    fn terminal_end_to_end_drive() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let dir = temp_dir_with_files();
        let mut app = app_with(&dir);
        app.config.terminal_shell = Some("/bin/sh".to_string());

        // First frame lays out the (closed) panel; toggling then spawns + focuses the shell.
        let _ = render_to_string(&mut app, 120, 40);
        app.dispatch(Command::ToggleTerminal);
        if app.panel.terminals.is_empty() {
            return; // no usable PTY on this runner — skip rather than fail.
        }
        assert_eq!(app.editor.focus, Focus::Panel);
        // Re-lay-out so the PTY is sized to the panel region.
        let _ = render_to_string(&mut app, 120, 40);
        app.sync_terminals();

        // Type a command via the real key path (Focus::Panel → PTY bytes).
        for ch in "echo lumina_smoke".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            pump_until(&mut app, "lumina_smoke"),
            "the shell echo should render in the terminal panel"
        );

        // A bracketed paste while focused goes to the shell, not the document.
        app.on_paste("echo pasted_ok\r".to_string());
        assert!(
            pump_until(&mut app, "pasted_ok"),
            "paste should reach the shell"
        );

        // Emit far more than one screenful, then wheel up hard over the panel: scrolling past a
        // screenful must not panic vt100's `cell()` (regression for the scrollback-clamp fix).
        for ch in "seq 1 300".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        pump_until(&mut app, "300");
        let content = app.regions.panel_content.expect("panel content region");
        for _ in 0..40 {
            app.on_mouse(mouse(
                MouseEventKind::ScrollUp,
                content.x + 2,
                content.y + 1,
            ));
            let _ = render_to_string(&mut app, 120, 40); // must not panic while scrolled back
        }
        // Typing snaps back to the live view.
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.panel.active_terminal().unwrap().at_live());

        // A second tab, then cycle and switch by clicking the header.
        app.dispatch(Command::NewTerminal);
        assert_eq!(app.panel.terminals.len(), 2);
        app.dispatch(Command::PrevTerminal);
        assert_eq!(app.panel.active, 0);
        app.dispatch(Command::NextTerminal);
        assert_eq!(app.panel.active, 1);
        let header = app.regions.panel_header.expect("header region");
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            header.x + 6,
            header.y,
        )); // click first tab area → focuses panel
        assert_eq!(app.editor.focus, Focus::Panel);

        // Close tabs until the dock collapses and focus returns to the editor.
        app.dispatch(Command::CloseTerminal);
        assert_eq!(app.panel.terminals.len(), 1);
        app.dispatch(Command::CloseTerminal);
        assert!(!app.panel.open);
        assert_eq!(app.editor.focus, Focus::Editor);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Drain PTY output and redraw until `needle` renders, or a short timeout elapses.
    #[cfg(unix)]
    fn pump_until(app: &mut App, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            app.drain_workers();
            if render_to_string(app, 120, 40).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn toggle_terminal_close_branch_without_spawn() {
        // Open + expanded → toggle closes and returns focus to the editor (no shell needed).
        let path = temp_file("x");
        let mut app = app_with(&path);
        app.panel.open = true;
        app.editor.focus = Focus::Panel;
        app.dispatch(Command::ToggleTerminal);
        assert!(!app.panel.open);
        assert_eq!(app.editor.focus, Focus::Editor);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn terminal_commands_and_routing_are_inert_without_a_panel() {
        let path = temp_file("hello");
        let mut app = app_with(&path);
        // next/prev are guarded no-ops while the dock is closed.
        app.dispatch(Command::NextTerminal);
        app.dispatch(Command::PrevTerminal);
        assert!(!app.panel.open && app.panel.active == 0);

        // A wheel scroll not over the panel routes to the editor.
        let body: String = (0..50).map(|i| format!("l{i}\n")).collect();
        let p2 = temp_file(&body);
        let mut app2 = app_with(&p2);
        app2.regions.editor = Rect::new(0, 0, 80, 24);
        app2.on_mouse(mouse(MouseEventKind::ScrollDown, 10, 10));
        assert_eq!(app2.editor.active_document().unwrap().view.scroll_line, 3);

        // A stray Panel focus with no terminal falls back to the editor.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        app.editor.focus = Focus::Panel;
        assert!(!app.handle_terminal_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert_eq!(app.editor.focus, Focus::Editor);

        // A paste while not panel-focused edits the document.
        app.on_paste("Z".to_string());
        assert!(app
            .editor
            .active_document()
            .unwrap()
            .to_string()
            .contains('Z'));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&p2).ok();
    }
}
