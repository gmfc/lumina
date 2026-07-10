use super::*;
use crate::history::GroupBreak;
use crate::selection::{Selection, Selections};
use crate::transaction::Transaction;

#[test]
fn from_str_round_trips() {
    let d = Document::from_str("hello\nworld");
    assert_eq!(d.to_string(), "hello\nworld");
    assert_eq!(d.len_lines(), 2);
}

#[test]
fn crlf_detected_and_normalized() {
    let d = Document::from_str("a\r\nb\r\n");
    assert_eq!(d.line_ending, LineEnding::Crlf);
    assert_eq!(d.to_string(), "a\nb\n"); // stored as LF internally
}

#[test]
fn line_len_excludes_newline() {
    let d = Document::from_str("abc\nde");
    assert_eq!(d.line_len_chars(0), 3);
    assert_eq!(d.line_len_chars(1), 2);
}

/// Regression (invariant #1): an external reload replaces the whole buffer, so the undo
/// history — whose transactions were recorded against the *old* offsets — must be discarded.
/// Otherwise a later undo replays an edit at a now-out-of-range offset and restores wrong text.
#[test]
fn reload_discards_stale_undo_history() {
    let mut doc = Document::from_str("hello world\n");
    // Record an edit against the current buffer (insert "!!!" after "hello").
    let fwd = Transaction::insert(&doc, 5, "!!!");
    let inv = fwd.apply(&mut doc);
    doc.history.record(
        fwd,
        inv,
        Selections::single(Selection::caret(5)),
        Selections::single(Selection::caret(8)),
        GroupBreak::Force,
    );
    assert!(
        doc.history.can_undo(),
        "precondition: an edit is on the undo stack"
    );

    // The file changed on disk; reload to unrelated, shorter content.
    doc.reload_from_str("hi\n");
    assert_eq!(doc.to_string(), "hi\n");
    // The stale revision would insert at offset 5 into a 3-char buffer — reload must drop it.
    assert!(
        !doc.history.can_undo(),
        "reload must clear the now-stale undo history"
    );
    assert_eq!(doc.history.past_len(), 0);
}

/// Regression (invariant #2): `set_selections` normalizes at the boundary, so a set built with
/// `single` + `push` (unsorted, overlapping — as an external plugin might hand us) becomes a
/// sorted, non-overlapping set before any downstream edit relies on that shape.
#[test]
fn set_selections_normalizes_at_boundary() {
    let mut doc = Document::from_str("abcdefgh");
    let mut sel = Selections::single(Selection::new(4, 6));
    sel.push(Selection::new(0, 5)); // out of order and overlapping [4,6)
    doc.set_selections(sel);
    assert_eq!(doc.selections.ranges().len(), 1, "overlap merged");
    assert_eq!(
        doc.selections.ranges()[0].span(),
        0..6,
        "sorted + merged span"
    );
}
