//! Motions: pure functions from a char offset to a new char offset over a [`Document`].
//!
//! Motions never mutate. Commands apply a motion to *every* selection (multi-cursor is
//! structural), collapsing or extending as appropriate.

use crate::document::Document;

/// A cursor motion. Resolved by [`resolve`] against a document + starting offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    WordEndRight,
    LineStart,
    LineEnd,
    LineFirstNonBlank,
    DocStart,
    DocEnd,
    PageUp,
    PageDown,
    MatchingBracket,
}

/// Character class for word motions.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Whitespace,
    Word,
    Punct,
}

fn class_of(ch: char) -> Class {
    if ch.is_whitespace() {
        Class::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Resolve `motion` from char offset `pos`. `page` is the viewport height in lines
/// (used by PageUp/PageDown). Returns the new char offset, clamped to the document.
pub fn resolve(doc: &Document, pos: usize, motion: Motion, page: usize) -> usize {
    let len = doc.len_chars();
    let pos = pos.min(len);
    match motion {
        Motion::Left => prev_grapheme(doc, pos),
        Motion::Right => next_grapheme(doc, pos),
        Motion::Up => vertical(doc, pos, -1),
        Motion::Down => vertical(doc, pos, 1),
        Motion::WordLeft => word_left(doc, pos),
        Motion::WordRight => word_right(doc, pos),
        Motion::WordEndRight => word_end_right(doc, pos),
        Motion::LineStart => {
            let line = doc.char_to_line(pos);
            doc.line_to_char(line)
        }
        Motion::LineEnd => {
            let line = doc.char_to_line(pos);
            doc.line_to_char(line) + doc.line_len_chars(line)
        }
        Motion::LineFirstNonBlank => first_non_blank(doc, pos),
        Motion::DocStart => 0,
        Motion::DocEnd => len,
        Motion::PageUp => vertical(doc, pos, -(page.max(1) as isize)),
        Motion::PageDown => vertical(doc, pos, page.max(1) as isize),
        Motion::MatchingBracket => matching_bracket(doc, pos).unwrap_or(pos),
    }
}

fn next_grapheme(doc: &Document, pos: usize) -> usize {
    if pos >= doc.len_chars() {
        return pos;
    }
    // Grapheme-aware step: cluster from the current line's text.
    let line = doc.char_to_line(pos);
    let line_start = doc.line_to_char(line);
    let text = doc.line_text(line);
    let in_line = pos - line_start;
    let mut byte = char_to_byte(&text, in_line);
    // If at line end (before newline), just advance one char.
    let stepped = grapheme_after(&text, byte);
    if let Some(next_byte) = stepped {
        byte = next_byte;
        line_start + byte_to_char(&text, byte)
    } else {
        (pos + 1).min(doc.len_chars())
    }
}

fn prev_grapheme(doc: &Document, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let line = doc.char_to_line(pos);
    let line_start = doc.line_to_char(line);
    if pos == line_start {
        // Move to end of previous line's content (before its newline).
        return pos - 1;
    }
    let text = doc.line_text(line);
    let in_line = pos - line_start;
    let byte = char_to_byte(&text, in_line);
    if let Some(prev_byte) = grapheme_before(&text, byte) {
        line_start + byte_to_char(&text, prev_byte)
    } else {
        pos - 1
    }
}

fn grapheme_after(text: &str, byte: usize) -> Option<usize> {
    let mut cursor = unicode_segmentation::GraphemeCursor::new(byte, text.len(), true);
    cursor.next_boundary(text, 0).ok().flatten()
}

fn grapheme_before(text: &str, byte: usize) -> Option<usize> {
    let mut cursor = unicode_segmentation::GraphemeCursor::new(byte, text.len(), true);
    cursor.prev_boundary(text, 0).ok().flatten()
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Move `delta` lines, preserving the sticky goal column (in display cells).
fn vertical(doc: &Document, pos: usize, delta: isize) -> usize {
    let (line, _col) = doc.char_to_line_col(pos);
    let goal = doc
        .view
        .goal_col
        .unwrap_or_else(|| current_display_col(doc, pos));
    let target_line = (line as isize + delta).clamp(0, doc.len_lines() as isize - 1) as usize;
    let line_text = doc.line_text(target_line);
    let line_text = line_text.trim_end_matches(['\n', '\r']);
    let char_in_line = crate::view::display_col_to_char(line_text, goal, doc.tab_width);
    doc.line_to_char(target_line) + char_in_line
}

fn current_display_col(doc: &Document, pos: usize) -> usize {
    let (line, col) = doc.char_to_line_col(pos);
    let text = doc.line_text(line);
    let text = text.trim_end_matches(['\n', '\r']);
    crate::view::char_to_display_col(text, col, doc.tab_width)
}

fn first_non_blank(doc: &Document, pos: usize) -> usize {
    let line = doc.char_to_line(pos);
    let start = doc.line_to_char(line);
    let text = doc.line_text(line);
    for (i, ch) in text.chars().enumerate() {
        if !ch.is_whitespace() {
            return start + i;
        }
    }
    start
}

fn word_left(doc: &Document, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let chars: Vec<char> = doc.text.chars().collect();
    let mut i = pos;
    // Skip whitespace to the left.
    while i > 0 && class_of(chars[i - 1]) == Class::Whitespace {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let cls = class_of(chars[i - 1]);
    while i > 0 && class_of(chars[i - 1]) == cls {
        i -= 1;
    }
    i
}

fn word_right(doc: &Document, pos: usize) -> usize {
    let chars: Vec<char> = doc.text.chars().collect();
    let n = chars.len();
    let mut i = pos;
    if i >= n {
        return n;
    }
    let cls = class_of(chars[i]);
    if cls != Class::Whitespace {
        while i < n && class_of(chars[i]) == cls {
            i += 1;
        }
    }
    while i < n && class_of(chars[i]) == Class::Whitespace {
        i += 1;
    }
    i
}

fn word_end_right(doc: &Document, pos: usize) -> usize {
    let chars: Vec<char> = doc.text.chars().collect();
    let n = chars.len();
    let mut i = pos;
    if i >= n {
        return n;
    }
    i += 1;
    while i < n && class_of(chars[i]) == Class::Whitespace {
        i += 1;
    }
    if i >= n {
        return n;
    }
    let cls = class_of(chars[i]);
    while i < n && class_of(chars[i]) == cls {
        i += 1;
    }
    i
}

/// The `[start, end)` char range of the word (or whitespace/punct run) containing `pos`.
/// Used by double-click word selection.
pub fn word_at(doc: &Document, pos: usize) -> (usize, usize) {
    let chars: Vec<char> = doc.text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return (0, 0);
    }
    let idx = pos.min(n - 1);
    let cls = class_of(chars[idx]);
    let mut start = idx;
    while start > 0 && class_of(chars[start - 1]) == cls {
        start -= 1;
    }
    let mut end = idx;
    while end < n && class_of(chars[end]) == cls {
        end += 1;
    }
    (start, end)
}

/// The `[start, end)` char range of the whole line containing `pos`, including its
/// trailing newline (so triple-click selects the line and its break). Used by triple-click.
pub fn line_at(doc: &Document, pos: usize) -> (usize, usize) {
    let line = doc.char_to_line(pos);
    let start = doc.line_to_char(line);
    let end = if line + 1 < doc.len_lines() {
        doc.line_to_char(line + 1)
    } else {
        doc.len_chars()
    };
    (start, end)
}

fn matching_bracket(doc: &Document, pos: usize) -> Option<usize> {
    let chars: Vec<char> = doc.text.chars().collect();
    let n = chars.len();
    if pos >= n {
        return None;
    }
    let ch = chars[pos];
    let (open, close, forward) = match ch {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _ => return None,
    };
    let mut depth = 0isize;
    if forward {
        let mut i = pos;
        while i < n {
            if chars[i] == open {
                depth += 1;
            } else if chars[i] == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            i += 1;
        }
    } else {
        let mut i = pos as isize;
        while i >= 0 {
            let c = chars[i as usize];
            if c == close {
                depth += 1;
            } else if c == open {
                depth -= 1;
                if depth == 0 {
                    return Some(i as usize);
                }
            }
            i -= 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
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
}
