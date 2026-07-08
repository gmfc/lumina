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
use editor_core::{Document, Selection};
use editor_plugin::Registry;
use ratatui::DefaultTerminal;

use crate::editor::{EditorState, Focus};
use crate::files;
use crate::input::{key_to_command, Command, Focus as InputFocus};
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
    /// Char offset where the current drag began (selection anchor).
    drag_anchor: Option<usize>,
    /// Last click for multi-click detection.
    last_click: Option<ClickState>,
}

impl App {
    pub fn new(arg: Option<String>) -> Result<App> {
        let (root, open_file) = resolve_arg(arg);
        let mut editor = EditorState::new(root);

        let mut registry = Registry::with_plugins(editor_builtins::all_builtins());
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
        }

        let truecolor = crate::theme::truecolor_supported();
        let mut theme = crate::theme::Theme::default_dark(truecolor);
        theme.load_user_overrides();
        Ok(App {
            editor,
            registry,
            quit: false,
            page_height: 20,
            regions: Regions::default(),
            theme,
            drag_anchor: None,
            last_click: None,
        })
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.editor.update_highlights(self.page_height);
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
        Ok(())
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
        use crossterm::event::KeyCode;

        // A modal overlay captures all input while open.
        if self.editor.overlay.is_some() {
            self.overlay_key(key);
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

        let focus = match self.editor.focus {
            Focus::Editor => InputFocus::Editor,
            Focus::Sidebar => InputFocus::Sidebar,
        };
        if let Some(cmd) = key_to_command(key, focus) {
            self.dispatch(cmd);
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

    fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        let (col, row) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if in_rect(self.regions.editor, col, row) {
                    self.editor.focus = Focus::Editor;
                    self.editor_click(col, row);
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

            // --- editing ---
            Command::InsertChar(c) => self.with_doc(|d| edit::insert_char(d, c)),
            Command::InsertNewline => self.with_doc(edit::insert_newline),
            Command::InsertText(s) => {
                self.with_doc(|d| edit::insert_text(d, &s, editor_core::GroupBreak::Force))
            }
            Command::DeleteBackward => self.with_doc(edit::delete_backward),
            Command::DeleteForward => self.with_doc(edit::delete_forward),
            Command::Indent => {
                self.with_doc(|d| edit::insert_text(d, "    ", editor_core::GroupBreak::Force))
            }
            Command::Outdent => {} // Phase 2 polish
            Command::Paste(s) => {
                self.with_doc(|d| edit::insert_text(d, &s, editor_core::GroupBreak::Force))
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

            // --- ui ---
            Command::ToggleSidebar => self.editor.sidebar_visible = !self.editor.sidebar_visible,
            Command::FocusSidebar => self.editor.focus = Focus::Sidebar,
            Command::FocusEditor => self.editor.focus = Focus::Editor,

            // --- not yet implemented in this phase: surface as a hint + try registry ---
            Command::Run(id) => {
                if !self.registry.dispatch_command(&id, &mut self.editor) {
                    self.editor.status_message = Some(format!("Unknown command: {id}"));
                }
            }
            other => {
                self.editor.status_message = Some(format!("{other:?} — not yet implemented"));
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
