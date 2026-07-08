use super::*;

#[test]
fn newline_copies_indent() {
    let mut doc = Document::from_str("    foo");
    doc.set_caret(7);
    insert_newline_smart(&mut doc, &pt(), true);
    assert_eq!(doc.to_string(), "    foo\n    ");
    assert_eq!(doc.selections.primary().head, doc.len_chars());
}

#[test]
fn newline_after_open_brace_indents() {
    let mut doc = Document::from_str("fn f() {");
    doc.set_caret(8);
    insert_newline_smart(&mut doc, &pt(), true);
    assert_eq!(doc.to_string(), "fn f() {\n    ");
}

#[test]
fn newline_between_braces_expands() {
    let mut doc = Document::from_str("{}");
    doc.set_caret(1);
    insert_newline_smart(&mut doc, &pt(), true);
    assert_eq!(doc.to_string(), "{\n    \n}");
    // Caret sits on the indented middle line.
    assert_eq!(doc.char_to_line(doc.selections.primary().head), 1);
}

#[test]
fn typing_close_brace_dedents_whitespace_line() {
    let mut doc = Document::from_str("fn f() {\n    ");
    doc.set_caret(doc.len_chars());
    insert_char_smart(&mut doc, '}', &pt(), true, true);
    assert_eq!(doc.to_string(), "fn f() {\n}");
}

#[test]
fn newline_auto_indent_off_is_bare() {
    let mut doc = Document::from_str("    foo");
    doc.set_caret(7);
    insert_newline_smart(&mut doc, &pt(), false);
    assert_eq!(doc.to_string(), "    foo\n");
}
