#![cfg(feature = "proptests")]
//! PROPERTY: a transaction is exactly reversible.
//! For any buffer and any edit, `apply(txn)` followed by `apply(inverse)` restores the
//! original buffer; re-applying `txn` reproduces the edited buffer. This is what makes
//! undo/redo correct and is the guardrail behind invariant #1 (all mutation via transactions).
//!
//! Repo path:   crates/core/tests/transaction_roundtrip.rs
//! Activation:  Phase 2 (core editing + undo). Enable via CI's `proptests` job.
//! Requires:    `proptests = []` in editor-core's [features]; `proptest` as a dev-dependency.
//!
//! The SHAPE of the property is the contract. The exact constructor/method names below are
//! placeholders — align them with your final `editor_core` API (or align the API to them).
//! Assumed crate name: `editor_core` (do NOT name the crate `core`; it shadows std::core).

use editor_core::{Document, Transaction};
use proptest::prelude::*;

/// A randomly generated edit valid against a buffer of `char_len` characters.
#[derive(Debug, Clone)]
enum Edit {
    Insert { at: usize, text: String },
    Delete { start: usize, end: usize },
}

fn edit_strategy(char_len: usize) -> BoxedStrategy<Edit> {
    // Insert at any char boundary, including the very end.
    let insert = (0..=char_len, "[^\\n\\r]{0,8}")
        .prop_map(|(at, text)| Edit::Insert { at, text })
        .boxed();

    if char_len == 0 {
        return insert;
    }
    // Delete a (possibly empty) char range [start, end).
    let delete = (0..char_len)
        .prop_flat_map(move |start| (Just(start), start..=char_len))
        .prop_map(|(start, end)| Edit::Delete { start, end })
        .boxed();

    prop_oneof![insert, delete].boxed()
}

/// Pair a random buffer with an edit whose positions are valid for that buffer.
fn buffer_and_edit() -> impl Strategy<Value = (String, Edit)> {
    "[^\\n\\r]{0,200}".prop_flat_map(|initial| {
        let n = initial.chars().count();
        (Just(initial), edit_strategy(n))
    })
}

proptest! {
    #[test]
    fn apply_then_invert_is_identity((initial, edit) in buffer_and_edit()) {
        let mut doc = Document::from_str(&initial);
        let original = doc.to_string();

        let txn = match edit {
            Edit::Insert { at, text } => Transaction::insert(&doc, at, &text),
            Edit::Delete { start, end } => Transaction::delete(&doc, start..end),
        };

        // apply → the buffer changes; the returned inverse must restore it exactly.
        let inverse = txn.apply(&mut doc);
        let edited = doc.to_string();
        inverse.apply(&mut doc);
        prop_assert_eq!(doc.to_string(), original.clone(),
            "inverse did not restore the buffer");

        // redo: re-applying the original transaction reproduces the edited buffer.
        let _ = txn.apply(&mut doc);
        prop_assert_eq!(doc.to_string(), edited,
            "re-applying the transaction did not reproduce the edit");
    }
}
