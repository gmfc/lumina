//! Multi-cursor selection set.
//!
//! A document holds a *set* of selections, not one cursor (CLAUDE.md invariant #2).
//! The set is kept **sorted by span start, non-overlapping, and always non-empty**.
//! `normalize()` re-establishes those invariants after any mutation — call it religiously.

use std::ops::Range;

/// A single selection. `head` is the cursor; `anchor` is the fixed end.
/// Both are **char offsets** into the rope. A cursor with no selection has
/// `anchor == head`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    /// A caret (empty selection) at `pos`.
    pub fn caret(pos: usize) -> Self {
        Selection {
            anchor: pos,
            head: pos,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Selection { anchor, head }
    }

    /// Lower bound of the covered span, regardless of cursor direction.
    pub fn from(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// Upper bound (exclusive) of the covered span.
    pub fn to(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// Half-open span `[from, to)`.
    pub fn span(&self) -> Range<usize> {
        self.from()..self.to()
    }

    /// True when nothing is selected (a bare caret).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn len(&self) -> usize {
        self.to() - self.from()
    }
}

/// The selection set. `primary` indexes the selection that drives the viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selections {
    primary: usize,
    ranges: Vec<Selection>,
}

impl Selections {
    /// A fresh set with a single caret at the origin.
    pub fn single(sel: Selection) -> Self {
        Selections {
            primary: 0,
            ranges: vec![sel],
        }
    }

    /// Builder variant: consume `self`, normalize, return it.
    pub fn normalized(mut self) -> Self {
        self.normalize();
        self
    }

    /// The kept-sorted, non-overlapping ranges.
    pub fn ranges(&self) -> &[Selection] {
        &self.ranges
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// The primary selection (the one that drives scrolling / status).
    pub fn primary(&self) -> Selection {
        self.ranges[self.primary]
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        // A selection *set* is never truly empty (invariant); this reports "single caret".
        self.ranges.len() == 1 && self.ranges[0].is_empty()
    }

    /// Add a cursor. Overlaps are resolved by the next `normalize()`.
    pub fn push(&mut self, sel: Selection) {
        self.ranges.push(sel);
    }

    /// Replace all selections with a single one.
    pub fn set_single(&mut self, sel: Selection) {
        self.ranges.clear();
        self.ranges.push(sel);
        self.primary = 0;
    }

    /// Map every selection through `f`, then normalize. Used by motions/edits so
    /// multi-cursor is structural, not bolted on.
    pub fn transform<F: FnMut(Selection) -> Selection>(&mut self, mut f: F) {
        for s in &mut self.ranges {
            *s = f(*s);
        }
        self.normalize();
    }

    /// Re-establish the invariants: sorted by span start, merged on overlap,
    /// always at least one selection. The primary is tracked through the merge so
    /// it keeps pointing at the (possibly merged) selection it started on.
    pub fn normalize(&mut self) {
        if self.ranges.is_empty() {
            self.ranges.push(Selection::caret(0));
            self.primary = 0;
            return;
        }

        // Remember which concrete selection is primary so we can find it again.
        let primary_marker = self.ranges[self.primary.min(self.ranges.len() - 1)];

        // Sort by span start, then by span end.
        self.ranges
            .sort_by(|a, b| a.from().cmp(&b.from()).then(a.to().cmp(&b.to())));

        // Merge overlapping (not merely touching) selections.
        let mut merged: Vec<Selection> = Vec::with_capacity(self.ranges.len());
        for &cur in &self.ranges {
            if let Some(last) = merged.last_mut() {
                // Merge when the current span overlaps the last (starts strictly before it
                // ends), or when both are the *same* empty caret — two carets at one offset are
                // a single cursor, and keeping both would double every per-cursor edit.
                let coincident_carets =
                    cur.is_empty() && last.is_empty() && cur.from() == last.from();
                if cur.from() < last.to() || coincident_carets {
                    let forward = last.head >= last.anchor;
                    let lo = last.from().min(cur.from());
                    let hi = last.to().max(cur.to());
                    // Preserve the primary/last cursor's directionality when merging.
                    *last = if forward {
                        Selection::new(lo, hi)
                    } else {
                        Selection::new(hi, lo)
                    };
                    continue;
                }
            }
            merged.push(cur);
        }
        self.ranges = merged;

        // Re-find the primary: the merged selection whose span covers the old head.
        let target = primary_marker.head;
        self.primary = self
            .ranges
            .iter()
            .position(|s| s.from() <= target && target <= s.to())
            .unwrap_or(0);
    }

    /// Make the selection at `idx` primary (clamped).
    pub fn set_primary(&mut self, idx: usize) {
        self.primary = idx.min(self.ranges.len().saturating_sub(1));
    }
}

impl Default for Selections {
    fn default() -> Self {
        Selections::single(Selection::caret(0))
    }
}

impl FromIterator<Selection> for Selections {
    /// Collect selections and normalize immediately, so the result always upholds the
    /// sorted / non-overlapping / non-empty invariants.
    fn from_iter<I: IntoIterator<Item = Selection>>(iter: I) -> Self {
        let ranges: Vec<Selection> = iter.into_iter().collect();
        let mut s = Selections { primary: 0, ranges };
        s.normalize();
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_non_empty() {
        let s = Selections::from_iter(std::iter::empty());
        assert_eq!(s.ranges().len(), 1);
    }

    #[test]
    fn sorts_by_start() {
        let s = Selections::from_iter([
            Selection::caret(10),
            Selection::caret(2),
            Selection::caret(6),
        ]);
        let starts: Vec<usize> = s.ranges().iter().map(|r| r.from()).collect();
        assert_eq!(starts, vec![2, 6, 10]);
    }

    #[test]
    fn merges_overlaps_but_not_touching() {
        // [0,3) and [3,5) merely touch -> stay separate.
        let s = Selections::from_iter([Selection::new(0, 3), Selection::new(3, 5)]);
        assert_eq!(s.ranges().len(), 2);
        // [0,4) and [2,6) overlap -> merge to [0,6).
        let s = Selections::from_iter([Selection::new(0, 4), Selection::new(2, 6)]);
        assert_eq!(s.ranges().len(), 1);
        assert_eq!(s.ranges()[0].span(), 0..6);
    }

    #[test]
    fn coincident_carets_collapse_to_one() {
        // Two bare carets at the same offset are one cursor; keeping both would double edits.
        let s = Selections::from_iter([Selection::caret(3), Selection::caret(3)]);
        assert_eq!(s.ranges().len(), 1);
        assert_eq!(s.ranges()[0], Selection::caret(3));
        // Distinct carets are preserved.
        let s = Selections::from_iter([Selection::caret(1), Selection::caret(3)]);
        assert_eq!(s.ranges().len(), 2);
    }
}
