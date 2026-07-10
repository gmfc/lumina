//! The theme toggle and selection helpers.
//!
//! Multi-cursor commands (add-next-match, select-all-occurrences, add-cursor-above/below) used
//! to live here; they are now the `multicursor` builtin plugin (crates/builtins), reaching the
//! editor only through `Host` (invariant #3).
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Toggle between the dark and light themes.
    pub(super) fn toggle_theme(&mut self) {
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

    pub(super) fn selection_text(&self) -> Option<String> {
        let doc = self.editor.active_document()?;
        let sel = doc.selections.primary();
        if sel.is_empty() {
            None
        } else {
            Some(doc.rope().slice(sel.from()..sel.to()).to_string())
        }
    }
}
