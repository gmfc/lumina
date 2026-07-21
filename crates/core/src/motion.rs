//! Motions: pure functions from a char offset to a new char offset over a [`Document`].
//!
//! Motions never mutate. Commands apply a motion to *every* selection (multi-cursor is
//! structural), collapsing or extending as appropriate.

use crate::document::Document;

mod bracket;
mod char_motion;
mod word;

pub use bracket::matching_bracket;
pub use word::word_at;

use char_motion::{
    first_non_blank, next_grapheme, prev_grapheme, vertical, vertical_visual, visual_line_end,
    visual_line_start, wrap_active,
};
use word::{word_end_right, word_left, word_right};

/// A cursor motion. Resolved by [`resolve`] against a document + starting offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    WordEndRight,
    LineStart,
    LineEnd,
    LineFirstNonBlank,
    DocStart,
    DocEnd,
    PageUp,
    PageDown,
    MatchingBracket,
}

/// Resolve `motion` from char offset `pos`. `page` is the viewport height in lines
/// (used by PageUp/PageDown). Returns the new char offset, clamped to the document.
pub fn resolve(doc: &Document, pos: usize, motion: Motion, page: usize) -> usize {
    let len = doc.len_chars();
    let pos = pos.min(len);
    match motion {
        Motion::Left => prev_grapheme(doc, pos),
        Motion::Right => next_grapheme(doc, pos),
        // Up/Down move one *visual* row under soft-wrap (per-doc `view.wrap`), else one logical
        // line. PageUp/PageDown stay logical-line based for now (MVP).
        Motion::Up => vertical_visual(doc, pos, -1),
        Motion::Down => vertical_visual(doc, pos, 1),
        Motion::WordLeft => word_left(doc, pos),
        Motion::WordRight => word_right(doc, pos),
        Motion::WordEndRight => word_end_right(doc, pos),
        // Home/End snap to the *visual* row's edges under soft-wrap, else the logical line's.
        Motion::LineStart if wrap_active(doc) => visual_line_start(doc, pos),
        Motion::LineStart => {
            let line = doc.char_to_line(pos);
            doc.line_to_char(line)
        }
        Motion::LineEnd if wrap_active(doc) => visual_line_end(doc, pos),
        Motion::LineEnd => {
            let line = doc.char_to_line(pos);
            doc.line_to_char(line) + doc.line_len_chars(line)
        }
        Motion::LineFirstNonBlank => first_non_blank(doc, pos),
        Motion::DocStart => 0,
        Motion::DocEnd => len,
        Motion::PageUp => vertical(doc, pos, -(page.max(1) as isize)),
        Motion::PageDown => vertical(doc, pos, page.max(1) as isize),
        Motion::MatchingBracket => matching_bracket(doc, pos).unwrap_or(pos),
    }
}

/// The `[start, end)` char range of the whole line containing `pos`, including its
/// trailing newline (so triple-click selects the line and its break). Used by triple-click.
pub fn line_at(doc: &Document, pos: usize) -> (usize, usize) {
    let line = doc.char_to_line(pos);
    let start = doc.line_to_char(line);
    let end = if line + 1 < doc.len_lines() {
        doc.line_to_char(line + 1)
    } else {
        doc.len_chars()
    };
    (start, end)
}

#[cfg(test)]
mod tests;
