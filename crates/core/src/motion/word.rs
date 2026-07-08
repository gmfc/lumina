//! Word motions and word/whitespace/punct-run selection.

use crate::document::Document;

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

pub(super) fn word_left(doc: &Document, pos: usize) -> usize {
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

pub(super) fn word_right(doc: &Document, pos: usize) -> usize {
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

pub(super) fn word_end_right(doc: &Document, pos: usize) -> usize {
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
