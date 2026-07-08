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
use crate::pairs::{self, InsertAction, PairTable};
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

/// Like [`edit_selections`] but each op also dictates the resulting selection, via
/// `(anchor_offset, head_offset)` measured in chars from the op's post-shift start. This is
/// what lets auto-pairs drop the caret *between* an inserted pair, step it *past* an existing
/// closer (a no-op edit that still moves the cursor), or keep the inner text selected when
/// surrounding a selection — all through one [`Transaction`] (plan §1.1 invariants).
fn edit_selections_sel<F>(doc: &mut Document, mut f: F, group: GroupBreak)
where
    F: FnMut(&Document, Selection) -> (Range<usize>, String, (usize, usize)),
{
    let before = doc.selections.clone();

    let mut ops: Vec<(Range<usize>, String, (usize, usize))> =
        doc.selections.ranges().iter().map(|s| f(doc, *s)).collect();
    ops.sort_by_key(|(r, _, _)| r.start);

    // Skip no-op ops (e.g. a "type over" that inserts and removes nothing) so a pure cursor
    // step never marks the buffer dirty or lands on the undo stack.
    let changes: Vec<Change> = ops
        .iter()
        .filter_map(|(r, text, _)| {
            let start = r.start.min(doc.len_chars());
            let end = r.end.min(doc.len_chars());
            let removed = if start < end {
                doc.text.slice(start..end).to_string()
            } else {
                String::new()
            };
            if removed.is_empty() && text.is_empty() {
                None
            } else {
                Some(Change {
                    at: start,
                    removed,
                    inserted: text.clone(),
                })
            }
        })
        .collect();

    let txn = Transaction::from_changes(changes);
    let inverse = if txn.is_empty() {
        Transaction::empty()
    } else {
        txn.apply(doc)
    };

    // Resulting selections: each op's start shifts by the cumulative delta of prior ops
    // (no-op ops contribute zero delta, so filtering them out above is safe here).
    let len = doc.len_chars();
    let mut delta: isize = 0;
    let mut new_sels: Vec<Selection> = Vec::with_capacity(ops.len());
    for (r, text, (a_off, h_off)) in &ops {
        let start = (r.start as isize + delta) as usize;
        new_sels.push(Selection::new(
            (start + a_off).min(len),
            (start + h_off).min(len),
        ));
        let ins = text.chars().count();
        delta += ins as isize - (r.end - r.start) as isize;
    }
    let after = Selections::from_iter(new_sels);
    doc.selections = after.clone();
    doc.view.goal_col = None;
    if !txn.is_empty() {
        doc.dirty = true;
        doc.history.record(txn, inverse, before, after, group);
    }
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

// --- auto-closing pairs & auto-indent (plan §1.1, §1.2) ------------------------

/// Char immediately before `pos`, or `None` at the buffer start.
fn char_before(doc: &Document, pos: usize) -> Option<char> {
    (pos > 0).then(|| doc.text.char(pos - 1))
}

/// Char at `pos`, or `None` at the buffer end.
fn char_at(doc: &Document, pos: usize) -> Option<char> {
    (pos < doc.len_chars()).then(|| doc.text.char(pos))
}

/// Leading run of spaces/tabs in `line`.
fn leading_ws(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// One indentation level, matching the existing indent style: a tab if `base` uses tabs,
/// otherwise `tab_width` spaces.
fn indent_unit(base: &str, tab_width: usize) -> String {
    if base.contains('\t') {
        "\t".to_string()
    } else {
        " ".repeat(tab_width.max(1))
    }
}

/// Strip one indentation level from the *end* of a leading-whitespace run (a tab, else up to
/// `tab_width` spaces).
fn dedent_one(ws: &str, tab_width: usize) -> String {
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

/// Insert a typed char with optional auto-closing pairs and closing-bracket dedent
/// (plan §1.1, §1.2). Degrades to a plain per-caret insert when both flags are off, so this
/// can back every `InsertChar` unconditionally.
pub fn insert_char_smart(
    doc: &mut Document,
    ch: char,
    table: &PairTable,
    auto_pairs: bool,
    auto_indent: bool,
) {
    let single_caret = doc.selections.len() == 1;
    edit_selections_sel(
        doc,
        |d, sel| {
            if sel.is_empty() {
                caret_insert_op(
                    d,
                    sel.head,
                    ch,
                    table,
                    auto_pairs,
                    auto_indent,
                    single_caret,
                )
            } else {
                selection_insert_op(d, sel, ch, table, auto_pairs)
            }
        },
        GroupBreak::None,
    );
}

/// The op for a bare caret: an auto-pair, a type-over step, a closing-bracket dedent, or a
/// plain char insert. Returned as `(range, replacement, (anchor_off, head_off))` for
/// [`edit_selections_sel`].
fn caret_insert_op(
    d: &Document,
    head: usize,
    ch: char,
    table: &PairTable,
    auto_pairs: bool,
    auto_indent: bool,
    single_caret: bool,
) -> (Range<usize>, String, (usize, usize)) {
    if auto_pairs {
        match pairs::decide_insert(table, ch, char_before(d, head), char_at(d, head)) {
            InsertAction::OpenPair(close) => return (head..head, format!("{ch}{close}"), (1, 1)),
            // No text change; the (1, 1) offset steps the caret past the closer.
            InsertAction::TypeOver => return (head..head, String::new(), (1, 1)),
            InsertAction::Literal => {}
        }
    }
    // A closing bracket typed on an all-whitespace prefix dedents the line to align with its
    // opener (plan §1.2 acceptance). SPEC-NOTE: restricted to a single caret so two carets on
    // one line can't produce overlapping ops (the dedent range reaches back to the line start).
    if auto_indent && single_caret && table.is_close_bracket(ch) {
        let line_start = d.line_to_char(d.char_to_line(head));
        let prefix = d.text.slice(line_start..head).to_string();
        if !prefix.is_empty() && prefix.bytes().all(|b| b == b' ' || b == b'\t') {
            let s = format!("{}{ch}", dedent_one(&prefix, d.tab_width));
            let caret = s.chars().count();
            return (line_start..head, s, (caret, caret));
        }
    }
    (head..head, ch.to_string(), (1, 1))
}

/// The op for a non-empty selection: surround it with the pair (keeping the inner text
/// selected) when `ch` opens one, otherwise a plain replace.
fn selection_insert_op(
    d: &Document,
    sel: Selection,
    ch: char,
    table: &PairTable,
    auto_pairs: bool,
) -> (Range<usize>, String, (usize, usize)) {
    if auto_pairs {
        if let Some(close) = table.close_for(ch) {
            let inner = d.text.slice(sel.span()).to_string();
            let inner_len = inner.chars().count();
            return (
                sel.span(),
                format!("{ch}{inner}{close}"),
                (1, 1 + inner_len),
            );
        }
    }
    (sel.span(), ch.to_string(), (1, 1))
}

/// Insert a newline, copying the current line's indent and adjusting one level for brackets
/// (plan §1.2). With `auto_indent` off, inserts a bare newline. `table` supplies the bracket
/// set; a caret sitting between a matched pair expands to an indented, dedented block.
pub fn insert_newline_smart(doc: &mut Document, table: &PairTable, auto_indent: bool) {
    if !auto_indent {
        insert_newline(doc);
        return;
    }
    edit_selections_sel(
        doc,
        |d, sel| {
            let start = sel.from();
            let line = d.char_to_line(start);
            let line_start = d.line_to_char(line);
            let base = leading_ws(&d.line_text(line));
            let before = d.text.slice(line_start..start).to_string();
            let last_open = before
                .trim_end()
                .chars()
                .next_back()
                .filter(|c| table.is_open_bracket(*c));
            let after = char_at(d, sel.to());
            let between =
                matches!((last_open, after), (Some(o), Some(c)) if table.close_for(o) == Some(c));
            let unit = indent_unit(&base, d.tab_width);
            if between {
                // `{|}` → `{`, indented caret line, then a dedented `}`.
                let mid = format!("\n{base}{unit}");
                let caret = mid.chars().count();
                (sel.span(), format!("{mid}\n{base}"), (caret, caret))
            } else if last_open.is_some() {
                let s = format!("\n{base}{unit}");
                let n = s.chars().count();
                (sel.span(), s, (n, n))
            } else {
                let s = format!("\n{base}");
                let n = s.chars().count();
                (sel.span(), s, (n, n))
            }
        },
        GroupBreak::Force,
    );
}

/// Backspace, deleting both members when a caret sits inside an empty auto-pair (`(|)` → ``,
/// plan §1.1). With `auto_pairs` off, identical to [`delete_backward`].
pub fn delete_backward_smart(doc: &mut Document, table: &PairTable, auto_pairs: bool) {
    // SPEC-NOTE: the empty-pair delete reaches *back* to `head - 1`, so with several carets an
    // adjacent one's range could overlap it and corrupt the buffer (Transaction changes must
    // be non-overlapping). Restrict the both-members delete to a single caret — the same guard
    // the close-bracket dedent uses — and fall back to a plain per-caret backspace otherwise.
    let single_caret = doc.selections.len() == 1;
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let head = sel.head;
                if auto_pairs
                    && single_caret
                    && pairs::is_empty_pair(table, char_before(d, head), char_at(d, head))
                {
                    // Remove the open and its close together.
                    return (head - 1..head + 1, String::new());
                }
                let from = motion::resolve(d, head, Motion::Left, 1);
                (from..head, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

/// On-save hygiene (plan §1.4): optionally trim trailing whitespace from every line and/or
/// ensure the buffer ends in a single newline. Applied as one undoable [`Transaction`] *before*
/// the write, so undo restores the pre-save text. Internal storage stays LF — the file's
/// `line_ending` is re-emitted at serialization, never rewritten here (invariant #6). Returns
/// `true` when it changed anything. Selections are mapped through the edit, so a caret sitting
/// past a trimmed line's new end is pulled back to the new EOL.
pub fn apply_save_hygiene(doc: &mut Document, trim_trailing: bool, final_newline: bool) -> bool {
    let before = doc.selections.clone();
    let mut changes: Vec<Change> = Vec::new();

    if trim_trailing {
        for line in 0..doc.len_lines() {
            let text = doc.line_text(line);
            let body = text.trim_end_matches(['\n', '\r']);
            let kept = body.trim_end_matches([' ', '\t']);
            let kept_chars = kept.chars().count();
            let body_chars = body.chars().count();
            if kept_chars < body_chars {
                let line_start = doc.line_to_char(line);
                let start = line_start + kept_chars;
                let end = line_start + body_chars;
                changes.push(Change {
                    at: start,
                    removed: doc.text.slice(start..end).to_string(),
                    inserted: String::new(),
                });
            }
        }
    }

    if final_newline {
        let len = doc.len_chars();
        let ends_with_nl = len > 0 && doc.text.char(len - 1) == '\n';
        if len > 0 && !ends_with_nl {
            changes.push(Change {
                at: len,
                removed: String::new(),
                inserted: "\n".into(),
            });
        }
    }

    if changes.is_empty() {
        return false;
    }
    apply_line_changes(doc, changes, before);
    true
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

    // --- auto-pairs & auto-indent -------------------------------------------

    fn pt() -> PairTable {
        PairTable::default()
    }

    #[test]
    fn auto_pair_inserts_partner_with_caret_between() {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        insert_char_smart(&mut doc, '(', &pt(), true, true);
        assert_eq!(doc.to_string(), "()");
        assert_eq!(doc.selections.primary().head, 1); // caret between the parens
        assert!(doc.dirty);
    }

    #[test]
    fn auto_pair_multi_cursor() {
        // Three carets at the three line starts (plan §1.1 acceptance).
        let mut doc = Document::from_str("a\nb\nc");
        multi_caret(&mut doc, &[0, 2, 4]);
        insert_char_smart(&mut doc, '(', &pt(), true, true);
        assert_eq!(doc.to_string(), "()a\n()b\n()c");
        assert_eq!(doc.selections.len(), 3);
        for s in doc.selections.ranges() {
            assert!(s.is_empty(), "each caret stays a caret");
            assert_eq!(doc.text.char(s.head - 1), '(');
            assert_eq!(doc.text.char(s.head), ')');
        }
    }

    #[test]
    fn auto_pair_type_over_is_not_an_edit() {
        let mut doc = Document::from_str("()");
        doc.set_caret(1); // between the parens
        doc.dirty = false;
        insert_char_smart(&mut doc, ')', &pt(), true, true);
        assert_eq!(doc.to_string(), "()"); // no duplicate closer
        assert_eq!(doc.selections.primary().head, 2); // stepped past
        assert!(!doc.dirty, "stepping over a closer is not a buffer change");
    }

    #[test]
    fn auto_pair_backspace_deletes_both() {
        let mut doc = Document::from_str("()");
        doc.set_caret(1);
        delete_backward_smart(&mut doc, &pt(), true);
        assert_eq!(doc.to_string(), "");
        assert_eq!(doc.selections.primary().head, 0);
    }

    #[test]
    fn multi_cursor_backspace_never_overlaps() {
        // Regression: the empty-pair delete reaches back to head-1; with a second caret at
        // head+1 the two ranges must not overlap and corrupt the buffer. `()x` with carets
        // at 1 and 2 backspaces to `x` (both members gone, `x` preserved) — never empty.
        let mut doc = Document::from_str("()x");
        doc.selections = Selections::from_iter([Selection::caret(1), Selection::caret(2)]);
        delete_backward_smart(&mut doc, &pt(), true);
        assert_eq!(doc.to_string(), "x");
    }

    #[test]
    fn quote_after_word_is_literal() {
        let mut doc = Document::from_str("don");
        doc.set_caret(3);
        insert_char_smart(&mut doc, '\'', &pt(), true, true);
        assert_eq!(doc.to_string(), "don'"); // not don''
                                             // At a boundary it still auto-closes.
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        insert_char_smart(&mut doc, '"', &pt(), true, true);
        assert_eq!(doc.to_string(), "\"\"");
    }

    #[test]
    fn surround_selection_with_pair() {
        let mut doc = Document::from_str("word");
        doc.selections = Selections::single(Selection::new(0, 4));
        insert_char_smart(&mut doc, '(', &pt(), true, true);
        assert_eq!(doc.to_string(), "(word)");
        let s = doc.selections.primary();
        assert_eq!((s.from(), s.to()), (1, 5)); // inner text stays selected
    }

    #[test]
    fn auto_pairs_off_is_plain_insert() {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        insert_char_smart(&mut doc, '(', &pt(), false, false);
        assert_eq!(doc.to_string(), "(");
        assert_eq!(doc.selections.primary().head, 1);
    }

    #[test]
    fn auto_pair_undo_removes_both_in_one_step() {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        insert_char_smart(&mut doc, '[', &pt(), true, true);
        assert_eq!(doc.to_string(), "[]");
        undo(&mut doc);
        assert_eq!(doc.to_string(), "");
    }

    #[test]
    fn newline_copies_indent() {
        let mut doc = Document::from_str("    foo");
        doc.set_caret(7);
        insert_newline_smart(&mut doc, &pt(), true);
        assert_eq!(doc.to_string(), "    foo\n    ");
        assert_eq!(doc.selections.primary().head, doc.len_chars());
    }

    #[test]
    fn newline_after_open_brace_indents() {
        let mut doc = Document::from_str("fn f() {");
        doc.set_caret(8);
        insert_newline_smart(&mut doc, &pt(), true);
        assert_eq!(doc.to_string(), "fn f() {\n    ");
    }

    #[test]
    fn newline_between_braces_expands() {
        let mut doc = Document::from_str("{}");
        doc.set_caret(1);
        insert_newline_smart(&mut doc, &pt(), true);
        assert_eq!(doc.to_string(), "{\n    \n}");
        // Caret sits on the indented middle line.
        assert_eq!(doc.char_to_line(doc.selections.primary().head), 1);
    }

    #[test]
    fn typing_close_brace_dedents_whitespace_line() {
        let mut doc = Document::from_str("fn f() {\n    ");
        doc.set_caret(doc.len_chars());
        insert_char_smart(&mut doc, '}', &pt(), true, true);
        assert_eq!(doc.to_string(), "fn f() {\n}");
    }

    #[test]
    fn newline_auto_indent_off_is_bare() {
        let mut doc = Document::from_str("    foo");
        doc.set_caret(7);
        insert_newline_smart(&mut doc, &pt(), false);
        assert_eq!(doc.to_string(), "    foo\n");
    }

    #[test]
    fn save_hygiene_trims_and_moves_caret_off_eol() {
        let mut doc = Document::from_str("foo   \nbar\t\nbaz");
        doc.set_caret(6); // just past "foo", at the old trailing-space EOL of line 0
        let changed = apply_save_hygiene(&mut doc, true, false);
        assert!(changed);
        assert_eq!(doc.to_string(), "foo\nbar\nbaz");
        // The caret is pulled back to the new EOL, never left past it.
        assert_eq!(doc.selections.primary().head, 3);
        undo(&mut doc);
        assert_eq!(doc.to_string(), "foo   \nbar\t\nbaz");
    }

    #[test]
    fn save_hygiene_inserts_final_newline_only_when_missing() {
        let mut doc = Document::from_str("abc");
        assert!(apply_save_hygiene(&mut doc, false, true));
        assert_eq!(doc.to_string(), "abc\n");
        // Idempotent: a file already ending in a newline is untouched.
        let mut doc = Document::from_str("abc\n");
        assert!(!apply_save_hygiene(&mut doc, false, true));
        assert_eq!(doc.to_string(), "abc\n");
    }

    #[test]
    fn save_hygiene_noop_when_both_off() {
        let mut doc = Document::from_str("foo  \n");
        assert!(!apply_save_hygiene(&mut doc, false, false));
        assert_eq!(doc.to_string(), "foo  \n");
    }

    #[test]
    fn save_hygiene_preserves_crlf_line_ending() {
        // Trimming operates on internal LF text; the file's CRLF style is a Document flag that
        // serialization re-emits — hygiene must not touch it (invariant #6).
        let mut doc = Document::from_str("foo  \r\nbar");
        assert_eq!(doc.line_ending, crate::document::LineEnding::Crlf);
        apply_save_hygiene(&mut doc, true, true);
        assert_eq!(doc.to_string(), "foo\nbar\n"); // internal LF, trimmed, final newline
        assert_eq!(doc.line_ending, crate::document::LineEnding::Crlf);
    }
}
