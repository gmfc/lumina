//! Per-document git status refresh (the change map that feeds the gutter). Navigating between
//! changes is the `git-nav` builtin plugin (crates/builtins), which reads this map through
//! `Host::changed_lines`.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
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
