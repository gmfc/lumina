use super::*;

#[test]
fn save_hygiene_trims_and_moves_caret_off_eol() {
    let mut doc = Document::from_str("foo   \nbar\t\nbaz");
    doc.set_caret(6); // just past "foo", at the old trailing-space EOL of line 0
    let changed = apply_save_hygiene(&mut doc, true, false);
    assert!(changed);
    assert_eq!(doc.to_string(), "foo\nbar\nbaz");
    // The caret is pulled back to the new EOL, never left past it.
    assert_eq!(doc.selections.primary().head, 3);
    undo(&mut doc);
    assert_eq!(doc.to_string(), "foo   \nbar\t\nbaz");
}

#[test]
fn save_hygiene_inserts_final_newline_only_when_missing() {
    let mut doc = Document::from_str("abc");
    assert!(apply_save_hygiene(&mut doc, false, true));
    assert_eq!(doc.to_string(), "abc\n");
    // Idempotent: a file already ending in a newline is untouched.
    let mut doc = Document::from_str("abc\n");
    assert!(!apply_save_hygiene(&mut doc, false, true));
    assert_eq!(doc.to_string(), "abc\n");
}

#[test]
fn save_hygiene_noop_when_both_off() {
    let mut doc = Document::from_str("foo  \n");
    assert!(!apply_save_hygiene(&mut doc, false, false));
    assert_eq!(doc.to_string(), "foo  \n");
}

#[test]
fn save_hygiene_preserves_crlf_line_ending() {
    // Trimming operates on internal LF text; the file's CRLF style is a Document flag that
    // serialization re-emits — hygiene must not touch it (invariant #6).
    let mut doc = Document::from_str("foo  \r\nbar");
    assert_eq!(doc.line_ending, crate::document::LineEnding::Crlf);
    apply_save_hygiene(&mut doc, true, true);
    assert_eq!(doc.to_string(), "foo\nbar\n"); // internal LF, trimmed, final newline
    assert_eq!(doc.line_ending, crate::document::LineEnding::Crlf);
}
