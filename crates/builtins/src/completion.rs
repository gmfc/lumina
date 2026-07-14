//! The LSP completion popup, implemented **as a plugin** (invariant #3).
//!
//! Owns the whole completion feature: the `lsp.completion` trigger (issued through
//! [`Host::lsp_request`]), the filter model ([`CompletionState`], moved here and retyped onto the
//! primitive [`LspCompletionItem`]), and the caret-anchored popup published through the POPUP port
//! ([`Host::set_popup`]). While the popup is up the app routes navigation keys to
//! [`Plugin::on_popup_key`]; other keys edit the buffer and the plugin re-syncs on `DidChange`.
//! Accepting an item edits through [`Host::apply_transaction`] (multi-cursor aware) — the LSP
//! transport + the on-screen popup geometry stay app-side.

use editor_plugin::{
    Contributions, Event, Host, Key, KeyCode, LspCompletionItem, LspRequestKind, LspTextEdit,
    LspWorkspaceEdit, Plugin, Popup, PopupRow,
};

mod state;
use state::*;

/// True for identifier characters — the popup anchors at the start of the identifier under the
/// caret and filters on what's typed since.
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `prev` (the char before the caret), with `prev2` before it, is a completion trigger:
/// `.` (member access) or `::` (path). Pure so it unit-tests.
fn is_trigger(prev: char, prev2: Option<char>) -> bool {
    prev == '.' || (prev == ':' && prev2 == Some(':'))
}

#[derive(Default)]
pub(crate) struct CompletionPlugin {
    state: Option<CompletionState>,
    /// The server truncated the list — re-request on each keystroke instead of filtering locally.
    is_incomplete: bool,
}

impl CompletionPlugin {
    const ID: &'static str = "completion";

    /// Whether the char just typed is a completion trigger (`.` or `::`) — member/path access,
    /// where the editor should offer completions automatically *(≈ VS Code 24×7 IntelliSense)*.
    /// A capability-driven trigger set is a later refinement; `.`/`::` cover most languages.
    fn at_trigger_char(host: &dyn Host) -> bool {
        let Some(doc) = host
            .active_doc()
            .and_then(|id| host.workspace().documents.get(id))
        else {
            return false;
        };
        let head = doc.selections.primary().head;
        if head == 0 {
            return false;
        }
        let rope = doc.rope();
        let prev = rope.char(head - 1);
        let prev2 = (head >= 2).then(|| rope.char(head - 2));
        is_trigger(prev, prev2)
    }

    /// Open a popup from server `items`, anchored at the start of the identifier under the caret.
    fn open(&mut self, items: Vec<LspCompletionItem>, is_incomplete: bool, host: &mut dyn Host) {
        self.is_incomplete = is_incomplete;
        if items.is_empty() {
            return;
        }
        let Some(id) = host.active_doc() else {
            return;
        };
        let Some((anchor, prefix)) = host.workspace().documents.get(id).map(|doc| {
            let head = doc.selections.primary().head;
            let mut anchor = head;
            while anchor > 0 && is_ident(doc.rope().char(anchor - 1)) {
                anchor -= 1;
            }
            let prefix = doc.rope().slice(anchor..head).to_string();
            (anchor, prefix)
        }) else {
            return;
        };
        let state = CompletionState::new(items, anchor, prefix);
        if state.is_empty() {
            return;
        }
        self.state = Some(state);
        self.publish(host);
    }

    /// After an edit / caret move, recompute the typed prefix and re-filter, dismissing when the
    /// caret leaves the identifier or nothing matches.
    fn refresh(&mut self, host: &mut dyn Host) {
        let Some(anchor) = self.state.as_ref().map(|s| s.anchor) else {
            return;
        };
        let prefix = host.active_doc().and_then(|id| {
            host.workspace().documents.get(id).and_then(|doc| {
                let head = doc.selections.primary().head;
                if head < anchor {
                    return None;
                }
                let p = doc.rope().slice(anchor..head).to_string();
                p.chars().all(is_ident).then_some(p)
            })
        });
        match prefix {
            None => self.dismiss(host),
            Some(prefix) => {
                let empty = match self.state.as_mut() {
                    Some(s) => {
                        s.prefix = prefix;
                        s.refilter();
                        s.is_empty()
                    }
                    None => true,
                };
                if empty {
                    self.dismiss(host);
                } else {
                    self.publish(host);
                }
            }
        }
    }

    /// Accept the selected item, replacing the identifier prefix before each caret, then applying
    /// any `additionalTextEdits` (auto-imports) — eagerly if present, else via
    /// `completionItem/resolve` (§5.2).
    fn accept(&mut self, host: &mut dyn Host) {
        let item = self.state.as_ref().and_then(|s| s.selected_item().cloned());
        self.dismiss(host); // clear before editing so our own DidChange is a no-op
        let Some(item) = item else {
            return;
        };
        let Some(id) = host.active_doc() else {
            return;
        };
        if item.is_snippet {
            Self::accept_snippet(host, id, &item.insert_text);
        } else {
            Self::accept_text(host, id, &item.insert_text);
        }
        Self::apply_followups(host, item);
    }

    /// Replace the identifier prefix before each caret with plain `insert` text (the non-snippet
    /// accept path), leaving the caret after the inserted text.
    fn accept_text(host: &mut dyn Host, id: editor_core::DocId, insert: &str) {
        let insert = insert.to_string();
        let built = host.workspace().documents.get(id).map(|doc| {
            let head = doc.selections.primary().head;
            let mut start = head;
            while start > 0 && is_ident(doc.rope().char(start - 1)) {
                start -= 1;
            }
            editor_core::edit::selection_edit_transaction(doc, |_d, sel| {
                if sel.head == head {
                    (start..head, insert.clone())
                } else {
                    (sel.span(), insert.clone())
                }
            })
        });
        if let Some((txn, after)) = built {
            host.apply_transaction(id, txn);
            host.set_selections(id, after);
        }
    }

    /// After inserting: apply eager `additionalTextEdits` (auto-imports) or resolve them lazily,
    /// then run any post-accept command (e.g. `editor.action.triggerSuggest`) via the shim.
    fn apply_followups(host: &mut dyn Host, item: LspCompletionItem) {
        if !item.additional_edits.is_empty() {
            Self::apply_additional_edits(host, &item.additional_edits);
        } else if let Some(data) = item.data {
            if host.lsp_enabled() {
                host.lsp_request(LspRequestKind::ResolveCompletion {
                    label: item.label,
                    data,
                });
            }
        }
        if let Some((command, arguments)) = item.command {
            host.lsp_request(LspRequestKind::ExecuteCommand { command, arguments });
        }
    }

    /// Expand and insert a snippet at the primary cursor, replacing the typed prefix and placing
    /// the caret at the first tabstop (selecting its placeholder). Multi-cursor snippet accepts
    /// collapse to the primary cursor; full tabstop cycling is a follow-up.
    fn accept_snippet(host: &mut dyn Host, id: editor_core::DocId, raw: &str) {
        let snip = crate::snippet::expand(raw);
        let Some((start, removed)) = host.workspace().documents.get(id).map(|doc| {
            let head = doc.selections.primary().head;
            let mut start = head;
            while start > 0 && is_ident(doc.rope().char(start - 1)) {
                start -= 1;
            }
            (start, doc.rope().slice(start..head).to_string())
        }) else {
            return;
        };
        let txn = editor_core::Transaction::from_changes(vec![editor_core::Change {
            at: start,
            removed,
            inserted: snip.text.clone(),
        }]);
        host.apply_transaction(id, txn);
        let sel = match snip.first_stop() {
            Some(t) => editor_core::Selection::new(start + t.range.0, start + t.range.1),
            None => editor_core::Selection::caret(start + snip.text.chars().count()),
        };
        host.set_selections(id, editor_core::Selections::single(sel));
    }

    /// Apply a completion's `additionalTextEdits` (auto-import edits) to the active document via
    /// the app's workspace-edit pipeline (which owns UTF-16↔char resolution + Transaction).
    fn apply_additional_edits(host: &mut dyn Host, edits: &[LspTextEdit]) {
        let path = host
            .active_doc()
            .and_then(|id| host.workspace().documents.get(id))
            .and_then(|d| d.path.clone());
        if let Some(path) = path {
            host.apply_workspace_edit(LspWorkspaceEdit {
                changes: vec![(path.to_string_lossy().into_owned(), edits.to_vec())],
            });
        }
    }

    /// Publish (or clear) the caret popup from the current state.
    fn publish(&self, host: &mut dyn Host) {
        match self.state.as_ref() {
            Some(state) if !state.is_empty() => {
                let rows = state
                    .filtered
                    .iter()
                    .map(|&i| {
                        let it = &state.items[i];
                        PopupRow::new(kind_label(it.kind), it.label.clone(), it.detail.clone())
                    })
                    .collect();
                host.set_popup(Some(Popup {
                    owner: Self::ID.to_string(),
                    anchor: state.anchor,
                    rows,
                    selected: state.selected,
                }));
            }
            _ => host.set_popup(None),
        }
    }

    fn dismiss(&mut self, host: &mut dyn Host) {
        self.state = None;
        host.set_popup(None);
    }
}

impl Plugin for CompletionPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("lsp.completion", "Edit: Trigger Suggest")
            .keybinding("ctrl+space", "lsp.completion")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if command_id == "lsp.completion" {
            if host.lsp_enabled() {
                host.lsp_request(LspRequestKind::Completion);
            }
            return true;
        }
        false
    }

    fn on_popup_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        if self.state.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Down => {
                if let Some(s) = self.state.as_mut() {
                    s.move_sel(1);
                }
                self.publish(host);
                true
            }
            KeyCode::Up => {
                if let Some(s) = self.state.as_mut() {
                    s.move_sel(-1);
                }
                self.publish(host);
                true
            }
            KeyCode::Esc => {
                self.dismiss(host);
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.accept(host);
                true
            }
            // Everything else falls through to normal editing; `refresh` re-syncs on DidChange.
            _ => false,
        }
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        match event {
            Event::LspCompletion {
                items,
                is_incomplete,
            } => self.open(items.clone(), *is_incomplete, host),
            Event::DidChange(id) if host.active_doc() == Some(*id) => {
                // Typing a trigger char (`.`/`::`) auto-requests member/path completions; while a
                // popup is open, an `isIncomplete` list is re-requested (server re-filters), an
                // exhaustive one is filtered locally.
                if host.lsp_enabled() && Self::at_trigger_char(host) {
                    host.lsp_request(LspRequestKind::Completion);
                } else if self.state.is_some() {
                    if self.is_incomplete && host.lsp_enabled() {
                        host.lsp_request(LspRequestKind::Completion);
                    } else {
                        self.refresh(host);
                    }
                }
            }
            Event::DidChangeCursor(id) if host.active_doc() == Some(*id) => {
                if self.state.is_some() {
                    self.refresh(host);
                }
            }
            Event::DidChangeActive(_) | Event::ExternalReload(_) if self.state.is_some() => {
                self.dismiss(host)
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_chars_are_dot_and_double_colon() {
        assert!(is_trigger('.', Some('j'))); // member access
        assert!(is_trigger(':', Some(':'))); // path
        assert!(!is_trigger(':', Some('a'))); // a lone colon is not a trigger
        assert!(!is_trigger('x', Some('.'))); // an identifier char is not
        assert!(is_trigger('.', None)); // a leading '.' still triggers
    }
}
