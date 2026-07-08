//! High-level editing operations that apply to *every* selection at once.
//!
//! This is the one place multi-cursor edits are built: each selection contributes a
//! change, the changes are applied as a single [`Transaction`] (bottom-up, so offsets
//! stay valid — plan "hard part" #3), history is recorded, and the selection set is
//! re-derived from the edited positions.

use std::ops::Range;

use crate::document::Document;
use crate::history::GroupBreak;
use crate::motion::{self, Motion};
use crate::selection::{Selection, Selections};
use crate::transaction::{Change, Transaction};

/// Apply a per-selection edit. `f` maps each selection to `(range_to_replace, replacement)`.
/// Ranges must be non-overlapping across selections (the set is normalized, so they are).
pub fn edit_selections<F>(doc: &mut Document, mut f: F, group: GroupBreak)
where
    F: FnMut(&Document, Selection) -> (Range<usize>, String),
{
    let before = doc.selections.clone();

    let mut ops: Vec<(Range<usize>, String)> =
        doc.selections.ranges().iter().map(|s| f(doc, *s)).collect();
    ops.sort_by_key(|(r, _)| r.start);

    let changes: Vec<Change> = ops
        .iter()
        .map(|(r, text)| {
            let start = r.start.min(doc.len_chars());
            let end = r.end.min(doc.len_chars());
            let removed = if start < end {
                doc.text.slice(start..end).to_string()
            } else {
                String::new()
            };
            Change {
                at: start,
                removed,
                inserted: text.clone(),
            }
        })
        .collect();

    let txn = Transaction::from_changes(changes);
    if txn.is_empty() {
        return;
    }
    let inverse = txn.apply(doc);

    // New caret after each op: op start shifted by cumulative delta, plus inserted len.
    let mut delta: isize = 0;
    let mut new_sels: Vec<Selection> = Vec::with_capacity(ops.len());
    for (r, text) in &ops {
        let start = (r.start as isize + delta) as usize;
        let ins = text.chars().count();
        new_sels.push(Selection::caret(start + ins));
        delta += ins as isize - (r.end - r.start) as isize;
    }
    let after = Selections::from_iter(new_sels);
    doc.selections = after.clone();
    doc.view.goal_col = None;
    doc.dirty = true;
    doc.history.record(txn, inverse, before, after, group);
}

/// Insert `text` at every caret (replacing any selected span).
pub fn insert_text(doc: &mut Document, text: &str, group: GroupBreak) {
    edit_selections(doc, |_d, sel| (sel.span(), text.to_string()), group);
}

/// Insert a single typed char (coalesces into the current undo group).
pub fn insert_char(doc: &mut Document, ch: char) {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    insert_text(doc, s, GroupBreak::None);
}

/// Insert a newline at every caret, breaking the undo group.
pub fn insert_newline(doc: &mut Document) {
    insert_text(doc, "\n", GroupBreak::Force);
}

/// Delete the char (grapheme) before each caret, or the selection if non-empty.
pub fn delete_backward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let from = motion::resolve(d, sel.head, Motion::Left, 1);
                (from..sel.head, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

/// Delete the char after each caret, or the selection if non-empty.
pub fn delete_forward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let to = motion::resolve(d, sel.head, Motion::Right, 1);
                (sel.head..to, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

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

/// Undo one revision; installs the restored selection set.
pub fn undo(doc: &mut Document) -> bool {
    let mut hist = std::mem::take(&mut doc.history);
    let restored = hist.undo(doc);
    doc.history = hist;
    if let Some(sel) = restored {
        doc.selections = sel;
        doc.dirty = true;
        true
    } else {
        false
    }
}

/// Redo one revision.
pub fn redo(doc: &mut Document) -> bool {
    let mut hist = std::mem::take(&mut doc.history);
    let restored = hist.redo(doc);
    doc.history = hist;
    if let Some(sel) = restored {
        doc.selections = sel;
        doc.dirty = true;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multi_caret(doc: &mut Document, positions: &[usize]) {
        let sels: Vec<Selection> = positions.iter().map(|&p| Selection::caret(p)).collect();
        doc.selections = Selections::from_iter(sels);
    }

    #[test]
    fn insert_at_single_caret() {
        let mut doc = Document::from_str("hello");
        doc.set_caret(5);
        insert_text(&mut doc, "!", GroupBreak::Force);
        assert_eq!(doc.to_string(), "hello!");
    }

    #[test]
    fn multi_cursor_insert_keeps_offsets_valid() {
        let mut doc = Document::from_str("a\nb\nc");
        // carets at start of each line: offsets 0, 2, 4
        multi_caret(&mut doc, &[0, 2, 4]);
        insert_text(&mut doc, "> ", GroupBreak::Force);
        assert_eq!(doc.to_string(), "> a\n> b\n> c");
        // three carets, each after its inserted "> "
        assert_eq!(doc.selections.len(), 3);
    }

    #[test]
    fn backspace_then_undo() {
        let mut doc = Document::from_str("hello");
        doc.set_caret(5);
        delete_backward(&mut doc);
        assert_eq!(doc.to_string(), "hell");
        undo(&mut doc);
        assert_eq!(doc.to_string(), "hello");
        assert_eq!(doc.selections.primary().head, 5);
    }

    #[test]
    fn typing_burst_undoes_together() {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        insert_char(&mut doc, 'h');
        insert_char(&mut doc, 'i');
        assert_eq!(doc.to_string(), "hi");
        undo(&mut doc);
        assert_eq!(doc.to_string(), "");
    }
}
