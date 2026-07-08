//! Shared helpers for pair/indent-aware and line-oriented edits.

use std::collections::BTreeSet;

use crate::document::Document;
use crate::history::GroupBreak;
use crate::selection::{Selection, Selections};
use crate::transaction::{Change, Transaction};

/// Char immediately before `pos`, or `None` at the buffer start.
pub(super) fn char_before(doc: &Document, pos: usize) -> Option<char> {
    (pos > 0).then(|| doc.text.char(pos - 1))
}

/// Char at `pos`, or `None` at the buffer end.
pub(super) fn char_at(doc: &Document, pos: usize) -> Option<char> {
    (pos < doc.len_chars()).then(|| doc.text.char(pos))
}

/// Leading run of spaces/tabs in `line`.
pub(super) fn leading_ws(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// One indentation level, matching the existing indent style: a tab if `base` uses tabs,
/// otherwise `tab_width` spaces.
pub(super) fn indent_unit(base: &str, tab_width: usize) -> String {
    if base.contains('\t') {
        "\t".to_string()
    } else {
        " ".repeat(tab_width.max(1))
    }
}

/// Strip one indentation level from the *end* of a leading-whitespace run (a tab, else up to
/// `tab_width` spaces).
pub(super) fn dedent_one(ws: &str, tab_width: usize) -> String {
    let mut s = ws.to_string();
    if s.ends_with('\t') {
        s.pop();
    } else {
        for _ in 0..tab_width.max(1) {
            if s.ends_with(' ') {
                s.pop();
            } else {
                break;
            }
        }
    }
    s
}

/// Last line index whose selection end sits on it (a non-empty selection ending exactly at
/// a line start does not pull in that next line).
pub(super) fn span_end_line(doc: &Document, s: &Selection) -> usize {
    if s.to() > s.from() {
        doc.char_to_line(s.to().saturating_sub(1))
    } else {
        doc.char_to_line(s.to())
    }
}

/// Distinct, sorted set of lines any selection touches.
pub(super) fn affected_lines(doc: &Document) -> Vec<usize> {
    let mut set = BTreeSet::new();
    for s in doc.selections.ranges() {
        let first = doc.char_to_line(s.from());
        let last = span_end_line(doc, s);
        for l in first..=last {
            set.insert(l);
        }
    }
    set.into_iter().collect()
}

/// Index of the last line that carries content (ropey reports a trailing empty line when the
/// buffer ends in `\n`; that phantom line is never "movable").
pub(super) fn last_content_line(doc: &Document) -> usize {
    let lines = doc.len_lines();
    if lines == 0 {
        return 0;
    }
    let ends_with_nl = doc.len_chars() > 0 && doc.text.char(doc.len_chars() - 1) == '\n';
    if ends_with_nl {
        lines.saturating_sub(2)
    } else {
        lines.saturating_sub(1)
    }
}

/// Char offset just past line `l` (its newline included), clamped to the buffer end.
pub(super) fn region_end_char(doc: &Document, l: usize) -> usize {
    if l + 1 < doc.len_lines() {
        doc.line_to_char(l + 1)
    } else {
        doc.len_chars()
    }
}

pub(super) fn line_col_to_char(doc: &Document, line: usize, col: usize) -> usize {
    let line = line.min(doc.len_lines().saturating_sub(1));
    doc.line_to_char(line) + col.min(doc.line_len_chars(line))
}

/// Split a region into its lines, remembering whether it ended in a newline, so the join is
/// exactly reversible (no lost or gained line breaks when reordering).
pub(super) fn split_region(region: &str) -> (Vec<String>, bool) {
    let fin = region.ends_with('\n');
    let mut v: Vec<String> = region.split('\n').map(|s| s.to_string()).collect();
    if fin {
        v.pop(); // drop the trailing "" that `split` yields after the final '\n'
    }
    (v, fin)
}

pub(super) fn join_region(lines: &[String], fin: bool) -> String {
    let mut s = lines.join("\n");
    if fin {
        s.push('\n');
    }
    s
}

/// Apply a set of per-line changes as one transaction, mapping selections through it.
pub(super) fn apply_line_changes(doc: &mut Document, changes: Vec<Change>, before: Selections) {
    let txn = Transaction::from_changes(changes);
    if txn.is_empty() {
        return;
    }
    let inverse = txn.apply(doc);
    let mapped: Vec<Selection> = before
        .ranges()
        .iter()
        .map(|s| Selection::new(txn.map_position(s.anchor), txn.map_position(s.head)))
        .collect();
    let primary = before.primary_index();
    let mut after = Selections::from_iter(mapped);
    after.set_primary(primary);
    doc.selections = after.clone();
    doc.dirty = true;
    doc.view.goal_col = None;
    doc.history
        .record(txn, inverse, before, after, GroupBreak::Force);
}
