use super::*;

#[test]
fn duplicate_middle_and_last_line() {
    let mut doc = Document::from_str("a\nb\nc");
    doc.set_caret(doc.line_to_char(1)); // line "b"
    duplicate_line(&mut doc);
    assert_eq!(doc.to_string(), "a\nb\nb\nc");
    undo(&mut doc);
    assert_eq!(doc.to_string(), "a\nb\nc");
    // Last line (no trailing newline) duplicates with a fresh break.
    doc.set_caret(doc.len_chars());
    duplicate_line(&mut doc);
    assert_eq!(doc.to_string(), "a\nb\nc\nc");
}

#[test]
fn copy_line_up_middle_and_last() {
    let mut doc = Document::from_str("a\nb\nc");
    doc.set_caret(doc.line_to_char(1)); // line "b"
    copy_line_up(&mut doc);
    assert_eq!(doc.to_string(), "a\nb\nb\nc");
    undo(&mut doc);
    assert_eq!(doc.to_string(), "a\nb\nc");
    // Last line (no trailing newline) copies up with a fresh break after it.
    doc.set_caret(doc.len_chars());
    copy_line_up(&mut doc);
    assert_eq!(doc.to_string(), "a\nb\nc\nc");
}

#[test]
fn delete_lines_middle_first_last() {
    let mut doc = Document::from_str("a\nb\nc");
    doc.set_caret(doc.line_to_char(1)); // "b"
    delete_lines(&mut doc);
    assert_eq!(doc.to_string(), "a\nc");
    undo(&mut doc);
    assert_eq!(doc.to_string(), "a\nb\nc");
    // Deleting the last (newline-less) line eats the preceding break.
    doc.set_caret(doc.len_chars());
    delete_lines(&mut doc);
    assert_eq!(doc.to_string(), "a\nb");
    // Deleting the first line drops its trailing break too.
    doc.set_caret(0);
    delete_lines(&mut doc);
    assert_eq!(doc.to_string(), "b");
}

#[test]
fn delete_lines_keeps_trailing_newline() {
    let mut doc = Document::from_str("a\nb\n");
    doc.set_caret(doc.line_to_char(1)); // "b"
    delete_lines(&mut doc);
    assert_eq!(doc.to_string(), "a\n");
}

#[test]
fn delete_multiple_lines_via_selection() {
    let mut doc = Document::from_str("one\ntwo\nthree\nfour");
    // Selection spanning "two" and "three".
    doc.selections = Selections::single(Selection::new(4, 13));
    delete_lines(&mut doc);
    assert_eq!(doc.to_string(), "one\nfour");
}

#[test]
fn insert_line_below_copies_indent_and_moves_caret() {
    let mut doc = Document::from_str("    foo\nbar");
    doc.set_caret(2); // mid-word on the indented line
    insert_line_below(&mut doc);
    assert_eq!(doc.to_string(), "    foo\n    \nbar");
    // Caret sits at the end of the copied indent on the new line.
    let (line, col) = doc.char_to_line_col(doc.selections.primary().head);
    assert_eq!((line, col), (1, 4));
}

#[test]
fn insert_line_above_copies_indent() {
    let mut doc = Document::from_str("a\n    b");
    doc.set_caret(doc.len_chars()); // on "    b"
    insert_line_above(&mut doc);
    assert_eq!(doc.to_string(), "a\n    \n    b");
    let (line, col) = doc.char_to_line_col(doc.selections.primary().head);
    assert_eq!((line, col), (1, 4));
}

#[test]
fn cursors_to_line_ends_fans_out() {
    let mut doc = Document::from_str("aa\nbbb\nc");
    doc.selections = Selections::single(Selection::new(0, doc.len_chars()));
    cursors_to_line_ends(&mut doc);
    let ends: Vec<usize> = doc.selections.ranges().iter().map(|s| s.head).collect();
    // End of "aa" (2), end of "bbb" (6), end of "c" (8).
    assert_eq!(ends, vec![2, 6, 8]);
    assert!(doc.selections.ranges().iter().all(|s| s.is_empty()));
}

#[test]
fn toggle_comment_round_trips() {
    let mut doc = Document::from_str("  let x = 1;\n  let y = 2;");
    doc.selections = Selections::single(Selection::new(0, doc.len_chars()));
    toggle_comment(&mut doc, "//");
    assert_eq!(doc.to_string(), "  // let x = 1;\n  // let y = 2;");
    toggle_comment(&mut doc, "//");
    assert_eq!(doc.to_string(), "  let x = 1;\n  let y = 2;");
}

#[test]
fn indent_and_outdent_lines() {
    let mut doc = Document::from_str("a\nb");
    doc.selections = Selections::single(Selection::new(0, doc.len_chars()));
    indent(&mut doc);
    assert_eq!(doc.to_string(), "    a\n    b");
    outdent(&mut doc);
    assert_eq!(doc.to_string(), "a\nb");
}

#[test]
fn move_line_up_and_down() {
    let mut doc = Document::from_str("one\ntwo\nthree");
    doc.set_caret(doc.line_to_char(2)); // "three"
    move_lines(&mut doc, -1);
    assert_eq!(doc.to_string(), "one\nthree\ntwo");
    // Cursor rode the moved line up to line 1.
    assert_eq!(doc.char_to_line(doc.selections.primary().head), 1);
    move_lines(&mut doc, 1);
    assert_eq!(doc.to_string(), "one\ntwo\nthree");
}

#[test]
fn move_line_up_at_top_is_noop() {
    let mut doc = Document::from_str("one\ntwo");
    doc.set_caret(0);
    move_lines(&mut doc, -1);
    assert_eq!(doc.to_string(), "one\ntwo");
}

#[test]
fn move_line_down_with_trailing_newline() {
    let mut doc = Document::from_str("one\ntwo\n");
    doc.set_caret(0); // "one"
    move_lines(&mut doc, 1);
    assert_eq!(doc.to_string(), "two\none\n");
}
