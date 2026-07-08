//! High-level editing operations that apply to *every* selection at once.
//!
//! This is the one place multi-cursor edits are built: each selection contributes a
//! change, the changes are applied as a single [`Transaction`] (bottom-up, so offsets
//! stay valid — plan "hard part" #3), history is recorded, and the selection set is
//! re-derived from the edited positions.

use std::collections::BTreeSet;
use std::ops::Range;

use crate::document::Document;
use crate::history::GroupBreak;
use crate::motion::{self, Motion};
use crate::selection::{Selection, Selections};
use crate::transaction::{Change, Transaction};

/// Apply a per-selection edit. `f` maps each selection to `(range_to_replace, replacement)`.
/// Ranges must be non-overlapping across selections (the set is normalized, so they are).
pub fn edit_selections<F>(doc: &mut Document, mut f: F, group: GroupBreak)
where
    F: FnMut(&Document, Selection) -> (Range<usize>, String),
{
    let before = doc.selections.clone();

    let mut ops: Vec<(Range<usize>, String)> =
        doc.selections.ranges().iter().map(|s| f(doc, *s)).collect();
    ops.sort_by_key(|(r, _)| r.start);

    let changes: Vec<Change> = ops
        .iter()
        .map(|(r, text)| {
            let start = r.start.min(doc.len_chars());
            let end = r.end.min(doc.len_chars());
            let removed = if start < end {
                doc.text.slice(start..end).to_string()
            } else {
                String::new()
            };
            Change {
                at: start,
                removed,
                inserted: text.clone(),
            }
        })
        .collect();

    let txn = Transaction::from_changes(changes);
    if txn.is_empty() {
        return;
    }
    let inverse = txn.apply(doc);

    // New caret after each op: op start shifted by cumulative delta, plus inserted len.
    let mut delta: isize = 0;
    let mut new_sels: Vec<Selection> = Vec::with_capacity(ops.len());
    for (r, text) in &ops {
        let start = (r.start as isize + delta) as usize;
        let ins = text.chars().count();
        new_sels.push(Selection::caret(start + ins));
        delta += ins as isize - (r.end - r.start) as isize;
    }
    let after = Selections::from_iter(new_sels);
    doc.selections = after.clone();
    doc.view.goal_col = None;
    doc.dirty = true;
    doc.history.record(txn, inverse, before, after, group);
}

/// Insert `text` at every caret (replacing any selected span).
pub fn insert_text(doc: &mut Document, text: &str, group: GroupBreak) {
    edit_selections(doc, |_d, sel| (sel.span(), text.to_string()), group);
}

/// Insert a single typed char (coalesces into the current undo group).
pub fn insert_char(doc: &mut Document, ch: char) {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    insert_text(doc, s, GroupBreak::None);
}

/// Insert a newline at every caret, breaking the undo group.
pub fn insert_newline(doc: &mut Document) {
    insert_text(doc, "\n", GroupBreak::Force);
}

/// Delete the char (grapheme) before each caret, or the selection if non-empty.
pub fn delete_backward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let from = motion::resolve(d, sel.head, Motion::Left, 1);
                (from..sel.head, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

/// Delete the char after each caret, or the selection if non-empty.
pub fn delete_forward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let to = motion::resolve(d, sel.head, Motion::Right, 1);
                (sel.head..to, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

/// Move (or extend) every selection by `motion`. `page` is the viewport height.
pub fn move_selections(doc: &mut Document, motion: Motion, page: usize, extend: bool) {
    // Track the sticky goal column for vertical motions on the primary selection.
    let is_vertical = matches!(
        motion,
        Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
    );
    if !is_vertical {
        doc.view.goal_col = None;
    }

    let mut sels: Vec<Selection> = Vec::with_capacity(doc.selections.len());
    for sel in doc.selections.ranges() {
        let new_head = motion::resolve(doc, sel.head, motion, page);
        let anchor = if extend { sel.anchor } else { new_head };
        sels.push(Selection::new(anchor, new_head));
    }
    let primary = doc.selections.primary_index();
    let mut set = Selections::from_iter(sels);
    set.set_primary(primary);
    doc.selections = set;
}

/// Undo one revision; installs the restored selection set.
pub fn undo(doc: &mut Document) -> bool {
    let mut hist = std::mem::take(&mut doc.history);
    let restored = hist.undo(doc);
    doc.history = hist;
    if let Some(sel) = restored {
        doc.selections = sel;
        doc.dirty = true;
        true
    } else {
        false
    }
}

/// Redo one revision.
pub fn redo(doc: &mut Document) -> bool {
    let mut hist = std::mem::take(&mut doc.history);
    let restored = hist.redo(doc);
    doc.history = hist;
    if let Some(sel) = restored {
        doc.selections = sel;
        doc.dirty = true;
        true
    } else {
        false
    }
}

// --- word / line operations (VS Code parity, plan §5) --------------------------

/// Delete the word before each caret (Ctrl+Backspace), or the selection if non-empty.
pub fn delete_word_backward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let from = motion::resolve(d, sel.head, Motion::WordLeft, 1);
                (from..sel.head, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

/// Expand every selection to the word under its head (like a double-click).
pub fn select_word(doc: &mut Document) {
    let sels: Vec<Selection> = doc
        .selections
        .ranges()
        .iter()
        .map(|s| {
            let (a, b) = motion::word_at(doc, s.head);
            Selection::new(a, b)
        })
        .collect();
    let primary = doc.selections.primary_index();
    let mut set = Selections::from_iter(sels);
    set.set_primary(primary);
    doc.selections = set;
    doc.view.goal_col = None;
}

/// Expand every selection to the whole line(s) it touches (incl. trailing newline).
pub fn select_line(doc: &mut Document) {
    let sels: Vec<Selection> = doc
        .selections
        .ranges()
        .iter()
        .map(|s| {
            let first = doc.char_to_line(s.from());
            let last = span_end_line(doc, s);
            let start = doc.line_to_char(first);
            let end = if last + 1 < doc.len_lines() {
                doc.line_to_char(last + 1)
            } else {
                doc.len_chars()
            };
            Selection::new(start, end)
        })
        .collect();
    let primary = doc.selections.primary_index();
    let mut set = Selections::from_iter(sels);
    set.set_primary(primary);
    doc.selections = set;
    doc.view.goal_col = None;
}

/// Duplicate the line(s) covered by each selection, downward (Shift+Alt+Down).
pub fn duplicate_line(doc: &mut Document) {
    let before = doc.selections.clone();
    let changes: Vec<Change> = affected_lines(doc)
        .iter()
        .map(|&l| {
            let start = doc.line_to_char(l);
            if l + 1 < doc.len_lines() {
                let end = doc.line_to_char(l + 1); // includes this line's newline
                let body = doc.text.slice(start..end).to_string();
                Change {
                    at: end,
                    removed: String::new(),
                    inserted: body,
                }
            } else {
                // Last line has no trailing newline; prepend one to the copy.
                let end = doc.len_chars();
                let body = doc.text.slice(start..end).to_string();
                Change {
                    at: end,
                    removed: String::new(),
                    inserted: format!("\n{body}"),
                }
            }
        })
        .collect();
    apply_line_changes(doc, changes, before);
}

/// Toggle a `token` line comment (e.g. `//` or `#`) on the affected lines. Comments when
/// any affected non-blank line is uncommented; otherwise uncomments.
pub fn toggle_comment(doc: &mut Document, token: &str) {
    let before = doc.selections.clone();
    let lines = affected_lines(doc);
    let tok_len = token.chars().count();

    let mut all_commented = true;
    let mut any_nonblank = false;
    for &l in &lines {
        let text = doc.line_text(l);
        let body = text.trim_end_matches(['\n', '\r']);
        let stripped = body.trim_start_matches([' ', '\t']);
        if stripped.is_empty() {
            continue;
        }
        any_nonblank = true;
        if !stripped.starts_with(token) {
            all_commented = false;
        }
    }
    if !any_nonblank {
        return;
    }

    let mut changes = Vec::new();
    for &l in &lines {
        let line_start = doc.line_to_char(l);
        let text = doc.line_text(l);
        let body = text.trim_end_matches(['\n', '\r']);
        let indent = body.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let stripped = body.trim_start_matches([' ', '\t']);
        if stripped.is_empty() {
            continue;
        }
        let at = line_start + indent;
        if all_commented {
            // Remove the token plus one following space, if present.
            let after: String = stripped.chars().skip(tok_len).collect();
            let remove_len = tok_len + usize::from(after.starts_with(' '));
            changes.push(Change {
                at,
                removed: doc.text.slice(at..at + remove_len).to_string(),
                inserted: String::new(),
            });
        } else {
            changes.push(Change {
                at,
                removed: String::new(),
                inserted: format!("{token} "),
            });
        }
    }
    apply_line_changes(doc, changes, before);
}

/// Indent the affected lines (or insert spaces at each caret when nothing is selected).
pub fn indent(doc: &mut Document) {
    let has_selection = doc.selections.ranges().iter().any(|s| !s.is_empty());
    if !has_selection {
        insert_text(doc, "    ", GroupBreak::Force);
        return;
    }
    let before = doc.selections.clone();
    let changes: Vec<Change> = affected_lines(doc)
        .iter()
        .map(|&l| Change {
            at: doc.line_to_char(l),
            removed: String::new(),
            inserted: "    ".into(),
        })
        .collect();
    apply_line_changes(doc, changes, before);
}

/// Outdent the affected lines: strip one leading tab or up to `tab_width` spaces.
pub fn outdent(doc: &mut Document) {
    let before = doc.selections.clone();
    let width = doc.tab_width.max(1);
    let mut changes = Vec::new();
    for l in affected_lines(doc) {
        let start = doc.line_to_char(l);
        let text = doc.line_text(l);
        let chars: Vec<char> = text.chars().collect();
        let remove = if chars.first() == Some(&'\t') {
            1
        } else {
            let mut n = 0;
            while n < width && chars.get(n) == Some(&' ') {
                n += 1;
            }
            n
        };
        if remove > 0 {
            changes.push(Change {
                at: start,
                removed: doc.text.slice(start..start + remove).to_string(),
                inserted: String::new(),
            });
        }
    }
    if changes.is_empty() {
        return;
    }
    apply_line_changes(doc, changes, before);
}

/// Move the block of lines covered by the selection(s) up (`delta<0`) or down (`delta>0`),
/// keeping the cursors on the moved lines (Alt+Up / Alt+Down).
pub fn move_lines(doc: &mut Document, delta: isize) {
    if delta == 0 {
        return;
    }
    let lines = affected_lines(doc);
    let (Some(&first), Some(&last)) = (lines.first(), lines.last()) else {
        return;
    };
    let last_content = last_content_line(doc);
    let last = last.min(last_content);

    let (region_start, region_end, new_region) = if delta < 0 {
        if first == 0 {
            return;
        }
        let rs = doc.line_to_char(first - 1);
        let re = region_end_char(doc, last);
        let (mut ls, fin) = split_region(&doc.text.slice(rs..re).to_string());
        if ls.len() < 2 {
            return;
        }
        let prev = ls.remove(0); // move the preceding line below the block
        ls.push(prev);
        (rs, re, join_region(&ls, fin))
    } else {
        if last >= last_content {
            return;
        }
        let rs = doc.line_to_char(first);
        let re = region_end_char(doc, last + 1);
        let (mut ls, fin) = split_region(&doc.text.slice(rs..re).to_string());
        if ls.len() < 2 {
            return;
        }
        let next = ls.pop().unwrap(); // move the following line above the block
        ls.insert(0, next);
        (rs, re, join_region(&ls, fin))
    };

    let original = doc.text.slice(region_start..region_end).to_string();
    if new_region == original {
        return;
    }

    let before = doc.selections.clone();
    // Remember each endpoint as (line, col) so cursors ride the moved lines.
    let cols: Vec<(usize, usize, usize, usize)> = before
        .ranges()
        .iter()
        .map(|s| {
            let (al, ac) = doc.char_to_line_col(s.anchor);
            let (hl, hc) = doc.char_to_line_col(s.head);
            (al, ac, hl, hc)
        })
        .collect();

    let txn = Transaction::from_changes(vec![Change {
        at: region_start,
        removed: original,
        inserted: new_region,
    }]);
    let inverse = txn.apply(doc);

    let shift = |line: usize| (line as isize + delta).max(0) as usize;
    let new_sels: Vec<Selection> = cols
        .iter()
        .map(|&(al, ac, hl, hc)| {
            Selection::new(
                line_col_to_char(doc, shift(al), ac),
                line_col_to_char(doc, shift(hl), hc),
            )
        })
        .collect();
    let primary = before.primary_index();
    let mut after = Selections::from_iter(new_sels);
    after.set_primary(primary);
    doc.selections = after.clone();
    doc.dirty = true;
    doc.view.goal_col = None;
    doc.history
        .record(txn, inverse, before, after, GroupBreak::Force);
}

// --- shared helpers for line-oriented edits ------------------------------------

/// Last line index whose selection end sits on it (a non-empty selection ending exactly at
/// a line start does not pull in that next line).
fn span_end_line(doc: &Document, s: &Selection) -> usize {
    if s.to() > s.from() {
        doc.char_to_line(s.to().saturating_sub(1))
    } else {
        doc.char_to_line(s.to())
    }
}

/// Distinct, sorted set of lines any selection touches.
fn affected_lines(doc: &Document) -> Vec<usize> {
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
fn last_content_line(doc: &Document) -> usize {
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
fn region_end_char(doc: &Document, l: usize) -> usize {
    if l + 1 < doc.len_lines() {
        doc.line_to_char(l + 1)
    } else {
        doc.len_chars()
    }
}

fn line_col_to_char(doc: &Document, line: usize, col: usize) -> usize {
    let line = line.min(doc.len_lines().saturating_sub(1));
    doc.line_to_char(line) + col.min(doc.line_len_chars(line))
}

/// Split a region into its lines, remembering whether it ended in a newline, so the join is
/// exactly reversible (no lost or gained line breaks when reordering).
fn split_region(region: &str) -> (Vec<String>, bool) {
    let fin = region.ends_with('\n');
    let mut v: Vec<String> = region.split('\n').map(|s| s.to_string()).collect();
    if fin {
        v.pop(); // drop the trailing "" that `split` yields after the final '\n'
    }
    (v, fin)
}

fn join_region(lines: &[String], fin: bool) -> String {
    let mut s = lines.join("\n");
    if fin {
        s.push('\n');
    }
    s
}

/// Apply a set of per-line changes as one transaction, mapping selections through it.
fn apply_line_changes(doc: &mut Document, changes: Vec<Change>, before: Selections) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn multi_caret(doc: &mut Document, positions: &[usize]) {
        let sels: Vec<Selection> = positions.iter().map(|&p| Selection::caret(p)).collect();
        doc.selections = Selections::from_iter(sels);
    }

    #[test]
    fn insert_at_single_caret() {
        let mut doc = Document::from_str("hello");
        doc.set_caret(5);
        insert_text(&mut doc, "!", GroupBreak::Force);
        assert_eq!(doc.to_string(), "hello!");
    }

    #[test]
    fn multi_cursor_insert_keeps_offsets_valid() {
        let mut doc = Document::from_str("a\nb\nc");
        // carets at start of each line: offsets 0, 2, 4
        multi_caret(&mut doc, &[0, 2, 4]);
        insert_text(&mut doc, "> ", GroupBreak::Force);
        assert_eq!(doc.to_string(), "> a\n> b\n> c");
        // three carets, each after its inserted "> "
        assert_eq!(doc.selections.len(), 3);
    }

    #[test]
    fn backspace_then_undo() {
        let mut doc = Document::from_str("hello");
        doc.set_caret(5);
        delete_backward(&mut doc);
        assert_eq!(doc.to_string(), "hell");
        undo(&mut doc);
        assert_eq!(doc.to_string(), "hello");
        assert_eq!(doc.selections.primary().head, 5);
    }

    #[test]
    fn typing_burst_undoes_together() {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        insert_char(&mut doc, 'h');
        insert_char(&mut doc, 'i');
        assert_eq!(doc.to_string(), "hi");
        undo(&mut doc);
        assert_eq!(doc.to_string(), "");
    }

    #[test]
    fn delete_word_backward_removes_word() {
        let mut doc = Document::from_str("hello world");
        doc.set_caret(11);
        delete_word_backward(&mut doc);
        assert_eq!(doc.to_string(), "hello ");
        assert_eq!(doc.selections.primary().head, 6);
    }

    #[test]
    fn select_word_and_line() {
        let mut doc = Document::from_str("foo bar\nbaz");
        doc.set_caret(5); // inside "bar"
        select_word(&mut doc);
        let s = doc.selections.primary();
        assert_eq!((s.from(), s.to()), (4, 7));
        doc.set_caret(1);
        select_line(&mut doc);
        let s = doc.selections.primary();
        assert_eq!((s.from(), s.to()), (0, 8)); // "foo bar\n"
    }

    #[test]
    fn duplicate_middle_and_last_line() {
        let mut doc = Document::from_str("a\nb\nc");
        doc.set_caret(doc.line_to_char(1)); // line "b"
        duplicate_line(&mut doc);
        assert_eq!(doc.to_string(), "a\nb\nb\nc");
        undo(&mut doc);
        assert_eq!(doc.to_string(), "a\nb\nc");
        // Last line (no trailing newline) duplicates with a fresh break.
        doc.set_caret(doc.len_chars());
        duplicate_line(&mut doc);
        assert_eq!(doc.to_string(), "a\nb\nc\nc");
    }

    #[test]
    fn toggle_comment_round_trips() {
        let mut doc = Document::from_str("  let x = 1;\n  let y = 2;");
        doc.selections = Selections::single(Selection::new(0, doc.len_chars()));
        toggle_comment(&mut doc, "//");
        assert_eq!(doc.to_string(), "  // let x = 1;\n  // let y = 2;");
        toggle_comment(&mut doc, "//");
        assert_eq!(doc.to_string(), "  let x = 1;\n  let y = 2;");
    }

    #[test]
    fn indent_and_outdent_lines() {
        let mut doc = Document::from_str("a\nb");
        doc.selections = Selections::single(Selection::new(0, doc.len_chars()));
        indent(&mut doc);
        assert_eq!(doc.to_string(), "    a\n    b");
        outdent(&mut doc);
        assert_eq!(doc.to_string(), "a\nb");
    }

    #[test]
    fn move_line_up_and_down() {
        let mut doc = Document::from_str("one\ntwo\nthree");
        doc.set_caret(doc.line_to_char(2)); // "three"
        move_lines(&mut doc, -1);
        assert_eq!(doc.to_string(), "one\nthree\ntwo");
        // Cursor rode the moved line up to line 1.
        assert_eq!(doc.char_to_line(doc.selections.primary().head), 1);
        move_lines(&mut doc, 1);
        assert_eq!(doc.to_string(), "one\ntwo\nthree");
    }

    #[test]
    fn move_line_up_at_top_is_noop() {
        let mut doc = Document::from_str("one\ntwo");
        doc.set_caret(0);
        move_lines(&mut doc, -1);
        assert_eq!(doc.to_string(), "one\ntwo");
    }

    #[test]
    fn move_line_down_with_trailing_newline() {
        let mut doc = Document::from_str("one\ntwo\n");
        doc.set_caret(0); // "one"
        move_lines(&mut doc, 1);
        assert_eq!(doc.to_string(), "two\none\n");
    }
}
