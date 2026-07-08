//! Selection movement and word/line selection expansion.

use crate::document::Document;
use crate::motion::{self, Motion};
use crate::selection::{Selection, Selections};

use super::helpers::span_end_line;

/// Move (or extend) every selection by `motion`. `page` is the viewport height.
pub fn move_selections(doc: &mut Document, motion: Motion, page: usize, extend: bool) {
    // Track the sticky goal column for vertical motions on the primary selection.
    let is_vertical = matches!(
        motion,
        Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
    );
    if !is_vertical {
        doc.view.goal_col = None;
    }

    let mut sels: Vec<Selection> = Vec::with_capacity(doc.selections.len());
    for sel in doc.selections.ranges() {
        let new_head = motion::resolve(doc, sel.head, motion, page);
        let anchor = if extend { sel.anchor } else { new_head };
        sels.push(Selection::new(anchor, new_head));
    }
    let primary = doc.selections.primary_index();
    let mut set = Selections::from_iter(sels);
    set.set_primary(primary);
    doc.selections = set;
}

/// Expand every selection to the word under its head (like a double-click).
pub fn select_word(doc: &mut Document) {
    let sels: Vec<Selection> = doc
        .selections
        .ranges()
        .iter()
        .map(|s| {
            let (a, b) = motion::word_at(doc, s.head);
            Selection::new(a, b)
        })
        .collect();
    let primary = doc.selections.primary_index();
    let mut set = Selections::from_iter(sels);
    set.set_primary(primary);
    doc.selections = set;
    doc.view.goal_col = None;
}

/// Expand every selection to the whole line(s) it touches (incl. trailing newline).
pub fn select_line(doc: &mut Document) {
    let sels: Vec<Selection> = doc
        .selections
        .ranges()
        .iter()
        .map(|s| {
            let first = doc.char_to_line(s.from());
            let last = span_end_line(doc, s);
            let start = doc.line_to_char(first);
            let end = if last + 1 < doc.len_lines() {
                doc.line_to_char(last + 1)
            } else {
                doc.len_chars()
            };
            Selection::new(start, end)
        })
        .collect();
    let primary = doc.selections.primary_index();
    let mut set = Selections::from_iter(sels);
    set.set_primary(primary);
    doc.selections = set;
    doc.view.goal_col = None;
}
