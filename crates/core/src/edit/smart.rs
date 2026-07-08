//! Auto-closing pairs and auto-indent (plan §1.1, §1.2). Each entry point degrades to a plain
//! insert/delete when its flags are off, so these can back every keystroke unconditionally.

use std::ops::Range;

use crate::document::Document;
use crate::history::GroupBreak;
use crate::motion::{self, Motion};
use crate::pairs::{self, InsertAction, PairTable};
use crate::selection::Selection;

use super::apply::{edit_selections, edit_selections_sel};
use super::helpers::{char_at, char_before, dedent_one, indent_unit, leading_ws};
use super::insert::insert_newline;

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
/// plan §1.1). With `auto_pairs` off, identical to [`super::delete_backward`].
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
