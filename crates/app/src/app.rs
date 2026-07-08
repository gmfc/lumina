//! The `App`: terminal lifecycle, the input loop, and the command dispatcher.
//!
//! `App` owns the plugin `Registry` and the `EditorState` as separate fields so dispatch
//! can split-borrow (`registry.dispatch_command(id, &mut self.editor)`).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyEventKind};
use editor_core::edit;
use editor_core::{Document, Selection};
use editor_plugin::Registry;
use ratatui::DefaultTerminal;

use crate::editor::{EditorState, Focus};
use crate::files;
use crate::input::{key_to_command, Command, Focus as InputFocus};
use crate::ui;

pub struct App {
    pub editor: EditorState,
    pub registry: Registry,
    pub quit: bool,
    /// Last body height in rows (for PageUp/PageDown).
    pub page_height: usize,
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

        Ok(App {
            editor,
            registry,
            quit: false,
            page_height: 20,
        })
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
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
        }
        Ok(())
    }

    fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        let focus = match self.editor.focus {
            Focus::Editor => InputFocus::Editor,
            Focus::Sidebar => InputFocus::Sidebar,
        };
        if let Some(cmd) = key_to_command(key, focus) {
            self.dispatch(cmd);
        }
    }

    fn on_mouse(&mut self, _m: crossterm::event::MouseEvent) {
        // Wired in Phase 3.
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
            Command::CloseTab => {
                let idx = self.editor.workspace.active_tab;
                self.editor.workspace.close_tab(idx);
            }
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
    fn quit_sets_flag() {
        let path = temp_file("x");
        let mut app = app_with(&path);
        app.dispatch(Command::Quit);
        assert!(app.quit);
        std::fs::remove_file(&path).ok();
    }
}
