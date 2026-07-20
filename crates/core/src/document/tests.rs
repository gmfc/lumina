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

#[test]
fn line_str_matches_line_text() {
    // `line_str` is the borrowing twin of `line_text` used on the render hot path; it must
    // return byte-for-byte the same content (including the trailing newline) for every line,
    // and an empty string for an out-of-range index — same as `line_text`.
    let d = Document::from_str("abc\nde\n");
    for line in 0..=d.len_lines() {
        assert_eq!(&*d.line_str(line), d.line_text(line), "line {line}");
    }
    assert_eq!(&*d.line_str(0), "abc\n");
    assert_eq!(&*d.line_str(999), ""); // out of range → empty, no panic
}

#[test]
fn line_str_owns_when_line_straddles_chunks() {
    // A single line longer than ropey's chunk size spans multiple chunks, so `as_str()` yields
    // `None` and `line_str` must fall back to an owned copy that still matches `line_text`.
    let long = "x".repeat(8192);
    let d = Document::from_str(&long);
    assert_eq!(&*d.line_str(0), d.line_text(0));
    assert_eq!(d.line_str(0).len(), 8192);
}

/// Release-timing A/B for finding #3 (borrow line text on the render hot path). Behind the
/// `perfbench` feature (so the coverage build skips it) and ignored by default; run with
/// `cargo test -p editor-core --features perfbench --release -- --ignored --nocapture bench_line`.
/// Times the owned `line_text` against the borrowing `line_str` over the same corpus in one run,
/// so the before/after is directly comparable on the same machine.
#[cfg(feature = "perfbench")]
#[test]
#[ignore = "timing harness; run explicitly with --ignored --nocapture"]
fn bench_line_text_vs_line_str() {
    use std::hint::black_box;
    use std::time::Instant;

    // A realistic buffer: 2000 lines of ~60 chars — the render loop touches one viewport of
    // these per frame, so per-line allocation cost compounds.
    let body: String = (0..2000)
        .map(|i| format!("    let value_{i} = compute(x, y) + offset; // row {i}\n"))
        .collect();
    let d = Document::from_str(&body);
    let lines = d.len_lines();
    const REPS: usize = 200;

    let t0 = Instant::now();
    let mut sink = 0usize;
    for _ in 0..REPS {
        for line in 0..lines {
            let s = d.line_text(line);
            sink += black_box(s.len());
        }
    }
    let owned = t0.elapsed();

    let t1 = Instant::now();
    for _ in 0..REPS {
        for line in 0..lines {
            let s = d.line_str(line);
            sink += black_box(s.len());
        }
    }
    let borrowed = t1.elapsed();

    black_box(sink);
    let calls = (REPS * lines) as u128;
    println!(
        "line_text (owned):  {owned:?}  ({} ns/call)\nline_str  (borrow): {borrowed:?}  ({} ns/call)",
        owned.as_nanos() / calls,
        borrowed.as_nanos() / calls,
    );
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
