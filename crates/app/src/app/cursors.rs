//! Multi-cursor commands and the theme/selection helpers.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Ctrl+D: first press selects the word under the cursor; each subsequent press adds a
    /// selection at the next occurrence of the current selection's text (wrapping).
    pub(super) fn add_cursor_next_match(&mut self) {
        let Some(doc) = self.editor.active_document_mut() else {
            return;
        };
        let primary = doc.selections.primary();
        if primary.is_empty() {
            let (s, e) = motion::word_at(doc, primary.head);
            if s < e {
                doc.selections.set_single(Selection::new(s, e));
            }
            return;
        }
        let chars: Vec<char> = doc.rope().chars().collect();
        let needle: Vec<char> = chars[primary.from()..primary.to()].to_vec();
        if needle.is_empty() {
            return;
        }
        let search_from = doc
            .selections
            .ranges()
            .iter()
            .map(|s| s.to())
            .max()
            .unwrap_or(0);
        if let Some((ms, me)) = find_next_occurrence(&chars, &needle, search_from) {
            // Skip if that range is already selected.
            if doc
                .selections
                .ranges()
                .iter()
                .any(|s| s.from() == ms && s.to() == me)
            {
                return;
            }
            doc.selections.push(Selection::new(ms, me));
            doc.selections.normalize();
            // Make the newly added match the primary so the viewport follows it.
            if let Some(idx) = doc.selections.ranges().iter().position(|s| s.head == me) {
                doc.selections.set_primary(idx);
            }
        }
    }

    /// Alt+Up / Alt+Down: add a caret one line above/below at the same display column.
    pub(super) fn add_cursor_vertical(&mut self, dir: isize) {
        let Some(doc) = self.editor.active_document_mut() else {
            return;
        };
        let primary = doc.selections.primary();
        let (line, col) = doc.char_to_line_col(primary.head);
        let line_text = doc.line_text(line);
        let line_body = line_text.trim_end_matches(['\n', '\r']);
        let display_col = editor_core::view::char_to_display_col(line_body, col, doc.tab_width);
        let target = (line as isize + dir).clamp(0, doc.len_lines() as isize - 1) as usize;
        if target == line {
            return;
        }
        let target_text = doc.line_text(target);
        let target_body = target_text.trim_end_matches(['\n', '\r']);
        let ch = editor_core::view::display_col_to_char(target_body, display_col, doc.tab_width);
        let head = doc.line_to_char(target) + ch;
        doc.selections.push(Selection::caret(head));
        doc.selections.normalize();
        if let Some(idx) = doc.selections.ranges().iter().position(|s| s.head == head) {
            doc.selections.set_primary(idx);
        }
    }

    /// Ctrl+F2 / Select All Occurrences: replace the selection set with one selection over
    /// every occurrence of the current selection's text (or the word under a bare caret), so a
    /// subsequent edit rewrites them all at once.
    pub(super) fn select_all_matches(&mut self) {
        let Some(doc) = self.editor.active_document_mut() else {
            return;
        };
        let primary = doc.selections.primary();
        let (from, to) = if primary.is_empty() {
            motion::word_at(doc, primary.head)
        } else {
            (primary.from(), primary.to())
        };
        if from >= to {
            return;
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
            return;
        }
        let mut set = editor_core::Selections::from_iter(sels);
        // Keep the caret's original match primary, so the viewport doesn't jump.
        if let Some(idx) = set.ranges().iter().position(|s| s.from() >= from) {
            set.set_primary(idx);
        }
        doc.selections = set;
        doc.view.goal_col = None;
    }

    /// Toggle between the dark and light themes.
    pub(super) fn toggle_theme(&mut self) {
        let truecolor = crate::theme::truecolor_supported();
        self.theme = if self.theme.is_dark() {
            crate::theme::Theme::default_light(truecolor)
        } else {
            crate::theme::Theme::default_dark(truecolor)
        };
        self.editor.status_message = Some(format!(
            "Theme: {}",
            if self.theme.is_dark() {
                "dark"
            } else {
                "light"
            }
        ));
    }

    // --- clipboard -------------------------------------------------------------

    pub(super) fn selection_text(&self) -> Option<String> {
        let doc = self.editor.active_document()?;
        let sel = doc.selections.primary();
        if sel.is_empty() {
            None
        } else {
            Some(doc.rope().slice(sel.from()..sel.to()).to_string())
        }
    }
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
