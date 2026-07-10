//! The raw mutation chokepoint for a [`Document`](super::Document).
//!
//! These `apply_raw_*` helpers are the *single* internal path transactions use to poke the
//! rope; nothing outside `core` calls them directly (CLAUDE.md invariant #1). They also
//! record the tree-sitter [`SyntaxEdit`] stream that drives incremental reparsing.

use ropey::Rope;

use super::types::SYNTAX_EDIT_CAP;
use super::{Document, SyntaxEdit};

impl Document {
    /// The tree-sitter point `(row, column-in-bytes)` of a char offset.
    fn point_of_char(&self, char_idx: usize) -> (usize, usize) {
        let idx = char_idx.min(self.len_chars());
        let line = self.text.char_to_line(idx);
        let line_start_byte = self.text.line_to_byte(line);
        let byte = self.text.char_to_byte(idx);
        (line, byte - line_start_byte)
    }

    /// Buffer a syntax edit, dropping to "full reparse" mode if we blow the cap. Only tracked
    /// for documents that carry a language (others never reach the highlighter).
    fn record_syntax_edit(&mut self, edit: SyntaxEdit) {
        if self.language.is_none() {
            return;
        }
        if !self.syntax_edits_valid || self.syntax_edits.len() >= SYNTAX_EDIT_CAP {
            self.syntax_edits.clear();
            self.syntax_edits_valid = false;
            return;
        }
        self.syntax_edits.push(edit);
    }

    /// Remove chars in `[start, end)`. Internal; transactions call this.
    pub(crate) fn apply_raw_remove(&mut self, start: usize, end: usize) {
        let s = start.min(self.len_chars());
        let e = end.min(self.len_chars());
        if s < e {
            // Record the edit (in pre-removal coordinates) before mutating.
            let start_byte = self.text.char_to_byte(s);
            let old_end_byte = self.text.char_to_byte(e);
            let start_point = self.point_of_char(s);
            let old_end_point = self.point_of_char(e);
            self.record_syntax_edit(SyntaxEdit {
                start_byte,
                old_end_byte,
                new_end_byte: start_byte,
                start_point,
                old_end_point,
                new_end_point: start_point,
            });
            self.text.remove(s..e);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Insert `text` at char offset `at`. Internal; transactions call this.
    pub(crate) fn apply_raw_insert(&mut self, at: usize, text: &str) {
        let a = at.min(self.len_chars());
        // Record the edit (start in pre-insert coordinates; new end derived from `text`).
        let start_byte = self.text.char_to_byte(a);
        let start_point = self.point_of_char(a);
        let new_end_byte = start_byte + text.len();
        let new_end_point = match text.rfind('\n') {
            None => (start_point.0, start_point.1 + text.len()),
            Some(last_nl) => (
                start_point.0 + text.matches('\n').count(),
                text.len() - (last_nl + 1),
            ),
        };
        self.record_syntax_edit(SyntaxEdit {
            start_byte,
            old_end_byte: start_byte,
            new_end_byte,
            start_point,
            old_end_point: start_point,
            new_end_point,
        });
        self.text.insert(a, text);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Replace the whole buffer (external reload path). Invalidates incremental syntax state.
    /// `pub(crate)`: a whole-buffer replace has no transaction/inverse, so it must not be an
    /// escape hatch for other crates (invariant #1). The sanctioned external-reload entry point is
    /// [`Document::reload_from_str`], which also clears the now-stale undo history.
    pub(crate) fn set_text(&mut self, rope: Rope) {
        self.text = rope;
        self.revision = self.revision.wrapping_add(1);
        self.syntax_edits.clear();
        self.syntax_edits_valid = false;
    }

    /// Replace the whole buffer from a string, normalizing CRLF to internal LF. `pub(crate)`; see
    /// [`Document::set_text`] — callers outside `core` use [`Document::reload_from_str`].
    pub(crate) fn set_text_str(&mut self, s: &str) {
        let normalized = s.replace("\r\n", "\n");
        self.set_text(Rope::from_str(&normalized));
    }
}
