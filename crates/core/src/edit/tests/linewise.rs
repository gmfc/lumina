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
