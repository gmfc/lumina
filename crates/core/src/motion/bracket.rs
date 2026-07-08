//! Matching-bracket resolution.

use crate::document::Document;

/// The char offset of the bracket that balances the one at `pos`, or `None` when `pos` is not
/// on a bracket or the bracket is unbalanced. Public so the app can precompute a bracket-match
/// highlight into render state (plan §1.3) without duplicating the scan.
pub fn matching_bracket(doc: &Document, pos: usize) -> Option<usize> {
    let chars: Vec<char> = doc.text.chars().collect();
    if pos >= chars.len() {
        return None;
    }
    let (open, close, forward) = match chars[pos] {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _ => return None,
    };
    if forward {
        // The opening bracket sits at `pos`; scan the tail and offset the hit back.
        scan_bracket(&chars[pos..], open, close).map(|off| pos + off)
    } else {
        // The closing bracket sits at `pos`; scan the reversed head, indices realign.
        let n = pos + 1;
        scan_bracket_rev(&chars[..n], open, close)
    }
}

/// Index within `chars` of the bracket that balances `chars[0]`, scanning forward.
/// `chars[0]` is assumed to be an opening bracket; depth returns to zero at the match.
fn scan_bracket(chars: &[char], open: char, close: char) -> Option<usize> {
    let mut depth = 0isize;
    for (i, &c) in chars.iter().enumerate() {
        depth += bracket_delta(c, open, close);
        if depth == 0 {
            return Some(i);
        }
    }
    None
}

/// Index within `chars` of the bracket that balances the last element, scanning
/// backward. The last element is assumed to be a closing bracket.
fn scan_bracket_rev(chars: &[char], open: char, close: char) -> Option<usize> {
    let mut depth = 0isize;
    for i in (0..chars.len()).rev() {
        depth -= bracket_delta(chars[i], open, close);
        if depth == 0 {
            return Some(i);
        }
    }
    None
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
