//! The right-click context menu: built from plugin-contributed menu items, filtered by their
//! `when` predicate against live editor state, grouped, and opened as an [`Overlay`]. Activation
//! routes each item's command through `exec_id`. Part of the [`crate::app`] module.

use super::*;
use crate::editor::{ContextMenuItem, Overlay};
use editor_plugin::{MenuGroup, MenuItemSpec, MenuWhen};

impl App {
    /// Build + open the context menu anchored at screen `(x, y)`: keep the contributed menu items
    /// whose `when` predicate holds, order them by group (a divider precedes each new group), and
    /// show the overlay. A no-op when nothing is applicable (so an empty menu never appears).
    pub(super) fn open_context_menu(&mut self, x: u16, y: u16) {
        let mut applicable: Vec<&MenuItemSpec> = self
            .registry
            .menu_items()
            .iter()
            .filter(|m| self.menu_when_holds(m.when))
            .collect();
        if applicable.is_empty() {
            return;
        }
        // Stable sort by group keeps each group's contribution order intact.
        applicable.sort_by_key(|m| m.group);
        let mut items = Vec::with_capacity(applicable.len());
        let mut last_group: Option<MenuGroup> = None;
        for m in applicable {
            let first_in_group = last_group.is_some_and(|g| g != m.group);
            last_group = Some(m.group);
            items.push(ContextMenuItem {
                label: m.label.clone(),
                command: m.command.clone(),
                first_in_group,
            });
        }
        self.editor.overlay = Some(Overlay::ContextMenu {
            x,
            y,
            items,
            selected: 0,
        });
    }

    /// Evaluate a menu item's `when` predicate against the current editor state.
    fn menu_when_holds(&self, when: MenuWhen) -> bool {
        match when {
            MenuWhen::Always => true,
            MenuWhen::HasSelection => self.active_has_selection(),
            MenuWhen::LspEnabled => self.active_server_ready(),
            MenuWhen::LspOnWord => self.active_server_ready() && self.cursor_on_word(),
        }
    }

    /// Whether a language server is running for the active document's language.
    fn active_server_ready(&self) -> bool {
        self.editor
            .active_document()
            .and_then(|d| d.language.as_deref())
            .is_some_and(|lang| self.lsp.is_ready(lang))
    }

    /// Whether the active document has a non-empty primary selection.
    fn active_has_selection(&self) -> bool {
        self.editor
            .active_document()
            .is_some_and(|d| !d.selections.primary().is_empty())
    }

    /// Whether the primary caret sits on (or just after) a word character — i.e. on a symbol.
    /// Mirrors the `document_highlight` plugin's `cursor_on_word` recipe (it lives behind the
    /// `Host` port there; here we read `EditorState` directly, so the small logic is repeated).
    fn cursor_on_word(&self) -> bool {
        let Some(doc) = self.editor.active_document() else {
            return false;
        };
        let head = doc.selections.primary().head;
        let rope = doc.rope();
        let at = head < rope.len_chars() && is_word(rope.char(head));
        let before = head > 0 && is_word(rope.char(head - 1));
        at || before
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
