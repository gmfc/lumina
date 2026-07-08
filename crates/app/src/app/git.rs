//! Git change navigation and per-document status refresh.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Jump the caret to the start of the next (`dir > 0`) or previous git hunk in the active
    /// document, wrapping around (plan §4.2 navigation). A hunk starts at a changed line whose
    /// predecessor is unchanged.
    pub(super) fn goto_hunk(&mut self, dir: isize) {
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
    pub(super) fn request_git_status(&self, id: editor_core::DocId) {
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
    pub(super) fn refresh_git_all(&self) {
        for id in self.editor.workspace.tabs.clone() {
            self.request_git_status(id);
        }
    }
}
