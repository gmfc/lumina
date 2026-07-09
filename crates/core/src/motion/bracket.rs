//! Matching-bracket resolution.

use crate::document::Document;

/// The char offset of the bracket that balances the one at `pos`, or `None` when `pos` is not
/// on a bracket or the bracket is unbalanced. Public so the app can precompute a bracket-match
/// highlight into render state (plan §1.3) without duplicating the scan.
///
/// Scans the rope directly from `pos` (forward for an opener, backward for a closer) rather than
/// materializing the whole document into a `Vec<char>` — this runs once per frame on the primary
/// caret, so an allocation proportional to file size would be a per-frame cost on large buffers.
pub fn matching_bracket(doc: &Document, pos: usize) -> Option<usize> {
    if pos >= doc.len_chars() {
        return None;
    }
    let (open, close, forward) = match doc.text.char(pos) {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _ => return None,
    };
    if forward {
        // The opening bracket sits at `pos`; walk forward until depth returns to zero.
        let mut depth = 0isize;
        let mut i = pos;
        for c in doc.text.chars_at(pos) {
            depth += bracket_delta(c, open, close);
            if depth == 0 {
                return Some(i);
            }
            i += 1;
        }
        None
    } else {
        // The closing bracket sits at `pos`; walk backward until depth returns to zero. The
        // reverse cursor starts just past `pos`, so its first `prev()` yields `chars[pos]`.
        let mut depth = 0isize;
        let mut i = pos;
        let mut chars = doc.text.chars_at(pos + 1);
        while let Some(c) = chars.prev() {
            depth -= bracket_delta(c, open, close);
            if depth == 0 {
                return Some(i);
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        None
    }
}

/// `+1` when `c` opens, `-1` when `c` closes, `0` otherwise.
fn bracket_delta(c: char, open: char, close: char) -> isize {
    if c == open {
        1
    } else if c == close {
        -1
    } else {
        0
    }
}
