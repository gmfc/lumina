//! Clipboard copy / cut / paste, implemented **as a plugin** (invariant #3).
//!
//! The system clipboard (the arboard daemon + an OSC 52 fallback + an in-process register) is
//! app-owned I/O, so the plugin doesn't hold it — it reaches it through [`Host::clipboard_read`] /
//! [`Host::clipboard_write`]. Copy takes the primary selection; cut copies then deletes; paste
//! inserts the clipboard text at every caret. Cut and paste edit multi-cursor-aware through
//! [`Host::apply_transaction`], building the transaction with
//! [`editor_core::edit::selection_edit_transaction`] (the pure builder) so a plugin with only the
//! Host surface reproduces exactly what `edit::delete_backward` / `edit::insert_text` do — and,
//! like them, records nothing when the edit is a no-op.

use editor_core::edit::selection_edit_transaction;
use editor_core::Motion;
use editor_plugin::{Contributions, Host, Plugin};

pub struct ClipboardPlugin;

impl ClipboardPlugin {
    const ID: &'static str = "clipboard";

    /// The primary selection's text, or `None` for an empty caret (nothing to copy/cut).
    fn primary_selection_text(host: &dyn Host) -> Option<String> {
        let id = host.active_doc()?;
        let doc = host.workspace().documents.get(id)?;
        let sel = doc.selections.primary();
        if sel.is_empty() {
            None
        } else {
            Some(doc.rope().slice(sel.from()..sel.to()).to_string())
        }
    }

    fn copy(&self, host: &mut dyn Host) {
        if let Some(text) = Self::primary_selection_text(host) {
            host.clipboard_write(text);
        }
    }

    fn cut(&self, host: &mut dyn Host) {
        let Some(text) = Self::primary_selection_text(host) else {
            return; // matches the app: cut is inert without a non-empty primary selection
        };
        host.clipboard_write(text);
        // Delete like `edit::delete_backward`: each non-empty selection, or the grapheme before an
        // empty caret.
        self.edit(host, |d, sel| {
            if sel.is_empty() {
                let from = editor_core::motion::resolve(d, sel.head, Motion::Left, 1);
                (from..sel.head, String::new())
            } else {
                (sel.span(), String::new())
            }
        });
    }

    fn paste(&self, host: &mut dyn Host) {
        let text = host.clipboard_read();
        // Replace every selection's span with the clipboard text (mirrors `edit::insert_text`): an
        // empty caret inserts, a non-empty selection is overwritten. Empty text over empty carets
        // builds an empty transaction and is skipped below, so it records no undo step.
        self.edit(host, |_d, sel| (sel.span(), text.clone()));
    }

    /// Apply a per-selection edit through the Host: build the transaction + resulting selections
    /// purely, then apply + reset the selection set — but only when the edit actually changes
    /// something, so a no-op never lands on the undo stack (as `edit_selections` guarantees).
    fn edit<F>(&self, host: &mut dyn Host, f: F)
    where
        F: FnMut(
            &editor_core::Document,
            editor_core::Selection,
        ) -> (std::ops::Range<usize>, String),
    {
        let Some(id) = host.active_doc() else {
            return;
        };
        let Some((txn, after)) = host
            .workspace()
            .documents
            .get(id)
            .map(|doc| selection_edit_transaction(doc, f))
        else {
            return;
        };
        if txn.is_empty() {
            return;
        }
        host.apply_transaction(id, txn);
        host.set_selections(id, after);
    }
}

impl Plugin for ClipboardPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("edit.copy", "Edit: Copy")
            .command("edit.cut", "Edit: Cut")
            .command("edit.paste", "Edit: Paste")
            .keybinding("ctrl+c", "edit.copy")
            .keybinding("ctrl+x", "edit.cut")
            .keybinding("ctrl+v", "edit.paste")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "edit.copy" => self.copy(host),
            "edit.cut" => self.cut(host),
            "edit.paste" => self.paste(host),
            _ => return false,
        }
        true
    }
}
