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
fn multi_cursor_word_backspace_inside_one_word_does_not_corrupt() {
    // Two carets inside the *same* word derive overlapping word-left delete ranges (0..3 and
    // 0..6). Transaction changes must stay non-overlapping, so the overlap merges into one
    // deletion of the union — the buffer keeps the tail and undo round-trips cleanly.
    let mut doc = Document::from_str("abcdefgh");
    multi_caret(&mut doc, &[3, 6]);
    delete_word_backward(&mut doc);
    assert_eq!(doc.to_string(), "gh");
    undo(&mut doc);
    assert_eq!(doc.to_string(), "abcdefgh");
}

#[test]
fn caret_backspacing_into_adjacent_selection_does_not_corrupt() {
    // A selection [2,5) and a bare caret at 5 (kept distinct — they only touch). Backspace makes
    // the caret reach back to 4, inside the selection's span; the ranges must not overlap.
    let mut doc = Document::from_str("abcdefgh");
    doc.selections = Selections::from_iter([Selection::new(2, 5), Selection::caret(5)]);
    delete_backward(&mut doc);
    assert_eq!(doc.to_string(), "abfgh"); // only [2,5) removed; the caret's char is subsumed
    undo(&mut doc);
    assert_eq!(doc.to_string(), "abcdefgh");
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
