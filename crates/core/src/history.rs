//! Undo/redo as a stack of reversible revisions, with VS Code-style typing coalescence.
//!
//! Each edit records a [`Revision`] carrying the forward transaction (redo), its inverse
//! (undo), and the selection sets on both sides — so undo restores the cursor too
//! (plan §3). Consecutive single-char insertions coalesce into one revision until a
//! group break (cursor jump, save, or an idle timeout the caller signals).

use crate::document::Document;
use crate::selection::Selections;
use crate::transaction::{Change, Transaction};

/// One undoable step.
#[derive(Debug, Clone)]
pub struct Revision {
    pub forward: Transaction,
    pub inverse: Transaction,
    pub selections_before: Selections,
    pub selections_after: Selections,
}

/// Why a coalescing group should end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBreak {
    /// Keep coalescing if possible.
    None,
    /// Force a new revision (cursor jump, save, idle timeout, non-insert edit).
    Force,
}

#[derive(Default)]
pub struct History {
    past: Vec<Revision>,
    future: Vec<Revision>,
    /// When true, the next edit starts a fresh revision even if it would otherwise merge.
    break_next: bool,
}

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History")
            .field("past", &self.past.len())
            .field("future", &self.future.len())
            .finish()
    }
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn past_len(&self) -> usize {
        self.past.len()
    }

    /// Signal that the current coalescing group is finished (call on save, cursor jump,
    /// or ~300ms idle).
    pub fn break_group(&mut self) {
        self.break_next = true;
    }

    /// Record an already-applied edit. `forward` is the transaction that was applied,
    /// `inverse` is what `forward.apply` returned. Any redo stack is cleared.
    ///
    /// If `brk == GroupBreak::None` and this edit is a single adjacent char insertion
    /// continuing the previous one, it merges into the last revision.
    pub fn record(
        &mut self,
        forward: Transaction,
        inverse: Transaction,
        selections_before: Selections,
        selections_after: Selections,
        brk: GroupBreak,
    ) {
        self.future.clear();

        let can_merge = brk == GroupBreak::None && !self.break_next && self.can_coalesce(&forward);

        if can_merge {
            if let Some(last) = self.past.last_mut() {
                last.forward = concat_forward(&last.forward, &forward);
                last.inverse = concat_inverse(&inverse, &last.inverse);
                last.selections_after = selections_after;
                self.break_next = false;
                return;
            }
        }

        self.past.push(Revision {
            forward,
            inverse,
            selections_before,
            selections_after,
        });
        self.break_next = false;
    }

    /// A new edit coalesces with the previous revision iff both are a single-char
    /// insertion and the new one begins exactly where the previous one ended.
    fn can_coalesce(&self, forward: &Transaction) -> bool {
        let Some(last) = self.past.last() else {
            return false;
        };
        let (Some(prev), Some(next)) = (single_insert(&last.forward), single_insert(forward))
        else {
            return false;
        };
        // Adjacent, and not across a whitespace/word boundary that VS Code would break on.
        let prev_end = prev.at + prev.inserted.chars().count();
        prev_end == next.at && !next.inserted.contains('\n')
    }

    /// Undo one revision: apply its inverse to `doc`, restore the prior selection set.
    /// Returns the selections to install, or `None` if nothing to undo.
    pub fn undo(&mut self, doc: &mut Document) -> Option<Selections> {
        let rev = self.past.pop()?;
        rev.inverse.apply(doc);
        let sel = rev.selections_before.clone();
        self.future.push(rev);
        self.break_next = true;
        Some(sel)
    }

    /// Redo one revision: re-apply its forward transaction.
    pub fn redo(&mut self, doc: &mut Document) -> Option<Selections> {
        let rev = self.future.pop()?;
        rev.forward.apply(doc);
        let sel = rev.selections_after.clone();
        self.past.push(rev);
        self.break_next = true;
        Some(sel)
    }
}

/// If `txn` is exactly one insertion (no removal), return its change.
fn single_insert(txn: &Transaction) -> Option<&Change> {
    if txn.changes.len() == 1 && txn.changes[0].removed.is_empty() {
        Some(&txn.changes[0])
    } else {
        None
    }
}

/// Merge two forward insertion transactions where `b` continues `a`.
fn concat_forward(a: &Transaction, b: &Transaction) -> Transaction {
    // Both are single inserts, b.at == a.at + len(a.inserted). Combine into one insert.
    if let (Some(ca), Some(cb)) = (single_insert(a), single_insert(b)) {
        let mut inserted = ca.inserted.clone();
        inserted.push_str(&cb.inserted);
        return Transaction {
            changes: vec![Change {
                at: ca.at,
                removed: String::new(),
                inserted,
            }],
        };
    }
    // Fallback: not coalescible, keep `b`.
    b.clone()
}

/// Merge two inverse transactions. `new_inv` undoes the newest edit, `old_inv` the older;
/// the combined inverse removes the whole coalesced insertion in one step.
fn concat_inverse(new_inv: &Transaction, old_inv: &Transaction) -> Transaction {
    // For coalesced single-char inserts the inverse is a single deletion spanning both.
    if let (Some(cn), Some(co)) = (single_delete(new_inv), single_delete(old_inv)) {
        // old deletion removes chars starting at co.at; new removes the tail.
        let at = co.at.min(cn.at);
        let mut removed = co.removed.clone();
        removed.push_str(&cn.removed);
        return Transaction {
            changes: vec![Change {
                at,
                removed,
                inserted: String::new(),
            }],
        };
    }
    old_inv.clone()
}

/// If `txn` is exactly one deletion (no insertion), return its change.
fn single_delete(txn: &Transaction) -> Option<&Change> {
    if txn.changes.len() == 1 && txn.changes[0].inserted.is_empty() {
        Some(&txn.changes[0])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{Selection, Selections};

    fn caret(pos: usize) -> Selections {
        Selections::single(Selection::caret(pos))
    }

    fn type_char(doc: &mut Document, hist: &mut History, at: usize, ch: &str, brk: GroupBreak) {
        let before = caret(at);
        let fwd = Transaction::insert(doc, at, ch);
        let inv = fwd.apply(doc);
        let after = caret(at + ch.chars().count());
        hist.record(fwd, inv, before, after, brk);
    }

    #[test]
    fn typing_coalesces_into_one_undo() {
        let mut doc = Document::from_str("");
        let mut hist = History::default();
        type_char(&mut doc, &mut hist, 0, "h", GroupBreak::None);
        type_char(&mut doc, &mut hist, 1, "i", GroupBreak::None);
        assert_eq!(doc.to_string(), "hi");
        assert_eq!(hist.past_len(), 1); // coalesced
        hist.undo(&mut doc);
        assert_eq!(doc.to_string(), ""); // one undo removes the whole burst
    }

    #[test]
    fn group_break_splits_undo() {
        let mut doc = Document::from_str("");
        let mut hist = History::default();
        type_char(&mut doc, &mut hist, 0, "h", GroupBreak::None);
        type_char(&mut doc, &mut hist, 1, "i", GroupBreak::Force);
        assert_eq!(hist.past_len(), 2);
        hist.undo(&mut doc);
        assert_eq!(doc.to_string(), "h");
    }

    #[test]
    fn undo_then_redo() {
        let mut doc = Document::from_str("");
        let mut hist = History::default();
        type_char(&mut doc, &mut hist, 0, "a", GroupBreak::Force);
        hist.undo(&mut doc);
        assert_eq!(doc.to_string(), "");
        hist.redo(&mut doc);
        assert_eq!(doc.to_string(), "a");
    }
}
