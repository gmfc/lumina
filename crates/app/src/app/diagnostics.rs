//! Diagnostic navigation and the caret-diagnostic lookup.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Jump the caret to the next (`dir > 0`) or previous diagnostic in the active document,
    /// wrapping around the ends (plan §2.2).
    pub(super) fn goto_diagnostic(&mut self, dir: isize) {
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
}
