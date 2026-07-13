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

/// A short glyph/abbreviation for an LSP `CompletionItemKind` (plan §2.1 "show kind").
pub fn kind_label(kind: Option<u8>) -> &'static str {
    match kind {
        Some(2) | Some(3) => "ƒ", // Method / Function
        Some(4) => "ƒ",           // Constructor
        Some(5) => ".",           // Field
        Some(6) => "x",           // Variable
        Some(7) | Some(8) => "T", // Class / Interface
        Some(9) => "M",           // Module
        Some(10) => ".",          // Property
        Some(13) => "E",          // Enum
        Some(14) => "K",          // Keyword
        Some(15) => "S",          // Snippet
        Some(21) => "C",          // Constant
        Some(22) => "T",          // Struct
        _ => "•",
    }
}

/// A live completion session: the server's items, the subset matching what's been typed since the
/// trigger, and the current selection. Pure (only depends on the item type), so it unit-tests.
pub struct CompletionState {
    pub items: Vec<LspCompletionItem>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    /// Char offset where the replaced identifier prefix starts — the popup anchor.
    pub anchor: usize,
    /// The identifier text typed between `anchor` and the caret; drives filtering.
    pub prefix: String,
}

impl CompletionState {
    pub fn new(items: Vec<LspCompletionItem>, anchor: usize, prefix: String) -> CompletionState {
        let mut s = CompletionState {
            items,
            filtered: Vec::new(),
            selected: 0,
            anchor,
            prefix,
        };
        s.refilter();
        s
    }

    /// Recompute `filtered` from `prefix`: prefix matches rank before subsequence hits. Resets the
    /// selection.
    pub fn refilter(&mut self) {
        let prefix = self.prefix.to_lowercase();
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| score(&it.label, &prefix).map(|s| (s, i)))
            .collect();
        scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
    }

    pub fn selected_item(&self) -> Option<&LspCompletionItem> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    pub fn move_sel(&mut self, delta: isize) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let n = n as isize;
        self.selected = (((self.selected as isize + delta) % n + n) % n) as usize;
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }
}

/// Match score for `label` against a lowercase `prefix`: exact prefix beats a subsequence hit;
/// `None` means no match. An empty prefix matches everything (score 0).
fn score(label: &str, prefix: &str) -> Option<i32> {
    if prefix.is_empty() {
        return Some(0);
    }
    let label = label.to_lowercase();
    if label.starts_with(prefix) {
        Some(100)
    } else if is_subsequence(prefix, &label) {
        Some(50)
    } else {
        None
    }
}

/// True when every char of `needle` appears in `haystack` in order (a fuzzy contains).
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    for nc in needle.chars() {
        if !hay.any(|hc| hc == nc) {
            return false;
        }
    }
    true
}

#[derive(Default)]
pub struct CompletionPlugin {
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
            let insert = item.insert_text.clone();
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
        // Auto-imports: apply eager additionalTextEdits, or resolve to fetch them lazily.
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

    fn item(label: &str, kind: Option<u8>) -> LspCompletionItem {
        LspCompletionItem {
            label: label.to_string(),
            detail: None,
            insert_text: label.to_string(),
            kind,
            additional_edits: Vec::new(),
            is_snippet: false,
            data: None,
        }
    }

    fn items() -> Vec<LspCompletionItem> {
        vec![
            item("println", Some(3)),
            item("print", Some(3)),
            item("eprintln", Some(3)),
            item("format", Some(3)),
        ]
    }

    #[test]
    fn empty_prefix_matches_all_in_order() {
        let s = CompletionState::new(items(), 0, String::new());
        assert_eq!(s.filtered.len(), 4);
        assert_eq!(s.selected_item().unwrap().label, "println");
    }

    #[test]
    fn prefix_matches_rank_before_subsequence() {
        let s = CompletionState::new(items(), 0, "print".to_string());
        let labels: Vec<&str> = s
            .filtered
            .iter()
            .map(|&i| s.items[i].label.as_str())
            .collect();
        assert_eq!(&labels[..2], &["println", "print"]);
        assert!(labels.contains(&"eprintln"));
        assert!(!labels.contains(&"format"));
    }

    #[test]
    fn refilter_narrows_as_prefix_grows() {
        let mut s = CompletionState::new(items(), 0, "p".to_string());
        let before = s.filtered.len();
        s.prefix = "prin".to_string();
        s.refilter();
        assert!(s.filtered.len() <= before);
        assert!(s
            .filtered
            .iter()
            .all(|&i| s.items[i].label.contains("prin")));
    }

    #[test]
    fn selection_wraps() {
        let mut s = CompletionState::new(items(), 0, String::new());
        s.move_sel(-1);
        assert_eq!(s.selected, 3);
        s.move_sel(1);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn no_match_is_empty() {
        let s = CompletionState::new(items(), 0, "zzz".to_string());
        assert!(s.is_empty());
    }

    #[test]
    fn trigger_chars_are_dot_and_double_colon() {
        assert!(is_trigger('.', Some('j'))); // member access
        assert!(is_trigger(':', Some(':'))); // path
        assert!(!is_trigger(':', Some('a'))); // a lone colon is not a trigger
        assert!(!is_trigger('x', Some('.'))); // an identifier char is not
        assert!(is_trigger('.', None)); // a leading '.' still triggers
    }
}
