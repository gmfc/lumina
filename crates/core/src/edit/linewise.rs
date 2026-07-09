//! Line-oriented edits: duplicate, comment toggle, indent/outdent, and line moves.

use crate::document::Document;
use crate::history::GroupBreak;
use crate::selection::{Selection, Selections};
use crate::transaction::{Change, Transaction};

use super::apply::edit_selections_sel;
use super::helpers::{
    affected_lines, apply_line_changes, join_region, last_content_line, leading_ws,
    line_col_to_char, region_end_char, split_region,
};
use super::insert::insert_text;

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

/// Copy the line(s) covered by each selection *upward* (Shift+Alt+Up): a duplicate is
/// inserted above each affected line, mirroring [`duplicate_line`]'s downward copy.
pub fn copy_line_up(doc: &mut Document) {
    let before = doc.selections.clone();
    let changes: Vec<Change> = affected_lines(doc)
        .iter()
        .map(|&l| {
            let start = doc.line_to_char(l);
            if l + 1 < doc.len_lines() {
                let end = doc.line_to_char(l + 1); // includes this line's newline
                let body = doc.text.slice(start..end).to_string();
                Change {
                    at: start,
                    removed: String::new(),
                    inserted: body,
                }
            } else {
                // Last line has no trailing newline; append one to the inserted copy so the
                // original still starts on a fresh line below it.
                let end = doc.len_chars();
                let body = doc.text.slice(start..end).to_string();
                Change {
                    at: start,
                    removed: String::new(),
                    inserted: format!("{body}\n"),
                }
            }
        })
        .collect();
    apply_line_changes(doc, changes, before);
}

/// Delete every line any selection touches, whole (VS Code `editor.action.deleteLines`).
/// Selections collapse to a caret on the line that slides up into the removed line's place.
pub fn delete_lines(doc: &mut Document) {
    let before = doc.selections.clone();
    let lines = affected_lines(doc);
    let last_content = last_content_line(doc);
    let has_final_nl = doc.len_chars() > 0 && doc.text.char(doc.len_chars() - 1) == '\n';
    let line_set: std::collections::BTreeSet<usize> = lines.iter().copied().collect();

    let mut changes = Vec::new();
    for &l in &lines {
        if l > last_content {
            continue; // the phantom trailing empty line carries no content to remove
        }
        let mut start = doc.line_to_char(l);
        let end = region_end_char(doc, l);
        // The final line of a buffer without a trailing newline owns no line break of its
        // own; eat the *preceding* newline instead, unless the line above is going too (its
        // own removal already takes that break).
        if l == last_content && !has_final_nl && start > 0 && !line_set.contains(&(l - 1)) {
            start -= 1;
        }
        changes.push(Change {
            at: start,
            removed: doc.text.slice(start..end).to_string(),
            inserted: String::new(),
        });
    }
    if changes.is_empty() {
        return;
    }
    apply_line_changes(doc, changes, before);
}

/// Open a fresh line below each caret's line and move there, copying the line's indent
/// (Ctrl+Enter — VS Code `editor.action.insertLineAfter`). Works from any column.
pub fn insert_line_below(doc: &mut Document) {
    insert_blank_line(doc, false);
}

/// Open a fresh line above each caret's line and move there, copying the line's indent
/// (Ctrl+Shift+Enter — VS Code `editor.action.insertLineBefore`).
pub fn insert_line_above(doc: &mut Document) {
    insert_blank_line(doc, true);
}

/// Shared body for [`insert_line_below`]/[`insert_line_above`]. Selections are first
/// collapsed to one caret per distinct line so two cursors on the same line don't each open
/// a line; each caret then lands at the indent of the new line.
fn insert_blank_line(doc: &mut Document, above: bool) {
    let mut lines: Vec<usize> = doc
        .selections
        .ranges()
        .iter()
        .map(|s| doc.char_to_line(s.head))
        .collect();
    lines.sort_unstable();
    lines.dedup();
    let carets: Vec<Selection> = lines
        .iter()
        .map(|&l| Selection::caret(doc.line_to_char(l)))
        .collect();
    doc.selections = Selections::from_iter(carets);

    edit_selections_sel(
        doc,
        |d, sel| {
            let l = d.char_to_line(sel.head);
            let text = d.line_text(l);
            let indent = leading_ws(text.trim_end_matches(['\n', '\r']));
            let indent_chars = indent.chars().count();
            if above {
                let at = d.line_to_char(l);
                (at..at, format!("{indent}\n"), (indent_chars, indent_chars))
            } else {
                let at = d.line_to_char(l) + d.line_len_chars(l);
                let off = 1 + indent_chars;
                (at..at, format!("\n{indent}"), (off, off))
            }
        },
        crate::history::GroupBreak::Force,
    );
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
