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
    clamp_non_overlapping(ops.iter_mut().map(|(r, _)| r));

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

/// Build the [`Transaction`] + resulting [`Selections`] for a per-selection edit *without*
/// applying it — the pure half of [`edit_selections`]. A plugin that only has
/// `Host::apply_transaction` + `set_selections` (not `&mut Document`) uses this to reproduce a
/// multi-selection edit: `let (txn, after) = selection_edit_transaction(doc, f);
/// host.apply_transaction(id, txn); host.set_selections(id, after);`. `f` maps each selection to
/// `(range_to_replace, replacement)`, exactly as [`edit_selections`].
pub fn selection_edit_transaction<F>(doc: &Document, mut f: F) -> (Transaction, Selections)
where
    F: FnMut(&Document, Selection) -> (Range<usize>, String),
{
    let mut ops: Vec<(Range<usize>, String)> =
        doc.selections.ranges().iter().map(|s| f(doc, *s)).collect();
    ops.sort_by_key(|(r, _)| r.start);
    clamp_non_overlapping(ops.iter_mut().map(|(r, _)| r));

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

    // Resulting caret after each op: op start shifted by the cumulative delta, plus inserted len.
    let mut delta: isize = 0;
    let mut new_sels: Vec<Selection> = Vec::with_capacity(ops.len());
    for (r, text) in &ops {
        let start = (r.start as isize + delta) as usize;
        let ins = text.chars().count();
        new_sels.push(Selection::caret(start + ins));
        delta += ins as isize - (r.end - r.start) as isize;
    }
    (txn, Selections::from_iter(new_sels))
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
///
/// Takes an iterator of `&mut Range` (not a concrete op tuple) so both [`edit_selections`] and
/// [`edit_selections_sel`] — whose op tuples differ — share the one guard. For an already
/// non-overlapping (normalized) op list it is a no-op.
fn clamp_non_overlapping<'a>(ranges: impl Iterator<Item = &'a mut Range<usize>>) {
    let mut running_end = 0usize;
    for r in ranges {
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
    // Defend against overlapping op ranges exactly as `edit_selections` does — a no-op for the
    // normalized set this normally runs on, but it keeps `Transaction` changes non-overlapping if
    // an unnormalized set ever reaches here (invariant #2 defense-in-depth).
    clamp_non_overlapping(ops.iter_mut().map(|(r, _, _)| r));

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
