use super::*;

#[test]
fn auto_pair_inserts_partner_with_caret_between() {
    let mut doc = Document::from_str("");
    doc.set_caret(0);
    insert_char_smart(&mut doc, '(', &pt(), true, true);
    assert_eq!(doc.to_string(), "()");
    assert_eq!(doc.selections.primary().head, 1); // caret between the parens
    assert!(doc.dirty);
}

#[test]
fn auto_pair_multi_cursor() {
    // Three carets at the three line starts (plan §1.1 acceptance).
    let mut doc = Document::from_str("a\nb\nc");
    multi_caret(&mut doc, &[0, 2, 4]);
    insert_char_smart(&mut doc, '(', &pt(), true, true);
    assert_eq!(doc.to_string(), "()a\n()b\n()c");
    assert_eq!(doc.selections.len(), 3);
    for s in doc.selections.ranges() {
        assert!(s.is_empty(), "each caret stays a caret");
        assert_eq!(doc.text.char(s.head - 1), '(');
        assert_eq!(doc.text.char(s.head), ')');
    }
}

#[test]
fn auto_pair_type_over_is_not_an_edit() {
    let mut doc = Document::from_str("()");
    doc.set_caret(1); // between the parens
    doc.dirty = false;
    insert_char_smart(&mut doc, ')', &pt(), true, true);
    assert_eq!(doc.to_string(), "()"); // no duplicate closer
    assert_eq!(doc.selections.primary().head, 2); // stepped past
    assert!(!doc.dirty, "stepping over a closer is not a buffer change");
}

#[test]
fn auto_pair_backspace_deletes_both() {
    let mut doc = Document::from_str("()");
    doc.set_caret(1);
    delete_backward_smart(&mut doc, &pt(), true);
    assert_eq!(doc.to_string(), "");
    assert_eq!(doc.selections.primary().head, 0);
}

#[test]
fn multi_cursor_backspace_never_overlaps() {
    // Regression: the empty-pair delete reaches back to head-1; with a second caret at
    // head+1 the two ranges must not overlap and corrupt the buffer. `()x` with carets
    // at 1 and 2 backspaces to `x` (both members gone, `x` preserved) — never empty.
    let mut doc = Document::from_str("()x");
    doc.selections = Selections::from_iter([Selection::caret(1), Selection::caret(2)]);
    delete_backward_smart(&mut doc, &pt(), true);
    assert_eq!(doc.to_string(), "x");
}

#[test]
fn quote_after_word_is_literal() {
    let mut doc = Document::from_str("don");
    doc.set_caret(3);
    insert_char_smart(&mut doc, '\'', &pt(), true, true);
    assert_eq!(doc.to_string(), "don'"); // not don''
                                         // At a boundary it still auto-closes.
    let mut doc = Document::from_str("");
    doc.set_caret(0);
    insert_char_smart(&mut doc, '"', &pt(), true, true);
    assert_eq!(doc.to_string(), "\"\"");
}

#[test]
fn surround_selection_with_pair() {
    let mut doc = Document::from_str("word");
    doc.selections = Selections::single(Selection::new(0, 4));
    insert_char_smart(&mut doc, '(', &pt(), true, true);
    assert_eq!(doc.to_string(), "(word)");
    let s = doc.selections.primary();
    assert_eq!((s.from(), s.to()), (1, 5)); // inner text stays selected
}

#[test]
fn auto_pairs_off_is_plain_insert() {
    let mut doc = Document::from_str("");
    doc.set_caret(0);
    insert_char_smart(&mut doc, '(', &pt(), false, false);
    assert_eq!(doc.to_string(), "(");
    assert_eq!(doc.selections.primary().head, 1);
}

#[test]
fn auto_pair_undo_removes_both_in_one_step() {
    let mut doc = Document::from_str("");
    doc.set_caret(0);
    insert_char_smart(&mut doc, '[', &pt(), true, true);
    assert_eq!(doc.to_string(), "[]");
    undo(&mut doc);
    assert_eq!(doc.to_string(), "");
}
