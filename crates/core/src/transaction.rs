//! Reversible transactions — the only way a buffer changes (CLAUDE.md invariant #1).
//!
//! A [`Transaction`] is a set of [`Change`]s keyed by **original-document** char offsets,
//! sorted ascending and non-overlapping. Applying it returns its exact inverse (keyed by
//! *edited*-document offsets), which is what makes undo/redo correct — the property the
//! `transaction_roundtrip` suite pins.

use std::ops::Range;

use crate::document::Document;

/// One contiguous edit: at char offset `at`, `removed` text is replaced by `inserted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    fn removed_len(&self) -> usize {
        self.removed.chars().count()
    }

    fn inserted_len(&self) -> usize {
        self.inserted.chars().count()
    }

    fn delta(&self) -> isize {
        self.inserted_len() as isize - self.removed_len() as isize
    }
}

/// A group of changes applied atomically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transaction {
    /// Sorted ascending by `at`, non-overlapping.
    pub changes: Vec<Change>,
}

impl Transaction {
    /// An empty (no-op) transaction.
    pub fn empty() -> Transaction {
        Transaction {
            changes: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Build from raw changes, sorting them into canonical order.
    pub fn from_changes(mut changes: Vec<Change>) -> Transaction {
        changes.sort_by_key(|c| c.at);
        Transaction { changes }
    }

    /// A transaction that inserts `text` at char offset `at`.
    pub fn insert(_doc: &Document, at: usize, text: &str) -> Transaction {
        Transaction {
            changes: vec![Change {
                at,
                removed: String::new(),
                inserted: text.to_string(),
            }],
        }
    }

    /// A transaction that deletes the chars in `range`. Captures the removed text from
    /// `doc` so the change is invertible.
    pub fn delete(doc: &Document, range: Range<usize>) -> Transaction {
        let start = range.start.min(doc.len_chars());
        let end = range.end.min(doc.len_chars());
        let removed: String = if start < end {
            doc.text.slice(start..end).to_string()
        } else {
            String::new()
        };
        Transaction {
            changes: vec![Change {
                at: start,
                removed,
                inserted: String::new(),
            }],
        }
    }

    /// Replace `range` with `text` in one change.
    pub fn replace(doc: &Document, range: Range<usize>, text: &str) -> Transaction {
        let start = range.start.min(doc.len_chars());
        let end = range.end.min(doc.len_chars());
        let removed: String = if start < end {
            doc.text.slice(start..end).to_string()
        } else {
            String::new()
        };
        Transaction {
            changes: vec![Change {
                at: start,
                removed,
                inserted: text.to_string(),
            }],
        }
    }

    /// Apply this transaction to `doc`, returning its inverse. The inverse's offsets are
    /// in the *edited* document's coordinate space, so `inverse.apply(doc)` restores the
    /// original exactly. Re-applying the original reproduces the edit (redo).
    pub fn apply(&self, doc: &mut Document) -> Transaction {
        // Inverse changes, computed in edited-document coordinates.
        let mut inverse: Vec<Change> = Vec::with_capacity(self.changes.len());
        let mut delta: isize = 0;
        for ch in &self.changes {
            let at_edited = (ch.at as isize + delta) as usize;
            inverse.push(Change {
                at: at_edited,
                removed: ch.inserted.clone(),
                inserted: ch.removed.clone(),
            });
            delta += ch.delta();
        }

        // Apply forward, bottom-up, so earlier (original) offsets stay valid as we go.
        for ch in self.changes.iter().rev() {
            let start = ch.at;
            let end = ch.at + ch.removed_len();
            doc.apply_raw_remove(start, end);
            if !ch.inserted.is_empty() {
                doc.apply_raw_insert(start, &ch.inserted);
            }
        }

        // Keep the inverse in canonical (ascending) order.
        inverse.sort_by_key(|c| c.at);
        Transaction { changes: inverse }
    }

    /// Map a char offset from before this transaction to after it (position-following).
    /// Offsets at an insertion point move to *after* the inserted text.
    pub fn map_position(&self, pos: usize) -> usize {
        let mut result = pos as isize;
        for ch in &self.changes {
            let removed = ch.removed_len();
            let inserted = ch.inserted_len();
            if ch.at + removed <= pos {
                // Change is entirely before pos: shift by delta.
                result += ch.delta();
            } else if ch.at < pos {
                // pos falls inside the removed span: clamp to just past this change's inserted
                // text, in *edited* coordinates. `result - pos` is the delta accumulated from
                // all earlier changes (which shift this change's start too), so the edited start
                // of the change is `ch.at + (result - pos)`; add `inserted` to land after it.
                return (result + ch.at as isize - pos as isize + inserted as isize).max(0)
                    as usize;
            } else {
                // Change is at/after pos: no effect.
                break;
            }
        }
        result.max(0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_invert_restores() {
        let mut doc = Document::from_str("hello");
        let txn = Transaction::insert(&doc, 5, " world");
        let inv = txn.apply(&mut doc);
        assert_eq!(doc.to_string(), "hello world");
        inv.apply(&mut doc);
        assert_eq!(doc.to_string(), "hello");
    }

    #[test]
    fn delete_then_invert_restores() {
        let mut doc = Document::from_str("hello world");
        let txn = Transaction::delete(&doc, 5..11);
        let inv = txn.apply(&mut doc);
        assert_eq!(doc.to_string(), "hello");
        inv.apply(&mut doc);
        assert_eq!(doc.to_string(), "hello world");
    }

    #[test]
    fn redo_reproduces_edit() {
        let mut doc = Document::from_str("abc");
        let txn = Transaction::insert(&doc, 1, "XY");
        let inv = txn.apply(&mut doc);
        let edited = doc.to_string();
        inv.apply(&mut doc);
        txn.apply(&mut doc);
        assert_eq!(doc.to_string(), edited);
    }

    #[test]
    fn multi_change_applies_bottom_up() {
        let mut doc = Document::from_str("aXbYc");
        // Remove the X (at 1) and the Y (at 3) in one transaction.
        let txn = Transaction::from_changes(vec![
            Change {
                at: 1,
                removed: "X".into(),
                inserted: String::new(),
            },
            Change {
                at: 3,
                removed: "Y".into(),
                inserted: String::new(),
            },
        ]);
        let inv = txn.apply(&mut doc);
        assert_eq!(doc.to_string(), "abc");
        inv.apply(&mut doc);
        assert_eq!(doc.to_string(), "aXbYc");
    }

    #[test]
    fn map_position_follows_insert() {
        let doc = Document::from_str("hello world");
        let txn = Transaction::insert(&doc, 0, ">>");
        assert_eq!(txn.map_position(5), 7);
        assert_eq!(txn.map_position(0), 2);
    }

    #[test]
    fn map_position_in_removed_span_accounts_for_earlier_deltas() {
        // Two deletions: remove "aaa\n" at 0 and "ccc\n" at 8 from "aaa\nbbb\nccc\nddd\n".
        // A position inside the *second* removed span must map through the *first* delta too.
        let txn = Transaction::from_changes(vec![
            Change {
                at: 0,
                removed: "aaa\n".into(),
                inserted: String::new(),
            },
            Change {
                at: 8,
                removed: "ccc\n".into(),
                inserted: String::new(),
            },
        ]);
        // Offset 9 sits inside the deleted "ccc\n" [8,12). Result text is "bbb\nddd\n"; the
        // surviving line "ddd" starts at offset 4, not the buffer end (8).
        assert_eq!(txn.map_position(9), 4);
    }
}
