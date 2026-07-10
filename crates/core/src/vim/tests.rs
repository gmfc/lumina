use super::*;
use crate::document::Document;

fn doc(s: &str) -> Document {
    Document::from_str(s)
}

#[test]
fn word_start_small_and_big() {
    let d = doc("foo.bar baz");
    // small word: `.` is its own word.
    assert_eq!(next_word_start(&d, 0, false), 3); // -> '.'
    assert_eq!(next_word_start(&d, 3, false), 4); // -> 'bar'
    assert_eq!(next_word_start(&d, 4, false), 8); // -> 'baz'
                                                  // WORD: whitespace-delimited, so `foo.bar` is one WORD.
    assert_eq!(next_word_start(&d, 0, true), 8); // -> 'baz'
}

#[test]
fn word_end_forward() {
    let d = doc("foo bar");
    assert_eq!(next_word_end(&d, 0, false), 2); // last char of 'foo'
    assert_eq!(next_word_end(&d, 2, false), 6); // last char of 'bar'
}

#[test]
fn word_back() {
    let d = doc("foo bar baz");
    assert_eq!(prev_word_start(&d, 10, false), 8); // within 'baz' -> its start
    assert_eq!(prev_word_start(&d, 8, false), 4); // start of 'baz' -> start of 'bar'
    assert_eq!(prev_word_start(&d, 4, false), 0);
}

#[test]
fn prev_word_end_ge() {
    let d = doc("foo bar baz");
    // From inside 'baz' (index 10) -> end of 'bar' (the 'r' at 6).
    assert_eq!(prev_word_end(&d, 10, false), 6);
    // From the start of 'bar' (index 4) -> end of 'foo' (the second 'o' at 2).
    assert_eq!(prev_word_end(&d, 4, false), 2);
}

#[test]
fn find_char_forward_and_back() {
    let d = doc("abcdefb");
    assert_eq!(find_char(&d, 0, 'd', FindKind::Find), Some(3));
    assert_eq!(find_char(&d, 0, 'd', FindKind::Till), Some(2));
    assert_eq!(find_char(&d, 6, 'b', FindKind::FindBack), Some(1));
    assert_eq!(find_char(&d, 6, 'b', FindKind::TillBack), Some(2));
    assert_eq!(find_char(&d, 0, 'z', FindKind::Find), None);
}

#[test]
fn find_char_is_line_local() {
    let d = doc("abc\ndef");
    // 'd' is on the next line — not found from the first line.
    assert_eq!(find_char(&d, 0, 'd', FindKind::Find), None);
}

#[test]
fn last_non_blank_ignores_trailing_ws() {
    let d = doc("  hi   ");
    assert_eq!(last_non_blank(&d, 0), 3); // 'i' at index 3
    assert_eq!(first_non_blank(&d, 0), 2); // 'h' at index 2
}

#[test]
fn paragraph_motions() {
    let d = doc("a\nb\n\nc\nd\n");
    // From line 0, `}` lands on the blank line (index of '\n\n' start).
    let blank = d.line_to_char(2);
    assert_eq!(paragraph_forward(&d, 0), blank);
    // `{` from line 3 lands back on the blank line.
    let from = d.line_to_char(3);
    assert_eq!(paragraph_backward(&d, from), d.line_to_char(2));
}

#[test]
fn word_object_inner_and_around() {
    let d = doc("foo bar");
    assert_eq!(
        text_object(&d, 1, TextObject::Word { big: false }, false),
        Some((0, 3))
    );
    // `aw` grabs the trailing space.
    assert_eq!(
        text_object(&d, 1, TextObject::Word { big: false }, true),
        Some((0, 4))
    );
}

#[test]
fn pair_object_from_inside() {
    let d = doc("a(bc)d");
    let inner = text_object(
        &d,
        3,
        TextObject::Pair {
            open: '(',
            close: ')',
        },
        false,
    );
    assert_eq!(inner, Some((2, 4))); // "bc"
    let around = text_object(
        &d,
        3,
        TextObject::Pair {
            open: '(',
            close: ')',
        },
        true,
    );
    assert_eq!(around, Some((1, 5))); // "(bc)"
}

#[test]
fn pair_object_nested() {
    let d = doc("(a(b)c)");
    // Cursor on 'b' (index 3) selects the inner pair.
    assert_eq!(
        text_object(
            &d,
            3,
            TextObject::Pair {
                open: '(',
                close: ')'
            },
            false
        ),
        Some((3, 4))
    );
    // Cursor on 'a' (index 1) selects the outer pair's contents.
    assert_eq!(
        text_object(
            &d,
            1,
            TextObject::Pair {
                open: '(',
                close: ')'
            },
            false
        ),
        Some((1, 6))
    );
}

#[test]
fn pair_object_on_bracket() {
    let d = doc("a(bc)d");
    // Cursor on the opening '(' resolves the same pair.
    assert_eq!(
        text_object(
            &d,
            1,
            TextObject::Pair {
                open: '(',
                close: ')'
            },
            false
        ),
        Some((2, 4))
    );
    // Cursor on the closing ')' too.
    assert_eq!(
        text_object(
            &d,
            4,
            TextObject::Pair {
                open: '(',
                close: ')'
            },
            false
        ),
        Some((2, 4))
    );
}

#[test]
fn quote_object_inner_and_around() {
    let d = doc("x \"hi\" y");
    // Indices: x=0 ' '=1 '"'=2 h=3 i=4 '"'=5 ' '=6 y=7
    assert_eq!(
        text_object(&d, 4, TextObject::Quote { quote: '"' }, false),
        Some((3, 5))
    );
    // `a"` includes the quotes and the trailing space.
    assert_eq!(
        text_object(&d, 4, TextObject::Quote { quote: '"' }, true),
        Some((2, 7))
    );
}

#[test]
fn paragraph_object_lines() {
    let d = doc("a\nb\n\nc\n");
    // ip on line 0 covers lines 0..=1 (through the newline after 'b').
    let start = d.line_to_char(0);
    let end = d.line_to_char(2);
    assert_eq!(
        text_object(&d, 0, TextObject::Paragraph, false),
        Some((start, end))
    );
    // ap also swallows the trailing blank line.
    let end_ap = d.line_to_char(3);
    assert_eq!(
        text_object(&d, 0, TextObject::Paragraph, true),
        Some((start, end_ap))
    );
}

#[test]
fn missing_pair_returns_none() {
    let d = doc("no brackets here");
    assert_eq!(
        text_object(
            &d,
            3,
            TextObject::Pair {
                open: '(',
                close: ')'
            },
            false
        ),
        None
    );
}
