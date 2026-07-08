//! The find/replace widget: opening, incremental match recompute, and replace.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Handle a key while the find/replace widget is open.
    pub(super) fn find_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Esc => {
                self.editor.find = None;
            }
            KeyCode::Enter if alt => self.replace_current(),
            KeyCode::Char('a' | 'A') if alt => self.replace_all(),
            KeyCode::Char('c' | 'C') if alt => {
                toggle_and(&mut self.editor.find, |f| {
                    f.case_sensitive = !f.case_sensitive
                });
                self.recompute_find();
            }
            KeyCode::Char('w' | 'W') if alt => {
                toggle_and(&mut self.editor.find, |f| f.whole_word = !f.whole_word);
                self.recompute_find();
            }
            KeyCode::Char('r' | 'R') if alt => {
                toggle_and(&mut self.editor.find, |f| f.regex = !f.regex);
                self.recompute_find();
            }
            KeyCode::Enter if shift => {
                toggle_and(&mut self.editor.find, |f| f.select_prev());
                self.focus_current_match();
            }
            KeyCode::Up => {
                toggle_and(&mut self.editor.find, |f| f.select_prev());
                self.focus_current_match();
            }
            KeyCode::Enter | KeyCode::Down => {
                toggle_and(&mut self.editor.find, |f| f.select_next());
                self.focus_current_match();
            }
            KeyCode::Tab => toggle_and(&mut self.editor.find, |f| f.toggle_field()),
            KeyCode::Backspace => {
                toggle_and(&mut self.editor.find, |f| f.backspace());
                self.recompute_find();
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                toggle_and(&mut self.editor.find, |f| f.input_char(c));
                self.recompute_find();
            }
            _ => {}
        }
    }

    /// Open the find (or find+replace) widget, prefilling from the current selection.
    pub(super) fn open_find(&mut self, replace_mode: bool) {
        let mut fs = FindState::new(replace_mode);
        if let Some(doc) = self.editor.active_document() {
            let sel = doc.selections.primary();
            fs.origin = sel.from();
            if !sel.is_empty() {
                fs.query = doc.text.slice(sel.from()..sel.to()).to_string();
            }
        }
        self.editor.find = Some(fs);
        self.recompute_find();
    }

    /// Recompute matches against the active document and move to the current one.
    pub(super) fn recompute_find(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let text = {
            let Some(doc) = self.editor.workspace.documents.get(id) else {
                return;
            };
            doc.text.to_string()
        };
        if let Some(find) = &mut self.editor.find {
            let origin = find.origin;
            find.recompute(&text, origin);
        }
        self.focus_current_match();
    }

    /// Select the current match in the editor so it scrolls into view + highlights.
    pub(super) fn focus_current_match(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let m = self.editor.find.as_ref().and_then(|f| f.current_match());
        if let (Some((s, e)), Some(doc)) = (m, self.editor.workspace.documents.get_mut(id)) {
            doc.selections.set_single(Selection::new(s, e));
        }
    }

    /// Replace the current match with the (capture-expanded) replacement.
    pub(super) fn replace_current(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some((s, e)) = self.editor.find.as_ref().and_then(|f| f.current_match()) else {
            return;
        };
        let matched = {
            let Some(doc) = self.editor.workspace.documents.get(id) else {
                return;
            };
            doc.text.slice(s..e).to_string()
        };
        let repl = self
            .editor
            .find
            .as_ref()
            .map(|f| f.replacement_for(&matched))
            .unwrap_or_default();
        let txn = {
            let doc = &self.editor.workspace.documents[id];
            Transaction::replace(doc, s..e, &repl)
        };
        self.editor.apply_transaction(id, txn);
        self.recompute_find();
    }

    /// Replace every match in one undoable transaction (plan §6).
    pub(super) fn replace_all(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let matches = self
            .editor
            .find
            .as_ref()
            .map(|f| f.matches.clone())
            .unwrap_or_default();
        if matches.is_empty() {
            return;
        }
        let mut changes = Vec::with_capacity(matches.len());
        {
            let doc = &self.editor.workspace.documents[id];
            for &(s, e) in &matches {
                let matched = doc.text.slice(s..e).to_string();
                let inserted = self
                    .editor
                    .find
                    .as_ref()
                    .map(|f| f.replacement_for(&matched))
                    .unwrap_or_default();
                changes.push(Change {
                    at: s,
                    removed: matched,
                    inserted,
                });
            }
        }
        let n = changes.len();
        self.editor
            .apply_transaction(id, Transaction::from_changes(changes));
        self.recompute_find();
        self.editor.status_message = Some(format!("Replaced {n} occurrence(s)"));
    }

    // --- picker (palette / quick open / goto line) -----------------------------
}
