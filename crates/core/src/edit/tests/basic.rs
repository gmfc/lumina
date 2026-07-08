use super::*;

#[test]
fn insert_at_single_caret() {
    let mut doc = Document::from_str("hello");
    doc.set_caret(5);
    insert_text(&mut doc, "!", GroupBreak::Force);
    assert_eq!(doc.to_string(), "hello!");
}

#[test]
fn multi_cursor_insert_keeps_offsets_valid() {
    let mut doc = Document::from_str("a\nb\nc");
    // carets at start of each line: offsets 0, 2, 4
    multi_caret(&mut doc, &[0, 2, 4]);
    insert_text(&mut doc, "> ", GroupBreak::Force);
    assert_eq!(doc.to_string(), "> a\n> b\n> c");
    // three carets, each after its inserted "> "
    assert_eq!(doc.selections.len(), 3);
}

#[test]
fn backspace_then_undo() {
    let mut doc = Document::from_str("hello");
    doc.set_caret(5);
    delete_backward(&mut doc);
    assert_eq!(doc.to_string(), "hell");
    undo(&mut doc);
    assert_eq!(doc.to_string(), "hello");
    assert_eq!(doc.selections.primary().head, 5);
}

#[test]
fn typing_burst_undoes_together() {
    let mut doc = Document::from_str("");
    doc.set_caret(0);
    insert_char(&mut doc, 'h');
    insert_char(&mut doc, 'i');
    assert_eq!(doc.to_string(), "hi");
    undo(&mut doc);
    assert_eq!(doc.to_string(), "");
}

#[test]
fn delete_word_backward_removes_word() {
    let mut doc = Document::from_str("hello world");
    doc.set_caret(11);
    delete_word_backward(&mut doc);
    assert_eq!(doc.to_string(), "hello ");
    assert_eq!(doc.selections.primary().head, 6);
}

#[test]
fn select_word_and_line() {
    let mut doc = Document::from_str("foo bar\nbaz");
    doc.set_caret(5); // inside "bar"
    select_word(&mut doc);
    let s = doc.selections.primary();
    assert_eq!((s.from(), s.to()), (4, 7));
    doc.set_caret(1);
    select_line(&mut doc);
    let s = doc.selections.primary();
    assert_eq!((s.from(), s.to()), (0, 8)); // "foo bar\n"
}
