#![cfg(feature = "proptests")]
//! PROPERTY: within a line, char -> display-column -> char is the identity.
//! Mapping a char index to the display column where it *starts*, then mapping that column
//! back, returns the same char index — for every char, including wide (CJK/emoji) chars and
//! tab expansion. This is the guardrail behind invariant #6 (`screen_to_char` correctness),
//! the single most bug-prone calculation in the editor.
//!
//! NOTE ON DIRECTION: only char -> col -> char is identity. The reverse (col -> char -> col)
//! is deliberately NOT, because clicking the second cell of a wide char resolves to that char,
//! whose start column is the first cell. That asymmetry is correct; don't "fix" it.
//!
//! Repo path:   crates/core/tests/coordinate_mapping.rs
//! Activation:  Phase 3 (mouse / coordinate mapping). Enable via CI's `proptests` job.
//! Requires:    `proptests = []` in editor-core's [features]; `proptest` as a dev-dependency.
//!
//! Placeholder API. These must be PURE functions in a library crate (not the `app` binary) so
//! they're unit-testable — see invariant #6. Assumed signatures, operating on one line's text:
//!   editor_core::view::char_to_display_col(line: &str, char_idx: usize, tab_width: usize) -> usize
//!   editor_core::view::display_col_to_char(line: &str, col: usize,      tab_width: usize) -> usize
//! In production these take a `RopeSlice`; add a `&str` shim or convert here.

use editor_core::view::{char_to_display_col, display_col_to_char};
use proptest::prelude::*;

/// A single logical line (no line breaks) paired with a valid char index into it.
fn line_and_index() -> impl Strategy<Value = (String, usize)> {
    "[^\\n\\r]{0,120}".prop_flat_map(|line| {
        let n = line.chars().count();
        (Just(line), 0..=n)
    })
}

proptest! {
    #[test]
    fn char_to_col_to_char_is_identity(
        (line, idx) in line_and_index(),
        tab_width in 1usize..=8,
    ) {
        let col = char_to_display_col(&line, idx, tab_width);
        let back = display_col_to_char(&line, col, tab_width);
        prop_assert_eq!(back, idx,
            "char->col->char broke: line={:?} idx={} col={} back={}", line, idx, col, back);
    }

    #[test]
    fn display_columns_are_monotonic(
        line in "[^\\n\\r]{0,120}",
        tab_width in 1usize..=8,
    ) {
        let n = line.chars().count();
        let mut prev = 0usize;
        for idx in 0..=n {
            let col = char_to_display_col(&line, idx, tab_width);
            prop_assert!(col >= prev, "display column decreased at char {}", idx);
            prev = col;
        }
    }
}
