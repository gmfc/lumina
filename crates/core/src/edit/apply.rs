//! The multi-selection edit core: build one [`Transaction`] from a per-selection op set,
//! apply it bottom-up (offsets stay valid — plan "hard part" #3), record history, and
//! re-derive the selection set.

use std::ops::Range;

use crate::document::Document;
use crate::history::GroupBreak;
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
    clamp_non_overlapping(&mut ops);

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

/// Clamp a start-sorted op list so the ranges are non-overlapping, merging any overlap into the
/// earlier op. `edit_selections` maps one op per selection, and the selection *set* is normalized
/// (non-overlapping) — but an op whose range reaches *outside* its own selection can still cover a
/// neighbour. The clearest case is a word-wise backspace with two carets inside one word: each
/// derives a delete reaching back to the word start, so the ranges overlap even though the carets
/// don't. `Transaction` requires non-overlapping changes, so without this the forward apply
/// double-removes and the inverse underflows — silent buffer + undo corruption. Pulling each
/// range's start up to the previous range's end turns the overlap into one contiguous deletion,
/// which is exactly the intended union for the delete-style ops that can reach.
fn clamp_non_overlapping(ops: &mut [(Range<usize>, String)]) {
    let mut running_end = 0usize;
    for (r, _) in ops.iter_mut() {
        if r.start < running_end {
            r.start = running_end.min(r.end);
        }
        running_end = running_end.max(r.end);
    }
}

/// Like [`edit_selections`] but each op also dictates the resulting selection, via
/// `(anchor_offset, head_offset)` measured in chars from the op's post-shift start. This is
/// what lets auto-pairs drop the caret *between* an inserted pair, step it *past* an existing
/// closer (a no-op edit that still moves the cursor), or keep the inner text selected when
/// surrounding a selection — all through one [`Transaction`] (plan §1.1 invariants).
pub(super) fn edit_selections_sel<F>(doc: &mut Document, mut f: F, group: GroupBreak)
where
    F: FnMut(&Document, Selection) -> (Range<usize>, String, (usize, usize)),
{
    let before = doc.selections.clone();

    let mut ops: Vec<(Range<usize>, String, (usize, usize))> =
        doc.selections.ranges().iter().map(|s| f(doc, *s)).collect();
    ops.sort_by_key(|(r, _, _)| r.start);

    // Skip no-op ops (e.g. a "type over" that inserts and removes nothing) so a pure cursor
    // step never marks the buffer dirty or lands on the undo stack.
    let changes: Vec<Change> = ops
        .iter()
        .filter_map(|(r, text, _)| {
            let start = r.start.min(doc.len_chars());
            let end = r.end.min(doc.len_chars());
            let removed = if start < end {
                doc.text.slice(start..end).to_string()
            } else {
                String::new()
            };
            if removed.is_empty() && text.is_empty() {
                None
            } else {
                Some(Change {
                    at: start,
                    removed,
                    inserted: text.clone(),
                })
            }
        })
        .collect();

    let txn = Transaction::from_changes(changes);
    let inverse = if txn.is_empty() {
        Transaction::empty()
    } else {
        txn.apply(doc)
    };

    // Resulting selections: each op's start shifts by the cumulative delta of prior ops
    // (no-op ops contribute zero delta, so filtering them out above is safe here).
    let len = doc.len_chars();
    let mut delta: isize = 0;
    let mut new_sels: Vec<Selection> = Vec::with_capacity(ops.len());
    for (r, text, (a_off, h_off)) in &ops {
        let start = (r.start as isize + delta) as usize;
        new_sels.push(Selection::new(
            (start + a_off).min(len),
            (start + h_off).min(len),
        ));
        let ins = text.chars().count();
        delta += ins as isize - (r.end - r.start) as isize;
    }
    let after = Selections::from_iter(new_sels);
    doc.selections = after.clone();
    doc.view.goal_col = None;
    if !txn.is_empty() {
        doc.dirty = true;
        doc.history.record(txn, inverse, before, after, group);
    }
}
