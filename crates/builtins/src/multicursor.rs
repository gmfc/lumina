//! Multi-cursor commands, implemented **as a plugin** (invariant #3) — the first feature
//! besides the explorer to reach the editor only through [`Host`].
//!
//! Add a cursor at the next occurrence of the selection (`Ctrl+D`), select every occurrence
//! (`Ctrl+F2`), or drop a caret on the line above/below (`Ctrl+Alt+Up`/`Down`). Each command
//! computes a new [`Selections`] set purely from the active document and installs it through
//! [`Host::set_selections`] — no direct editor-state access, no privileged path.

use editor_core::{motion, view, Document, Selection, Selections};
use editor_plugin::{Contributions, Host, Plugin};

pub struct MultiCursorPlugin;

impl Plugin for MultiCursorPlugin {
    fn id(&self) -> &str {
        "multicursor"
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("cursor.addNextMatch", "Multi-cursor: Add Next Match")
            .command(
                "cursor.selectAllMatches",
                "Multi-cursor: Select All Occurrences",
            )
            .command("cursor.addAbove", "Multi-cursor: Add Cursor Above")
            .command("cursor.addBelow", "Multi-cursor: Add Cursor Below")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        let compute = match command_id {
            "cursor.addNextMatch" => add_next_match as fn(&Document) -> Option<Selections>,
            "cursor.selectAllMatches" => select_all_matches,
            "cursor.addAbove" => |d: &Document| add_vertical(d, -1),
            "cursor.addBelow" => |d: &Document| add_vertical(d, 1),
            _ => return false,
        };
        let Some(id) = host.active_doc() else {
            return true; // ours, but nothing to act on
        };
        let new_sels = host.workspace().documents.get(id).and_then(compute);
        if let Some(sels) = new_sels {
            host.set_selections(id, sels);
        }
        true
    }
}

/// `Ctrl+D`: with a bare caret, select the word under it; with a selection, add the next
/// occurrence of the selected text (wrapping) and make it primary.
fn add_next_match(doc: &Document) -> Option<Selections> {
    let primary = doc.selections.primary();
    if primary.is_empty() {
        let (s, e) = motion::word_at(doc, primary.head);
        return (s < e).then(|| Selections::single(Selection::new(s, e)));
    }
    let chars: Vec<char> = doc.rope().chars().collect();
    let needle: Vec<char> = chars[primary.from()..primary.to()].to_vec();
    if needle.is_empty() {
        return None;
    }
    let search_from = doc
        .selections
        .ranges()
        .iter()
        .map(|s| s.to())
        .max()
        .unwrap_or(0);
    let (ms, me) = find_next_occurrence(&chars, &needle, search_from)?;
    // Already selected? Nothing to add.
    if doc
        .selections
        .ranges()
        .iter()
        .any(|s| s.from() == ms && s.to() == me)
    {
        return None;
    }
    let mut set = doc.selections.clone();
    set.push(Selection::new(ms, me));
    set.normalize();
    if let Some(idx) = set.ranges().iter().position(|s| s.head == me) {
        set.set_primary(idx);
    }
    Some(set)
}

/// `Ctrl+F2`: replace the set with one selection over every occurrence of the current
/// selection's text (or the word under a bare caret), so a subsequent edit rewrites them all.
fn select_all_matches(doc: &Document) -> Option<Selections> {
    let primary = doc.selections.primary();
    let (from, to) = if primary.is_empty() {
        motion::word_at(doc, primary.head)
    } else {
        (primary.from(), primary.to())
    };
    if from >= to {
        return None;
    }
    let chars: Vec<char> = doc.rope().chars().collect();
    let needle = &chars[from..to];
    let (n, m) = (chars.len(), needle.len());
    let mut sels: Vec<Selection> = Vec::new();
    let mut i = 0;
    while i + m <= n {
        if &chars[i..i + m] == needle {
            sels.push(Selection::new(i, i + m));
            i += m;
        } else {
            i += 1;
        }
    }
    if sels.is_empty() {
        return None;
    }
    let mut set = Selections::from_iter(sels);
    // Keep the caret's original match primary, so the viewport doesn't jump.
    if let Some(idx) = set.ranges().iter().position(|s| s.from() >= from) {
        set.set_primary(idx);
    }
    Some(set)
}

/// `Ctrl+Alt+Up`/`Down`: add a caret one line above/below at the same display column.
fn add_vertical(doc: &Document, dir: isize) -> Option<Selections> {
    let primary = doc.selections.primary();
    let (line, col) = doc.char_to_line_col(primary.head);
    let line_text = doc.line_text(line);
    let line_body = line_text.trim_end_matches(['\n', '\r']);
    let display_col = view::char_to_display_col(line_body, col, doc.tab_width);
    let target = (line as isize + dir).clamp(0, doc.len_lines() as isize - 1) as usize;
    if target == line {
        return None;
    }
    let target_text = doc.line_text(target);
    let target_body = target_text.trim_end_matches(['\n', '\r']);
    let ch = view::display_col_to_char(target_body, display_col, doc.tab_width);
    let head = doc.line_to_char(target) + ch;
    let mut set = doc.selections.clone();
    set.push(Selection::caret(head));
    set.normalize();
    if let Some(idx) = set.ranges().iter().position(|s| s.head == head) {
        set.set_primary(idx);
    }
    Some(set)
}

/// Find the next occurrence of `needle` in `chars` at/after `from`, wrapping to the start.
fn find_next_occurrence(chars: &[char], needle: &[char], from: usize) -> Option<(usize, usize)> {
    let n = chars.len();
    let m = needle.len();
    if m == 0 || m > n {
        return None;
    }
    let span = n - m + 1; // number of valid start positions
    for off in 0..span {
        let i = (from + off) % span;
        if &chars[i..i + m] == needle {
            return Some((i, i + m));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(text: &str, sel: Selection) -> Document {
        let mut d = Document::from_str(text);
        d.selections = Selections::single(sel);
        d
    }

    #[test]
    fn add_next_match_selects_word_then_next_occurrence() {
        // Bare caret in "foo": first press selects the word.
        let mut d = doc_with("foo foo foo", Selection::caret(1));
        let s = add_next_match(&d).unwrap();
        assert_eq!(s.ranges().len(), 1);
        assert_eq!(s.primary().span(), 0..3);
        d.selections = s;
        // Second press adds the next "foo" and makes it primary.
        let s = add_next_match(&d).unwrap();
        assert_eq!(s.ranges().len(), 2);
        assert_eq!(s.primary().span(), 4..7);
    }

    #[test]
    fn select_all_matches_covers_every_occurrence() {
        let d = doc_with("x x x x", Selection::new(0, 1));
        let s = select_all_matches(&d).unwrap();
        assert_eq!(s.ranges().len(), 4);
    }

    #[test]
    fn add_below_adds_caret_on_next_line() {
        let d = doc_with("abc\ndef\nghi", Selection::caret(1)); // line 0, col 1
        let s = add_vertical(&d, 1).unwrap();
        assert_eq!(s.ranges().len(), 2);
        // second caret sits at col 1 of line 1 => char offset 5
        assert!(s.ranges().iter().any(|r| r.head == 5));
    }
}
