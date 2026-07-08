use super::*;

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
