//! `impl Host for EditorState` — the imperative port (`editor_plugin::Host`) the app implements.

use std::path::Path;

use editor_core::{DocId, Selections, Transaction, Workspace};
use editor_plugin::event::Event;
use editor_plugin::host::DirEntry;
use editor_plugin::{CommandInfo, DecorationSet, Host, PanelContent, PickerRequest, Popup, Prompt};

use super::{EditorState, Focus, Overlay};

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

    fn open_location(&mut self, path: &Path, line: u32, character: u32) {
        self.pending_locations
            .push((path.to_path_buf(), line, character));
    }

    fn show_info(&mut self, text: String) {
        self.overlay = Some(Overlay::Info(text));
    }

    fn apply_workspace_edit(&mut self, edit: editor_plugin::LspWorkspaceEdit) {
        self.pending_workspace_edits.push(edit);
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

    fn set_popup(&mut self, popup: Option<Popup>) {
        self.popup = popup;
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

    fn toggle_theme(&mut self) {
        self.pending_theme_toggle = true;
    }

    fn lsp_request(&mut self, kind: editor_plugin::LspRequestKind) {
        self.pending_lsp_requests.push(kind);
    }

    fn lsp_enabled(&self) -> bool {
        self.lsp_enabled
    }

    fn lsp_pos_to_offset(&self, doc: DocId, line: u32, character: u32) -> usize {
        self.workspace
            .documents
            .get(doc)
            .map(|d| crate::app::lsp_pos_to_char(d, line, character))
            .unwrap_or(0)
    }

    fn terminal_open(&mut self, cwd: &Path) -> Option<editor_plugin::TerminalId> {
        let tx = self.job_tx.clone()?;
        let id = editor_plugin::TerminalId(self.next_terminal_id);
        // Spawn at a default size; the app resizes to the drawn region on the next frame
        // (`sync_terminals`), so this is corrected before the terminal is first shown.
        let term = crate::terminal::Terminal::new(id.0, cwd, &self.terminal_shell, 24, 80, tx)?;
        self.next_terminal_id += 1;
        self.terminals.insert(id, term);
        Some(id)
    }

    fn terminal_close(&mut self, id: editor_plugin::TerminalId) {
        // Dropping the `Terminal` kills the shell + reaps the child.
        self.terminals.remove(&id);
    }

    fn set_terminal_view(&mut self, view: editor_plugin::TerminalView) {
        self.terminal_view = view;
    }

    fn set_terminal_focus(&mut self, focused: bool) {
        // The terminal grabbing focus (open/new/select/restore) makes it the *visible* dock tab, so
        // keystrokes can never route to a terminal hidden behind the LSP tab. The terminal plugin is
        // unaware of the dock's tab model — this is where its lifecycle is folded into it.
        if focused {
            self.dock_active = super::DockTab::Terminal;
            self.focus = Focus::Panel;
        } else {
            self.focus = Focus::Editor;
        }
    }

    fn viewport_height(&self) -> usize {
        self.page_height
    }

    fn move_lines(&mut self, doc: DocId, delta: isize, extend: bool) {
        let page = self.page_height;
        let motion = if delta < 0 {
            editor_core::Motion::Up
        } else {
            editor_core::Motion::Down
        };
        if let Some(d) = self.workspace.documents.get_mut(doc) {
            for _ in 0..delta.unsigned_abs() {
                editor_core::edit::move_selections(d, motion, page, extend);
            }
        }
        self.emit(Event::DidChangeCursor(doc));
    }

    fn set_scroll(&mut self, doc: DocId, line: usize) {
        if let Some(d) = self.workspace.documents.get_mut(doc) {
            let max = d.len_lines().saturating_sub(1);
            d.view.scroll_line = line.min(max);
        }
    }

    fn set_dirty(&mut self, doc: DocId, dirty: bool) {
        if let Some(d) = self.workspace.documents.get_mut(doc) {
            d.dirty = dirty;
        }
    }

    fn set_vim_view(&mut self, view: Option<editor_plugin::VimView>) {
        self.vim_view = view;
    }

    fn clipboard_read(&mut self) -> String {
        self.clipboard.get()
    }

    fn clipboard_write(&mut self, text: String) {
        self.clipboard.set(text);
    }

    fn execute(&mut self, command_id: &str) {
        self.pending_commands.push(command_id.to_string());
    }
}
