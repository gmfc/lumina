//! Pure primitives for modal (Vim) editing: extended word/WORD motions, the
//! `f`/`t`/`F`/`T` character search, paragraph motions, and the `i`/`a` text
//! objects.
//!
//! These are the "hard" offset-and-range computations a Vim layer needs but the
//! VS Code-style [`crate::motion`] module doesn't already provide. They live here
//! — in the headless core — rather than in `editor-app`, so they are pure
//! functions of a [`Document`] and unit-testable without a terminal (CLAUDE.md
//! invariants #5, #7). The modal *state machine* (modes, operators, registers,
//! key routing) lives in `editor-app`; this module only answers "given a cursor,
//! where does this motion land?" and "what range does this text object cover?".
//!
//! Offsets are **char offsets** into the rope, matching [`crate::selection`].
//! Motions return the offset the block cursor lands on; the caller decides
//! inclusive/exclusive operator ranges (see the `editor-app` `vim` module).

use crate::document::Document;

mod object;
#[cfg(test)]
mod tests;

pub use object::{text_object, TextObject};

/// A Vim character class. `word` motions treat these three as distinct runs;
/// `WORD` motions (the `big` variants) collapse [`Class::Word`] and
/// [`Class::Punct`] into a single non-blank run.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Class {
    Blank,
    Word,
    Punct,
}

/// Classify `ch`. With `big`, only blank vs non-blank matters (WORD motions).
fn class_of(ch: char, big: bool) -> Class {
    if ch.is_whitespace() {
        Class::Blank
    } else if big || ch.is_alphanumeric() || ch == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Forward to the **start of the next word** (`w`/`W`). Exclusive; can land on
/// end-of-buffer. Skips the current same-class run, then any blanks.
pub fn next_word_start(doc: &Document, pos: usize, big: bool) -> usize {
    let n = doc.len_chars();
    if pos >= n {
        return n;
    }
    let mut i = pos;
    let start = class_of(doc.text.char(i), big);
    if start != Class::Blank {
        while i < n && class_of(doc.text.char(i), big) == start {
            i += 1;
        }
    }
    while i < n && class_of(doc.text.char(i), big) == Class::Blank {
        i += 1;
    }
    i
}

/// Forward to the **end of the next word** (`e`/`E`). Inclusive: lands *on* the
/// word's last char. Always advances at least one char first, then skips blanks.
pub fn next_word_end(doc: &Document, pos: usize, big: bool) -> usize {
    let n = doc.len_chars();
    if n == 0 {
        return 0;
    }
    let mut i = pos + 1;
    while i < n && class_of(doc.text.char(i), big) == Class::Blank {
        i += 1;
    }
    if i >= n {
        return n - 1;
    }
    let cls = class_of(doc.text.char(i), big);
    while i + 1 < n && class_of(doc.text.char(i + 1), big) == cls {
        i += 1;
    }
    i
}

/// Back to the **start of the current/previous word** (`b`/`B`). Exclusive.
pub fn prev_word_start(doc: &Document, pos: usize, big: bool) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while i > 0 && class_of(doc.text.char(i), big) == Class::Blank {
        i -= 1;
    }
    if class_of(doc.text.char(i), big) == Class::Blank {
        return i;
    }
    let cls = class_of(doc.text.char(i), big);
    while i > 0 && class_of(doc.text.char(i - 1), big) == cls {
        i -= 1;
    }
    i
}

/// Back to the **end of the previous word** (`ge`/`gE`). Inclusive.
pub fn prev_word_end(doc: &Document, pos: usize, big: bool) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    // If we're inside/at the tail of a run, skip past its start so we land on the
    // *previous* word's end rather than the current word's.
    let here = class_of(doc.text.char(i), big);
    if here != Class::Blank {
        while i > 0 && class_of(doc.text.char(i - 1), big) == here {
            i -= 1;
        }
        i = i.saturating_sub(1);
    }
    // Skip the blank gap to land on the previous word's last char.
    while i > 0 && class_of(doc.text.char(i), big) == Class::Blank {
        i -= 1;
    }
    i
}

/// Which side of the `f`/`t` family a character search runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FindKind {
    /// `f` — forward, land **on** the target (inclusive).
    Find,
    /// `t` — forward, land **before** the target (inclusive).
    Till,
    /// `F` — backward, land **on** the target (exclusive of the cursor char).
    FindBack,
    /// `T` — backward, land **after** the target.
    TillBack,
}

/// Search for `target` on the current line only (Vim's `f`/`t`/`F`/`T`), starting
/// from `pos`. Returns the landing offset, or `None` when the char isn't found on
/// the line in that direction.
pub fn find_char(doc: &Document, pos: usize, target: char, kind: FindKind) -> Option<usize> {
    let line = doc.char_to_line(pos);
    let line_start = doc.line_to_char(line);
    let body: Vec<char> = {
        let t = doc.line_text(line);
        t.trim_end_matches(['\n', '\r']).chars().collect()
    };
    let col = pos - line_start;
    // The index (within `body`) of the nearest `target` in the search direction.
    let found = match kind {
        FindKind::Find | FindKind::Till => body
            .iter()
            .enumerate()
            .skip(col + 1)
            .find(|(_, &c)| c == target)
            .map(|(j, _)| j),
        FindKind::FindBack | FindKind::TillBack => body
            .iter()
            .enumerate()
            .take(col)
            .rev()
            .find(|(_, &c)| c == target)
            .map(|(j, _)| j),
    }?;
    // Adjust for `t`/`T`, which stop one short of the target.
    let landing = match kind {
        FindKind::Find | FindKind::FindBack => found,
        FindKind::Till => found.saturating_sub(1),
        FindKind::TillBack => found + 1,
    };
    Some(line_start + landing)
}

/// True when line `line` is blank (empty or whitespace-only) — a paragraph boundary.
fn is_blank_line(doc: &Document, line: usize) -> bool {
    if line >= doc.len_lines() {
        return true;
    }
    doc.line_text(line)
        .trim_end_matches(['\n', '\r'])
        .chars()
        .all(|c| c.is_whitespace())
}

/// Forward to the next paragraph boundary (`}`): the start of the next blank line,
/// or end-of-buffer. Exclusive, char-wise.
pub fn paragraph_forward(doc: &Document, pos: usize) -> usize {
    let n_lines = doc.len_lines();
    let cur = doc.char_to_line(pos);
    let mut l = cur + 1;
    // Skip an immediate blank run so repeated `}` advances past each paragraph.
    while l < n_lines && is_blank_line(doc, l) && is_blank_line(doc, cur) {
        l += 1;
    }
    while l < n_lines && !is_blank_line(doc, l) {
        l += 1;
    }
    if l >= n_lines {
        doc.len_chars()
    } else {
        doc.line_to_char(l)
    }
}

/// Back to the previous paragraph boundary (`{`). Exclusive, char-wise.
pub fn paragraph_backward(doc: &Document, pos: usize) -> usize {
    let cur = doc.char_to_line(pos);
    if cur == 0 {
        return 0;
    }
    let mut l = cur - 1;
    while l > 0 && is_blank_line(doc, l) && is_blank_line(doc, cur) {
        l -= 1;
    }
    while l > 0 && !is_blank_line(doc, l) {
        l -= 1;
    }
    doc.line_to_char(l)
}

/// First non-blank char offset of `line` (`^` and linewise operator landing).
pub fn first_non_blank(doc: &Document, line: usize) -> usize {
    let start = doc.line_to_char(line);
    let text = doc.line_text(line);
    for (i, ch) in text.chars().enumerate() {
        if ch == '\n' || ch == '\r' {
            break;
        }
        if !ch.is_whitespace() {
            return start + i;
        }
    }
    start
}

/// Last non-blank char offset on the line containing `pos` (`g_`). Inclusive.
pub fn last_non_blank(doc: &Document, pos: usize) -> usize {
    let line = doc.char_to_line(pos);
    let start = doc.line_to_char(line);
    let body: Vec<char> = {
        let t = doc.line_text(line);
        t.trim_end_matches(['\n', '\r']).chars().collect()
    };
    let mut i = body.len();
    while i > 0 && body[i - 1].is_whitespace() {
        i -= 1;
    }
    if i == 0 {
        start
    } else {
        start + i - 1
    }
}
