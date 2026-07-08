//! The LSP completion popup: open, filter, navigate, and accept.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Open a caret-anchored completion popup from server `items` (plan §2.1). Anchors at the
    /// start of the identifier under the caret so the popup filters on what's already typed.
    pub(super) fn open_completion(&mut self, items: Vec<editor_lsp::CompletionItem>) {
        if items.is_empty() {
            return;
        }
        let Some(doc) = self.editor.active_document() else {
            return;
        };
        let head = doc.selections.primary().head;
        let mut anchor = head;
        while anchor > 0 {
            let ch = doc.text.char(anchor - 1);
            if ch.is_alphanumeric() || ch == '_' {
                anchor -= 1;
            } else {
                break;
            }
        }
        let prefix = doc.text.slice(anchor..head).to_string();
        let state = crate::completion::CompletionState::new(items, anchor, prefix);
        if !state.is_empty() {
            self.editor.completion = Some(state);
        }
    }

    /// Handle a key while the completion popup is open. Returns `true` when it fully consumed
    /// the key (navigation / accept / dismiss); `false` lets the key edit the buffer normally,
    /// after which `refresh_completion` re-syncs the popup to the new text.
    pub(super) fn completion_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Down => {
                if let Some(c) = self.editor.completion.as_mut() {
                    c.move_sel(1);
                }
                true
            }
            KeyCode::Up => {
                if let Some(c) = self.editor.completion.as_mut() {
                    c.move_sel(-1);
                }
                true
            }
            KeyCode::Esc => {
                self.editor.completion = None;
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                let insert = self
                    .editor
                    .completion
                    .as_ref()
                    .and_then(|c| c.selected_item().map(|it| it.insert_text.clone()));
                self.editor.completion = None;
                if let Some(insert) = insert {
                    self.insert_completion(&insert);
                }
                true
            }
            _ => false,
        }
    }

    /// After a buffer edit while the popup is open, recompute the typed prefix and re-filter,
    /// dismissing when the caret leaves the identifier or nothing matches.
    pub(super) fn refresh_completion(&mut self) {
        let Some(anchor) = self.editor.completion.as_ref().map(|c| c.anchor) else {
            return;
        };
        let prefix = self.editor.active_document().and_then(|doc| {
            let head = doc.selections.primary().head;
            if head < anchor {
                return None;
            }
            let p = doc.text.slice(anchor..head).to_string();
            if p.chars().any(|c| !(c.is_alphanumeric() || c == '_')) {
                return None;
            }
            Some(p)
        });
        match prefix {
            None => self.editor.completion = None,
            Some(prefix) => {
                if let Some(c) = self.editor.completion.as_mut() {
                    c.prefix = prefix;
                    c.refilter();
                    if c.is_empty() {
                        self.editor.completion = None;
                    }
                }
            }
        }
    }

    /// Insert a completion, replacing the identifier prefix already typed before the cursor.
    pub(super) fn insert_completion(&mut self, insert: &str) {
        self.with_doc(|d| {
            let head = d.selections.primary().head;
            // Walk back over identifier chars to find the prefix to replace.
            let mut start = head;
            while start > 0 {
                let ch = d.text.char(start - 1);
                if ch.is_alphanumeric() || ch == '_' {
                    start -= 1;
                } else {
                    break;
                }
            }
            editor_core::edit::edit_selections(
                d,
                |_doc, sel| {
                    if sel.head == head {
                        (start..head, insert.to_string())
                    } else {
                        (sel.span(), insert.to_string())
                    }
                },
                editor_core::GroupBreak::Force,
            );
        });
    }
}
