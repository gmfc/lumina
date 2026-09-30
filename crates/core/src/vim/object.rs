//! Text objects: the `i`/`a` "smart nouns" — word, bracket pair, quoted string,
//! and paragraph. Each resolves, from any cursor position *inside* the object, to
//! the `[start, end)` char range it covers. Pure over a [`Document`].

use super::{class_of, is_blank_line};
use crate::document::Document;

/// A text object the cursor can sit inside. Resolved by [`text_object`] into a
/// concrete char range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextObject {
    /// `iw`/`aw` (small) or `iW`/`aW` (`big == true`).
    Word { big: bool },
    /// A bracket pair: `( )`, `{ }`, `[ ]`, `< >`.
    Pair { open: char, close: char },
    /// A quoted string delimited by `quote` (`"`, `'`, or `` ` ``).
    Quote { quote: char },
    /// `ip`/`ap` — a run of non-blank (or blank) lines.
    Paragraph,
    /// `it`/`at` — an HTML-ish tag pair enclosing the cursor.
    Tag,
}

/// Resolve `obj` at `pos`. `around` selects the `a` variant (delimiters/trailing
/// whitespace included) vs the `i` (inner) variant. Returns `None` when the
/// object can't be found (e.g. the cursor isn't inside a matching bracket pair).
pub fn text_object(
    doc: &Document,
    pos: usize,
    obj: TextObject,
    around: bool,
) -> Option<(usize, usize)> {
    match obj {
        TextObject::Word { big } => Some(word_object(doc, pos, big, around)),
        TextObject::Pair { open, close } => pair_object(doc, pos, open, close, around),
        TextObject::Quote { quote } => quote_object(doc, pos, quote, around),
        TextObject::Paragraph => Some(paragraph_object(doc, pos, around)),
        TextObject::Tag => tag_object(doc, pos, around),
    }
}

/// The `[start, end)` of the same-class run containing `pos`.
fn run_at(doc: &Document, pos: usize, big: bool) -> (usize, usize) {
    let n = doc.len_chars();
    if n == 0 {
        return (0, 0);
    }
    let p = pos.min(n - 1);
    let cls = class_of(doc.text.char(p), big);
    let mut start = p;
    while start > 0 && class_of(doc.text.char(start - 1), big) == cls {
        start -= 1;
    }
    let mut end = p;
    while end < n && class_of(doc.text.char(end), big) == cls {
        end += 1;
    }
    (start, end)
}

/// `iw`/`aw`: the run at the cursor. `aw` also grabs the trailing blank run (or,
/// failing that, the leading one), matching Vim.
fn word_object(doc: &Document, pos: usize, big: bool, around: bool) -> (usize, usize) {
    let (start, end) = run_at(doc, pos, big);
    if !around {
        return (start, end);
    }
    let n = doc.len_chars();
    // Trailing whitespace on the same line takes priority.
    let mut e = end;
    while e < n && doc.text.char(e).is_whitespace() && doc.text.char(e) != '\n' {
        e += 1;
    }
    if e > end {
        return (start, e);
    }
    // Otherwise fold in leading whitespace.
    let mut s = start;
    while s > 0 && doc.text.char(s - 1).is_whitespace() && doc.text.char(s - 1) != '\n' {
        s -= 1;
    }
    (s, end)
}

/// Find the bracket pair enclosing (or under) `pos`. Returns `(open_idx, close_idx)`.
fn enclosing_pair(doc: &Document, pos: usize, open: char, close: char) -> Option<(usize, usize)> {
    let n = doc.len_chars();
    if n == 0 {
        return None;
    }
    let open_idx = scan_open_left(doc, pos.min(n - 1), pos, open, close)?;
    let close_idx = scan_close_right(doc, open_idx, n, open, close)?;
    Some((open_idx, close_idx))
}

/// Scan left from `from` for the enclosing opener. A closer to our left (that isn't
/// the cursor char at `pos`) opens a nested pair we must skip over.
fn scan_open_left(
    doc: &Document,
    from: usize,
    pos: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from as isize;
    while i >= 0 {
        let c = doc.text.char(i as usize);
        if c == open && (open != close || depth == 0) {
            if depth == 0 {
                return Some(i as usize);
            }
            depth -= 1;
        } else if c == close && i as usize != pos {
            depth += 1;
        }
        i -= 1;
    }
    None
}

/// Scan right from just past `open_idx` for its matching closer.
fn scan_close_right(
    doc: &Document,
    open_idx: usize,
    n: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0i32;
    for j in (open_idx + 1)..n {
        let c = doc.text.char(j);
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return Some(j);
            }
            depth -= 1;
        }
    }
    None
}

/// `i(`/`a(` and friends. Inner is between the brackets; around includes them.
fn pair_object(
    doc: &Document,
    pos: usize,
    open: char,
    close: char,
    around: bool,
) -> Option<(usize, usize)> {
    let (o, c) = enclosing_pair(doc, pos, open, close)?;
    if around {
        Some((o, c + 1))
    } else {
        Some((o + 1, c))
    }
}

/// `i"`/`a"` etc., resolved within the cursor's line. Pairs quotes left-to-right
/// and picks the pair containing (or the next pair after) the cursor. `a"` also
/// swallows trailing whitespace.
fn quote_object(doc: &Document, pos: usize, quote: char, around: bool) -> Option<(usize, usize)> {
    let line = doc.char_to_line(pos);
    let line_start = doc.line_to_char(line);
    let body: Vec<char> = {
        let t = doc.line_text(line);
        t.trim_end_matches(['\n', '\r']).chars().collect()
    };
    let quotes: Vec<usize> = body
        .iter()
        .enumerate()
        .filter(|(_, &c)| c == quote)
        .map(|(i, _)| i)
        .collect();
    let col = pos - line_start;
    let mut k = 0;
    while k + 1 < quotes.len() {
        let (a, b) = (quotes[k], quotes[k + 1]);
        if col <= b {
            let (s, mut e) = if around { (a, b + 1) } else { (a + 1, b) };
            if around {
                // Swallow trailing blanks (Vim's `a"` behaviour).
                while e < body.len() && body[e].is_whitespace() {
                    e += 1;
                }
            }
            return Some((
                line_start + s.min(body.len()),
                line_start + e.min(body.len()),
            ));
        }
        k += 2;
    }
    None
}

/// `ip`/`ap`: the block of consecutive same-blankness lines around the cursor.
/// `ap` extends over the following blank lines (or, failing that, the preceding).
/// Returns a whole-line char range.
fn paragraph_object(doc: &Document, pos: usize, around: bool) -> (usize, usize) {
    let n_lines = doc.len_lines();
    let line = doc.char_to_line(pos);
    let blank = is_blank_line(doc, line);
    let (mut first, mut last) = same_blankness_run(doc, line, blank, n_lines);
    if around && !blank {
        (first, last) = extend_over_blanks(doc, first, last, n_lines);
    }
    let start = doc.line_to_char(first);
    let end = if last + 1 < n_lines {
        doc.line_to_char(last + 1)
    } else {
        doc.len_chars()
    };
    (start, end)
}

/// The `[first, last]` run of lines around `line` that share its blank-ness.
fn same_blankness_run(doc: &Document, line: usize, blank: bool, n_lines: usize) -> (usize, usize) {
    let mut first = line;
    while first > 0 && is_blank_line(doc, first - 1) == blank {
        first -= 1;
    }
    let mut last = line;
    while last + 1 < n_lines && is_blank_line(doc, last + 1) == blank {
        last += 1;
    }
    (first, last)
}

/// Grow a non-blank paragraph over the following blank lines (Vim's `ap`), or, when
/// there are none, over the preceding blank lines.
fn extend_over_blanks(doc: &Document, first: usize, last: usize, n_lines: usize) -> (usize, usize) {
    let mut last = last;
    let before = last;
    while last + 1 < n_lines && is_blank_line(doc, last + 1) {
        last += 1;
    }
    if last != before {
        return (first, last);
    }
    let mut first = first;
    while first > 0 && is_blank_line(doc, first - 1) {
        first -= 1;
    }
    (first, last)
}

/// Parse a simple HTML/XML open-tag name starting at `open_lt` (the `<` char). Returns
/// `(name, end_of_open_tag)` where `end_of_open_tag` is the index just past `>`.
fn parse_open_tag(doc: &Document, open_lt: usize) -> Option<(String, usize)> {
    let n = doc.len_chars();
    if open_lt >= n || doc.text.char(open_lt) != '<' {
        return None;
    }
    let mut i = open_lt + 1;
    if i < n && doc.text.char(i) == '/' {
        return None; // closing tag
    }
    // Skip `!` / `?` (comments / processing).
    if i < n && matches!(doc.text.char(i), '!' | '?') {
        return None;
    }
    let name_start = i;
    while i < n {
        let c = doc.text.char(i);
        if c.is_alphanumeric() || c == '-' || c == ':' || c == '_' {
            i += 1;
        } else {
            break;
        }
    }
    if i == name_start {
        return None;
    }
    let name: String = (name_start..i).map(|k| doc.text.char(k)).collect();
    // Find closing `>` (self-closing allowed).
    while i < n && doc.text.char(i) != '>' {
        i += 1;
    }
    if i >= n {
        return None;
    }
    // Self-closing `<br/>` has no inner content — treat as no tag object.
    let self_closing = i > open_lt && doc.text.char(i - 1) == '/';
    if self_closing {
        return None;
    }
    Some((name.to_ascii_lowercase(), i + 1))
}

/// Find the matching close tag `</name>` starting from `from`, with nesting awareness.
fn find_close_tag(doc: &Document, from: usize, name: &str) -> Option<usize> {
    let n = doc.len_chars();
    let close = format!("</{name}>");
    let open_pat = format!("<{name}");
    let mut depth = 1i32;
    let mut i = from;
    while i < n {
        // Cheap char-scan; tag names are ASCII-ish.
        if doc.text.char(i) != '<' {
            i += 1;
            continue;
        }
        let rest: String = (i..n.min(i + close.len().max(open_pat.len()) + 8))
            .map(|k| doc.text.char(k))
            .collect();
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with(&close) {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
            i += close.len();
            continue;
        }
        if lower.starts_with(&open_pat) {
            // Only count a real open tag (`<name…>` / `<name `), not a prefix of another name.
            let after = open_pat.len();
            let next = rest.chars().nth(after);
            if matches!(next, Some('>' | ' ' | '\t' | '\n' | '/' | '\r')) {
                // Self-closing opens don't deepen.
                if let Some(gt) = rest.find('>') {
                    if rest.as_bytes().get(gt.saturating_sub(1)) == Some(&b'/') {
                        i += gt + 1;
                        continue;
                    }
                }
                depth += 1;
            }
        }
        i += 1;
    }
    None
}

/// `it`/`at`: innermost HTML-ish tag enclosing `pos`. `around` includes the tags themselves.
fn tag_object(doc: &Document, pos: usize, around: bool) -> Option<(usize, usize)> {
    let n = doc.len_chars();
    if n == 0 {
        return None;
    }
    let p = pos.min(n - 1);
    // Scan left for open tags; keep the innermost whose close is past `pos`.
    let mut i = p as isize;
    while i >= 0 {
        if doc.text.char(i as usize) == '<' {
            if let Some((name, open_end)) = parse_open_tag(doc, i as usize) {
                if let Some(close_start) = find_close_tag(doc, open_end, &name) {
                    let close_end = close_start + 3 + name.len(); // </name>
                    if open_end <= p && p < close_end {
                        return if around {
                            Some((i as usize, close_end.min(n)))
                        } else {
                            Some((open_end, close_start))
                        };
                    }
                }
            }
        }
        i -= 1;
    }
    None
}
