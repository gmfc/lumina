#![cfg(feature = "proptests")]
//! PROPERTY: the multi-cursor selection set is always well-formed.
//! After construction and after any mutation + normalize, the set is (a) non-empty,
//! (b) sorted by range start, and (c) non-overlapping. This is the guardrail behind
//! invariant #2 (multi-cursor is structural) and prevents an entire class of edit bugs.
//!
//! Repo path:   crates/core/tests/selection_invariants.rs
//! Activation:  Phase 2 (selections + normalize). Enable via CI's `proptests` job.
//! Requires:    `proptests = []` in editor-core's [features]; `proptest` as a dev-dependency.
//!
//! Placeholder API — align with your final `editor_core`. Assumed: `Selection { anchor, head }`,
//! a `Selections` collection with `from_iter`, in-place `normalize()`, a `normalized()` builder,
//! a `push(Selection)` add-cursor op, and a `ranges() -> &[Selection]` accessor (kept sorted).

use editor_core::{Selection, Selections};
use proptest::prelude::*;

fn selection_strategy(max: usize) -> impl Strategy<Value = Selection> {
    (0..=max, 0..=max).prop_map(|(anchor, head)| Selection { anchor, head })
}

/// Half-open span [lo, hi) of a selection regardless of cursor direction.
fn span(s: &Selection) -> (usize, usize) {
    (s.anchor.min(s.head), s.anchor.max(s.head))
}

fn check_invariants(sel: &Selections) -> Result<(), String> {
    let ranges = sel.ranges();
    if ranges.is_empty() {
        return Err("selection set is empty (must always hold >= 1)".into());
    }
    for pair in ranges.windows(2) {
        let (a_lo, a_hi) = span(&pair[0]);
        let (b_lo, _) = span(&pair[1]);
        if a_lo > b_lo {
            return Err(format!(
                "not sorted by start: {:?} then {:?}",
                pair[0], pair[1]
            ));
        }
        // NOTE: this treats *touching* ranges (a_hi == b_lo) as allowed. If your design
        // merges touching selections/cursors, tighten this to `a_hi >= b_lo`.
        if a_hi > b_lo {
            return Err(format!("overlap: {:?} then {:?}", pair[0], pair[1]));
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn normalize_yields_sorted_nonoverlapping_nonempty(
        raw in prop::collection::vec(selection_strategy(1000), 1..12),
    ) {
        let sel = Selections::from_iter(raw).normalized();
        check_invariants(&sel).map_err(|e| TestCaseError::fail(e))?;
    }

    #[test]
    fn adding_a_cursor_preserves_invariants(
        raw in prop::collection::vec(selection_strategy(1000), 1..12),
        extra in selection_strategy(1000),
    ) {
        let mut sel = Selections::from_iter(raw).normalized();
        sel.push(extra);
        sel.normalize();
        check_invariants(&sel).map_err(|e| TestCaseError::fail(e))?;
    }
}
