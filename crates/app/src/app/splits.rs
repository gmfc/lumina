//! Editor split-pane commands: split / close / focus.

use super::*;
use crate::splits::SplitDir;

impl App {
    /// Split the focused editor pane horizontally (right) or vertically (down).
    pub(super) fn split_editor(&mut self, dir: SplitDir) {
        self.editor.ensure_splits();
        self.editor.store_focused_pane_view();
        let Some(tree) = self.editor.splits.as_mut() else {
            return;
        };
        tree.split_focused(&mut self.editor.split_focus, dir);
        self.editor.apply_focused_pane_view();
        self.force_redraw = true;
    }

    /// Close the focused split pane (no-op when only one remains).
    pub(super) fn close_editor_split(&mut self) {
        self.editor.store_focused_pane_view();
        let Some(tree) = self.editor.splits.as_mut() else {
            return;
        };
        if tree.close_focused(&mut self.editor.split_focus) {
            self.editor.apply_focused_pane_view();
            self.force_redraw = true;
        }
    }

    /// Cycle focus to the next/previous split pane.
    pub(super) fn focus_split(&mut self, forward: bool) {
        self.editor.store_focused_pane_view();
        let Some(tree) = &self.editor.splits else {
            return;
        };
        let paths: Vec<Vec<bool>> = tree.leaves().into_iter().map(|(p, _)| p).collect();
        crate::splits::SplitTree::focus_delta(&mut self.editor.split_focus, &paths, forward);
        self.editor.apply_focused_pane_view();
        self.force_redraw = true;
    }

    /// Focus the pane whose laid-out rect contains `(x, y)`, if any.
    pub(super) fn focus_split_at(&mut self, x: u16, y: u16) {
        let panes = self.regions.editor_panes.clone();
        if let Some((path, _)) = panes.into_iter().find(|(_, r)| {
            x >= r.x
                && x < r.x.saturating_add(r.width)
                && y >= r.y
                && y < r.y.saturating_add(r.height)
        }) {
            if path != self.editor.split_focus {
                self.editor.store_focused_pane_view();
                self.editor.split_focus = path;
                self.editor.apply_focused_pane_view();
            }
        }
    }
}
