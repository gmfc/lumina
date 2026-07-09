//! Word motions and word/whitespace/punct-run selection.
//!
//! These walk the rope with ropey's bidirectional char cursor (`chars_at`) rather than
//! materializing the whole document into a `Vec<char>` — a word step only ever inspects the run
//! around `pos`, so an allocation proportional to file size would be pure waste on large buffers
//! (these run once per selection per keystroke).

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
    // The cursor sits at `pos`; each `prev()` yields the char at the current `i - 1`.
    let mut cur = doc.text.chars_at(pos);
    let mut i = pos;
    // Skip whitespace to the left; the first non-whitespace char sets the run's class and is
    // itself consumed as the run's first member.
    let cls = loop {
        if i == 0 {
            return 0;
        }
        let c = cur.prev().unwrap();
        i -= 1;
        if class_of(c) != Class::Whitespace {
            break class_of(c);
        }
    };
    // Consume the rest of the same-class run.
    while i > 0 {
        if class_of(cur.prev().unwrap()) == cls {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

pub(super) fn word_right(doc: &Document, pos: usize) -> usize {
    let n = doc.len_chars();
    if pos >= n {
        return n;
    }
    let mut cur = doc.text.chars_at(pos);
    let mut i = pos;
    let cls = class_of(cur.next().unwrap()); // char at `pos`, now consumed
    if cls != Class::Whitespace {
        // Walk to the end of the starting run...
        i += 1;
        while i < n {
            let c = cur.next().unwrap();
            if class_of(c) == cls {
                i += 1;
                continue;
            }
            // ...then, if the run is followed by whitespace, skip that too (`c` is char `i`).
            if class_of(c) == Class::Whitespace {
                i += 1;
                skip_whitespace(&mut cur, &mut i, n);
            }
            return i;
        }
        n
    } else {
        // Starting on whitespace: skip the whitespace run only (char at `pos` already consumed).
        i += 1;
        skip_whitespace(&mut cur, &mut i, n);
        i
    }
}

pub(super) fn word_end_right(doc: &Document, pos: usize) -> usize {
    let n = doc.len_chars();
    if pos >= n {
        return n;
    }
    // Start one char ahead, skip whitespace, then ride the next same-class run to its end.
    let mut cur = doc.text.chars_at(pos + 1);
    let mut i = pos + 1;
    let cls = loop {
        if i >= n {
            return n;
        }
        let c = cur.next().unwrap();
        if class_of(c) != Class::Whitespace {
            break class_of(c);
        }
        i += 1;
    };
    i += 1; // consume the first non-whitespace char just read
    while i < n {
        if class_of(cur.next().unwrap()) == cls {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Advance `i` (and the cursor) over a run of whitespace, stopping at the first non-whitespace
/// char or end-of-buffer. The cursor is left having read one char past the run when it stops
/// early — callers that return immediately afterward don't observe that.
fn skip_whitespace(cur: &mut ropey::iter::Chars, i: &mut usize, n: usize) {
    while *i < n {
        if class_of(cur.next().unwrap()) == Class::Whitespace {
            *i += 1;
        } else {
            break;
        }
    }
}

/// The `[start, end)` char range of the word (or whitespace/punct run) containing `pos`.
/// Used by double-click word selection.
pub fn word_at(doc: &Document, pos: usize) -> (usize, usize) {
    let n = doc.len_chars();
    if n == 0 {
        return (0, 0);
    }
    let idx = pos.min(n - 1);
    let cls = class_of(doc.text.char(idx));
    // Extend left over the same-class run.
    let mut start = idx;
    let mut left = doc.text.chars_at(idx);
    while start > 0 {
        if class_of(left.prev().unwrap()) == cls {
            start -= 1;
        } else {
            break;
        }
    }
    // Extend right over the same-class run.
    let mut end = idx;
    let mut right = doc.text.chars_at(idx);
    while end < n {
        if class_of(right.next().unwrap()) == cls {
            end += 1;
        } else {
            break;
        }
    }
    (start, end)
}
