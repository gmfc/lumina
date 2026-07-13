//! File and tab lifecycle: open, close, save / save-as, and new file.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Drop all per-document state for a closed document, so these maps don't grow unbounded
    /// over a long session of opening and closing files.
    pub(super) fn forget_doc(&mut self, id: editor_core::DocId) {
        self.editor.highlighters.remove(&id);
        self.editor.decorations.remove(&id);
        // The `diagnostics` plugin prunes its own model on tab change (DidChangeActive).
        self.editor.git_hunks.remove(&id);
        self.lsp_sent_revision.remove(&id);
        self.lsp_pulled_revision.remove(&id);
        self.lsp_pull_deadline.remove(&id);
    }

    /// Close the tab at `idx` and drop the removed document's per-doc state (see [`forget_doc`]).
    pub(super) fn close_and_forget(&mut self, idx: usize) {
        // Capture the doc's LSP identity before the tab is removed, so we can tell the server the
        // document closed (§4.1) once we confirm it was actually dropped.
        let closing = self
            .editor
            .workspace
            .tabs
            .get(idx)
            .copied()
            .and_then(|id| self.editor.workspace.documents.get(id))
            .and_then(|d| Some((d.path.clone()?, d.language.clone()?)));
        if let Some(id) = self.editor.workspace.close_tab(idx) {
            if let Some((path, lang)) = closing {
                self.lsp.did_close(&path, &lang);
            }
            self.forget_doc(id);
        }
    }

    /// Close a tab, prompting first if it has unsaved changes (plan §6).
    pub(super) fn request_close(&mut self, tab: usize) {
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
            self.remember_closed(tab);
            self.close_and_forget(tab);
        }
    }

    /// Push a closed tab's path onto the reopen stack (Ctrl+Shift+T restores the newest).
    /// Untitled buffers have no path, so nothing is remembered for them.
    pub(super) fn remember_closed(&mut self, tab: usize) {
        if let Some(&id) = self.editor.workspace.tabs.get(tab) {
            if let Some(path) = self
                .editor
                .workspace
                .documents
                .get(id)
                .and_then(|d| d.path.clone())
            {
                self.closed_tabs.push(path);
            }
        }
    }

    /// Ctrl+Shift+T: reopen the most recently closed tab that still exists and isn't already
    /// open, focusing it. Skips missing files and duplicates, popping until one lands.
    pub(super) fn reopen_closed_tab(&mut self) {
        while let Some(path) = self.closed_tabs.pop() {
            if let Some(id) = self.editor.workspace.find_by_path(&path) {
                self.editor.workspace.focus_doc(id);
                self.editor.focus = Focus::Editor;
                return;
            }
            if path.exists() {
                self.open_path(&path);
                self.editor.focus = Focus::Editor;
                return;
            }
        }
        self.editor.status_message = Some("No closed editors to reopen".into());
    }

    /// Ctrl+K S: save every open, path-backed tab that has unsaved changes.
    pub(super) fn save_all(&mut self) {
        let restore = self.editor.workspace.active_tab;
        let count = self.editor.workspace.tabs.len();
        let mut saved = 0;
        for i in 0..count {
            self.editor.workspace.focus_tab(i);
            let (has_path, dirty) = self
                .editor
                .active_document()
                .map(|d| (d.path.is_some(), d.dirty))
                .unwrap_or((false, false));
            if has_path && dirty {
                self.save_active();
                saved += 1;
            }
        }
        self.editor.workspace.focus_tab(restore);
        self.editor.status_message = Some(format!("Saved {saved} file(s)"));
    }

    /// Ctrl+K Ctrl+W: close every tab. Clean tabs close outright; the first dirty one opens
    /// the confirm-close prompt and stops, so no unsaved work is lost silently.
    pub(super) fn close_all_tabs(&mut self) {
        while let Some(&id) = self.editor.workspace.tabs.last() {
            let idx = self.editor.workspace.tabs.len() - 1;
            let dirty = self
                .editor
                .workspace
                .documents
                .get(id)
                .map(|d| d.dirty)
                .unwrap_or(false);
            if dirty {
                self.request_close(idx); // prompt; re-run Close All after resolving it
                return;
            }
            self.remember_closed(idx);
            self.close_and_forget(idx);
        }
    }

    pub(super) fn cycle_tab(&mut self, delta: isize) {
        let n = self.editor.workspace.tabs.len();
        if n == 0 {
            return;
        }
        let cur = self.editor.workspace.active_tab as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.editor.workspace.focus_tab(next);
    }

    pub(super) fn open_path(&mut self, path: &std::path::Path) {
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

    /// Save the active document, falling back to the Save As prompt when it has no path yet
    /// (plan §1.5 — resolves the old "Save As not yet wired" gap).
    pub(super) fn save_or_save_as(&mut self) {
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
    pub(super) fn open_save_as(&mut self) {
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
    pub(super) fn save_as_to(&mut self, raw: &str) {
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
    pub(super) fn new_file(&mut self) {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        self.editor.workspace.open_document(doc);
        self.editor.focus = Focus::Editor;
    }

    pub(super) fn save_active(&mut self) {
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
            self.editor.status_message = Some("No path — use Save As".into());
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
