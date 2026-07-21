use super::*;

#[test]
fn horizontal_motion() {
    let doc = Document::from_str("hello");
    assert_eq!(resolve(&doc, 0, Motion::Right, 10), 1);
    assert_eq!(resolve(&doc, 5, Motion::Right, 10), 5);
    assert_eq!(resolve(&doc, 3, Motion::Left, 10), 2);
    assert_eq!(resolve(&doc, 0, Motion::Left, 10), 0);
}

#[test]
fn line_motions() {
    let doc = Document::from_str("  hi there\nnext");
    assert_eq!(resolve(&doc, 5, Motion::LineStart, 10), 0);
    assert_eq!(resolve(&doc, 0, Motion::LineFirstNonBlank, 10), 2);
    assert_eq!(resolve(&doc, 0, Motion::LineEnd, 10), 10);
}

#[test]
fn wrapped_up_down_move_by_visual_row() {
    // One logical line of 14 chars wraps at width 5 into rows "aaaa ", "bbbb ", "cccc"
    // (segment starts 0, 5, 10). Up/Down should step one visual row, preserving the column.
    let mut doc = Document::from_str("aaaa bbbb cccc");
    doc.view.wrap = true;
    doc.view.wrap_width = 5;

    // Column 0: Down walks row 0 → 1 → 2.
    assert_eq!(resolve(&doc, 0, Motion::Down, 10), 5);
    assert_eq!(resolve(&doc, 5, Motion::Down, 10), 10);
    assert_eq!(
        resolve(&doc, 10, Motion::Down, 10),
        10,
        "last visual row is a floor"
    );

    // Column 2 is preserved across the wrapped rows.
    assert_eq!(resolve(&doc, 2, Motion::Down, 10), 7); // row1 col2 = char 5+2
    assert_eq!(resolve(&doc, 7, Motion::Up, 10), 2); // back to row0 col2
    assert_eq!(
        resolve(&doc, 0, Motion::Up, 10),
        0,
        "first visual row is a ceiling"
    );
}

#[test]
fn wrapped_down_onto_full_row_does_not_skip() {
    // Regression: Down from the end of a full-width line onto a full-width non-final visual row
    // must land ON that row (its last char), not skip to the row after it.
    let mut doc = Document::from_str("aaaaa\nbbbbbbbbbb");
    doc.view.wrap = true;
    doc.view.wrap_width = 5;
    // Line 1 wraps into "bbbbb" (chars 6..11) and "bbbbb" (11..16). Down from char 5 (end of the
    // full line 0) must reach line 1's FIRST visual row, not skip into the second.
    let after_down = resolve(&doc, 5, Motion::Down, 10);
    let (line, _) = doc.char_to_line_col(after_down);
    assert_eq!(line, 1, "Down should reach line 1");
    assert!(
        after_down < 11,
        "must land on line 1's first visual row (chars 6..11), got {after_down}"
    );
}

#[test]
fn wrapped_home_end_snap_to_visual_row() {
    let mut doc = Document::from_str("aaaa bbbb cccc");
    doc.view.wrap = true;
    doc.view.wrap_width = 5;
    // Caret at char 7 is on visual row 1 ("bbbb ", chars 5..10).
    assert_eq!(resolve(&doc, 7, Motion::LineStart, 10), 5);
    assert_eq!(resolve(&doc, 7, Motion::LineEnd, 10), 10);
    // With wrap off, Home/End cover the whole logical line again.
    doc.view.wrap = false;
    assert_eq!(resolve(&doc, 7, Motion::LineStart, 10), 0);
    assert_eq!(resolve(&doc, 7, Motion::LineEnd, 10), 14);
}

#[test]
fn wrapped_down_crosses_into_the_next_logical_line() {
    // Row 0 = "aaaa " (chars 0..5) wraps; the logical line's last visual row is "bb" (5..7),
    // then Down crosses into the next logical line "next".
    let mut doc = Document::from_str("aaaa bb\nnext");
    doc.view.wrap = true;
    doc.view.wrap_width = 5;
    assert_eq!(resolve(&doc, 5, Motion::Down, 10), 8); // char 8 = start of "next"
    assert_eq!(resolve(&doc, 8, Motion::Up, 10), 5); // back up into row "bb"
}

#[test]
fn word_motions() {
    let doc = Document::from_str("foo bar_baz  qux");
    assert_eq!(resolve(&doc, 0, Motion::WordRight, 10), 4);
    assert_eq!(resolve(&doc, 4, Motion::WordRight, 10), 13);
    assert_eq!(resolve(&doc, 16, Motion::WordLeft, 10), 13);
}

#[test]
fn word_motions_over_punctuation_and_whitespace() {
    // Class boundaries: word | punct-run | whitespace. Exercises the rope-cursor walk that
    // replaced the whole-document Vec<char> allocation.
    let doc = Document::from_str("foo::bar  baz");
    // From 0: over "foo", then the "::" punct run is a separate stop.
    assert_eq!(resolve(&doc, 0, Motion::WordRight, 10), 3); // -> "::"
    assert_eq!(resolve(&doc, 3, Motion::WordRight, 10), 5); // "::" -> "bar"
    assert_eq!(resolve(&doc, 5, Motion::WordRight, 10), 10); // "bar" + gap -> "baz"
                                                             // WordLeft mirrors it.
    assert_eq!(resolve(&doc, 10, Motion::WordLeft, 10), 5); // back to "bar"
    assert_eq!(resolve(&doc, 5, Motion::WordLeft, 10), 3); // back to "::"
                                                           // WordEndRight lands on the end of the next run.
    assert_eq!(resolve(&doc, 0, Motion::WordEndRight, 10), 3); // end of "foo"
    assert_eq!(resolve(&doc, 3, Motion::WordEndRight, 10), 5); // end of "::"
                                                               // word_at over the punct run selects exactly "::".
    assert_eq!(matching_bracket(&doc, 0), None); // sanity: not a bracket
    let (s, e) = super::word_at(&doc, 4);
    assert_eq!((s, e), (3, 5));
}

#[test]
fn word_motion_edges_and_whitespace_starts() {
    let doc = Document::from_str("ab  cd");
    // WordRight starting *on* whitespace skips just the whitespace run.
    assert_eq!(resolve(&doc, 2, Motion::WordRight, 10), 4); // "  " -> "cd"
                                                            // WordRight from within the last run walks to end-of-buffer.
    assert_eq!(resolve(&doc, 4, Motion::WordRight, 10), 6);
    // WordLeft from the very start stays put; WordLeft skips leading whitespace.
    assert_eq!(resolve(&doc, 0, Motion::WordLeft, 10), 0);
    assert_eq!(resolve(&doc, 4, Motion::WordLeft, 10), 0); // back over "  " and "ab"
                                                           // WordEndRight over a trailing-whitespace tail and past end-of-buffer.
    assert_eq!(resolve(&doc, 6, Motion::WordEndRight, 10), 6); // already at end
                                                               // Motions clamp at/after the buffer end.
    assert_eq!(resolve(&doc, 6, Motion::WordRight, 10), 6);
    assert_eq!(resolve(&doc, 99, Motion::WordLeft, 10), 4); // past-end clamps, walks to "cd" start
                                                            // word_at on an empty document is a degenerate empty range.
    let empty = Document::from_str("");
    assert_eq!(super::word_at(&empty, 0), (0, 0));
}

#[test]
fn vertical_keeps_column() {
    let doc = Document::from_str("hello\nhi\nworld");
    // From col 4 on line 0, down to short line 1 clamps to its end.
    let pos = resolve(&doc, 4, Motion::Down, 10);
    let (line, _) = doc.char_to_line_col(pos);
    assert_eq!(line, 1);
}

#[test]
fn brackets_match() {
    let doc = Document::from_str("a(b(c)d)e");
    assert_eq!(resolve(&doc, 1, Motion::MatchingBracket, 10), 7);
    assert_eq!(resolve(&doc, 7, Motion::MatchingBracket, 10), 1);
}

#[test]
fn matching_bracket_all_kinds_and_edges() {
    let doc = Document::from_str("(a[b]c)");
    // Forward from each opener.
    assert_eq!(matching_bracket(&doc, 0), Some(6)); // ( -> )
    assert_eq!(matching_bracket(&doc, 2), Some(4)); // [ -> ]
                                                    // Backward from each closer.
    assert_eq!(matching_bracket(&doc, 6), Some(0)); // ) -> (
    assert_eq!(matching_bracket(&doc, 4), Some(2)); // ] -> [
                                                    // Non-bracket and out-of-range positions.
    assert_eq!(matching_bracket(&doc, 1), None);
    assert_eq!(matching_bracket(&doc, 99), None);
    // Braces, including nesting.
    let braces = Document::from_str("{{}}");
    assert_eq!(matching_bracket(&braces, 0), Some(3));
    assert_eq!(matching_bracket(&braces, 3), Some(0));
    // Unbalanced openers/closers return None.
    assert_eq!(matching_bracket(&Document::from_str("("), 0), None);
    assert_eq!(matching_bracket(&Document::from_str(")"), 0), None);
}

#[test]
fn matching_bracket_deep_nesting_and_offsets() {
    // A deeply nested run exercises the rope-cursor scan (no whole-doc Vec allocation) and
    // confirms the returned offsets stay correct far from `pos`, in both directions.
    let src = "(".repeat(500) + &")".repeat(500);
    let doc = Document::from_str(&src);
    // Outermost opener at 0 matches the last closer at 999; innermost pair is 499 <-> 500.
    assert_eq!(matching_bracket(&doc, 0), Some(999));
    assert_eq!(matching_bracket(&doc, 999), Some(0));
    assert_eq!(matching_bracket(&doc, 499), Some(500));
    assert_eq!(matching_bracket(&doc, 500), Some(499));
    // A bracket whose partner is missing (unbalanced tail) still returns None without panicking.
    let lopsided = Document::from_str("((()");
    assert_eq!(matching_bracket(&lopsided, 0), None); // no closer for the outermost opener
    assert_eq!(matching_bracket(&lopsided, 1), None); // still one unmatched opener remains
    assert_eq!(matching_bracket(&lopsided, 2), Some(3)); // the innermost pair balances
}
