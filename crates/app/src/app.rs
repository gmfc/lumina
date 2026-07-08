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
    /// Last click for multi-click detection.
    last_click: Option<ClickState>,
    // --- Phase 8: background workers + external sync ---
    /// Sender handed to background workers (search, watcher).
    worker_tx: std::sync::mpsc::Sender<crate::worker::WorkerMsg>,
    /// Receiver drained each tick by the main loop.
    worker_rx: std::sync::mpsc::Receiver<crate::worker::WorkerMsg>,
    /// The filesystem debouncer; kept alive so the watch persists.
    _watcher: Option<Box<dyn std::any::Any>>,
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
}

impl App {
    pub fn new(arg: Option<String>) -> Result<App> {
        let (root, open_file) = resolve_arg(arg);
        let mut editor = EditorState::new(root);

        // Built-in plugins + any external (script) plugins from the plugins dirs. Both
        // register through the same Registry — the external tier has no special path.
        let mut plugins = editor_builtins::all_builtins();
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

        let config = crate::config::Config::load();
        editor.sidebar_width = config.sidebar_width;
        let follow_mode = config.follow_mode;
        let lsp = crate::lsp::LspManager::new(&editor.workspace.root, config.lsp_servers.clone());
        let keymap = build_keymap(&config);

        // Background worker channel + directory watcher on the project root (plan §6).
        let (worker_tx, worker_rx) = crate::worker::channel();
        let watcher =
            crate::worker::spawn_watcher(editor.workspace.root.clone(), worker_tx.clone());
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
            last_click: None,
            worker_tx,
            worker_rx,
            _watcher: watcher,
            pending_self_writes: std::collections::HashMap::new(),
            follow_mode,
            search: None,
            last_search_run: String::new(),
            lsp,
            lsp_sent_revision: std::collections::HashMap::new(),
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

    /// The active project-search state (read by the renderer).
    pub fn search(&self) -> Option<&crate::search::SearchState> {
        self.search.as_ref()
    }

    /// Reload the config file and rebuild the keymap (the `config.reload` command).
    fn reload_config(&mut self) {
        self.config = crate::config::Config::load();
        self.editor.sidebar_width = self.config.sidebar_width;
        self.keymap = build_keymap(&self.config);
        self.editor.status_message = Some("Configuration reloaded".into());
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.editor.update_highlights(self.page_height);
            self.update_lsp();
            terminal.draw(|f| ui::draw(f, self))?;

            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    CtEvent::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
                    CtEvent::Mouse(m) => self.on_mouse(m),
                    CtEvent::Paste(s) => self.dispatch(Command::Paste(s)),
                    CtEvent::Resize(..) => {}
                    _ => {}
                }
            }
            // Drain background worker messages (FS/LSP/parse) — wired in later phases.
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
        use crossterm::event::KeyCode;

        // Modal captures, in priority order.
        if self.editor.overlay.is_some() {
            self.overlay_key(key);
            return;
        }
        if self.editor.picker.is_some() {
            self.picker_key(key);
            return;
        }
        if self.search.is_some() {
            self.search_key(key);
            return;
        }
        if self.editor.find.is_some() {
            self.find_key(key);
            return;
        }

        // Sidebar focus: arrows/enter drive the explorer; Esc returns to the editor.
        if self.editor.focus == Focus::Sidebar {
            if key.code == KeyCode::Esc {
                self.editor.focus = Focus::Editor;
                return;
            }
            if let Some(id) = sidebar_command(key) {
                self.registry.dispatch_command(id, &mut self.editor);
                self.drain_workers();
                return;
            }
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
            Resolve::None => {
                let single = self.pending.len() == 1;
                self.pending.clear();
                // Fallback: printable text entry in the editor.
                if single && self.editor.focus == Focus::Editor {
                    if let KeyCode::Char(c) = key.code {
                        let ctrl = key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL);
                        let alt = key.modifiers.contains(crossterm::event::KeyModifiers::ALT);
                        if !ctrl && !alt {
                            self.dispatch(Command::InsertChar(c));
                        }
                    }
                }
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
        }
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
            MouseEventKind::Down(MouseButton::Left) => {
                if in_rect(self.regions.editor, col, row) {
                    self.editor.focus = Focus::Editor;
                    if m.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
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
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(anchor) = self.drag_anchor {
                    if let Some(off) = self.editor_offset_at(col, row) {
                        self.with_doc(|d| {
                            d.selections.set_single(Selection::new(anchor, off));
                        });
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_anchor = None;
            }
            MouseEventKind::ScrollUp => self.scroll_editor(-3),
            MouseEventKind::ScrollDown => self.scroll_editor(3),
            _ => {}
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

    fn tab_bar_click(&mut self, col: u16) {
        // Tabs render as " name marker " segments; recompute their extents to hit-test.
        let ws = &self.editor.workspace;
        let mut x = self.regions.tabs.x;
        for (i, &id) in ws.tabs.iter().enumerate() {
            let Some(doc) = ws.documents.get(id) else {
                continue;
            };
            let name = doc
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into());
            let label_w = 1 + name.chars().count() + 1 + 1 + 1; // " name _ marker _ "
            let seg_end = x + label_w as u16;
            if col >= x && col < seg_end {
                // The marker (× / ●) sits near the segment's right edge -> close.
                if col >= seg_end.saturating_sub(2) {
                    self.editor.workspace.close_tab(i);
                } else {
                    self.editor.workspace.focus_tab(i);
                }
                return;
            }
            x = seg_end;
        }
    }

    fn sidebar_click(&mut self, _col: u16, row: u16) {
        // Route to the explorer plugin's panel row, if present (Phase 4).
        if let Some(panel) = self.editor.panels.get("explorer.tree") {
            let inner_top = self.regions.sidebar.map(|r| r.y).unwrap_or(0);
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

        // LSP diagnostics: map each update's URI back to an open document.
        for update in self.lsp.poll() {
            if let Some(path) = crate::lsp::path_from_uri(&update.uri) {
                if let Some(id) = self.editor.workspace.find_by_path(&path) {
                    self.editor.diagnostics.insert(id, update.diagnostics);
                }
            }
        }

        // Background worker messages (FS watch, project search).
        while let Ok(msg) = self.worker_rx.try_recv() {
            match msg {
                crate::worker::WorkerMsg::DiskChanged { path } => self.on_disk_changed(&path),
                crate::worker::WorkerMsg::SearchComplete { query, hits } => {
                    if let Some(search) = &mut self.search {
                        if search.query == query {
                            search.results = hits;
                            search.selected = 0;
                            search.running = false;
                        }
                    }
                }
            }
        }
    }

    /// Reconcile an external on-disk change against the buffer (plan §6 decision matrix).
    fn on_disk_changed(&mut self, path: &std::path::Path) {
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
            Command::InsertChar(c) => self.with_doc(|d| edit::insert_char(d, c)),
            Command::InsertNewline => self.with_doc(edit::insert_newline),
            Command::InsertText(s) => {
                self.with_doc(|d| edit::insert_text(d, &s, editor_core::GroupBreak::Force))
            }
            Command::DeleteBackward => self.with_doc(edit::delete_backward),
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
            Command::Save => self.save_active(),
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

            // --- ui ---
            Command::ToggleSidebar => self.editor.sidebar_visible = !self.editor.sidebar_visible,
            Command::FocusSidebar => self.editor.focus = Focus::Sidebar,
            Command::FocusEditor => self.editor.focus = Focus::Editor,
            Command::Palette => self.open_palette(),
            Command::QuickOpen => self.open_quick_open(),
            Command::GotoLine => self.open_goto_line(),

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
            }
            Err(e) => {
                self.editor.status_message = Some(format!("Open failed: {e}"));
            }
        }
    }

    fn save_active(&mut self) {
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
    }
}

/// Run `f` on the find state if it's open.
fn toggle_and<F: FnOnce(&mut FindState)>(find: &mut Option<FindState>, f: F) {
    if let Some(fs) = find {
        f(fs);
    }
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
}
