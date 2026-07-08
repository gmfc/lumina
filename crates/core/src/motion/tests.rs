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
fn word_motions() {
    let doc = Document::from_str("foo bar_baz  qux");
    assert_eq!(resolve(&doc, 0, Motion::WordRight, 10), 4);
    assert_eq!(resolve(&doc, 4, Motion::WordRight, 10), 13);
    assert_eq!(resolve(&doc, 16, Motion::WordLeft, 10), 13);
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
