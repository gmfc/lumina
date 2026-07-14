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

pub(crate) struct ClipboardPlugin;

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
        // One row per command — `(id, title, chord)` — folded into the builder. The chords travel
        // with the plugin (invariant #3); the keymap folds in registry-contributed bindings, so the
        // `ctrl+c`/`ctrl+x`/`ctrl+v` rows left `commands/tables.rs`.
        const ROWS: [(&str, &str, &str); 3] = [
            ("edit.copy", "Edit: Copy", "ctrl+c"),
            ("edit.cut", "Edit: Cut", "ctrl+x"),
            ("edit.paste", "Edit: Paste", "ctrl+v"),
        ];
        let mut b = Contributions::builder();
        for (id, title, chord) in ROWS {
            b = b.command(id, title).keybinding(chord, id);
        }
        b.build()
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

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{DocId, Document, Selection, Selections, Transaction, Workspace};
    use editor_plugin::PanelContent;
    use std::path::{Path, PathBuf};

    /// A minimal in-memory [`Host`]: one document + a clipboard register, enough to drive the
    /// plugin's copy/cut/paste edits and read back the result. Everything else is a no-op default.
    struct MockHost {
        ws: Workspace,
        clip: String,
    }

    impl MockHost {
        fn new(text: &str) -> MockHost {
            let mut ws = Workspace::new(PathBuf::from("."));
            ws.open_document(Document::from_str(text));
            MockHost {
                ws,
                clip: String::new(),
            }
        }
        fn id(&self) -> DocId {
            self.ws.active_doc().unwrap()
        }
        fn select(&mut self, from: usize, to: usize) {
            let id = self.id();
            self.ws
                .documents
                .get_mut(id)
                .unwrap()
                .set_selections(Selections::single(Selection::new(from, to)));
        }
        fn caret(&mut self, at: usize) {
            let id = self.id();
            self.ws.documents.get_mut(id).unwrap().set_caret(at);
        }
        fn text(&self) -> String {
            self.ws.active_document().unwrap().to_string()
        }
    }

    impl Host for MockHost {
        fn workspace(&self) -> &Workspace {
            &self.ws
        }
        fn apply_transaction(&mut self, doc: DocId, txn: Transaction) {
            if let Some(d) = self.ws.documents.get_mut(doc) {
                txn.apply(d);
            }
        }
        fn set_selections(&mut self, doc: DocId, sels: Selections) {
            if let Some(d) = self.ws.documents.get_mut(doc) {
                d.set_selections(sels);
            }
        }
        fn open_path(&mut self, _path: &Path) {}
        fn set_panel(&mut self, _panel_id: &str, _content: PanelContent) {}
        fn set_status(&mut self, _item_id: &str, _text: String) {}
        fn notify(&mut self, _message: String) {}
        fn clipboard_read(&mut self) -> String {
            self.clip.clone()
        }
        fn clipboard_write(&mut self, text: String) {
            self.clip = text;
        }
        fn execute(&mut self, _command_id: &str) {}
    }

    fn run(host: &mut MockHost, command: &str) {
        assert!(ClipboardPlugin.run_command(command, host));
    }

    #[test]
    fn copy_writes_the_primary_selection() {
        let mut host = MockHost::new("hello world");
        host.select(0, 5);
        run(&mut host, "edit.copy");
        assert_eq!(host.clip, "hello");
        assert_eq!(host.text(), "hello world"); // copy never edits
    }

    #[test]
    fn copy_on_empty_caret_is_a_noop() {
        let mut host = MockHost::new("hello");
        host.clip = "keep".into();
        host.caret(2);
        run(&mut host, "edit.copy");
        assert_eq!(host.clip, "keep", "an empty caret has nothing to copy");
    }

    #[test]
    fn cut_writes_then_deletes_the_selection() {
        let mut host = MockHost::new("hello world");
        host.select(0, 6); // "hello "
        run(&mut host, "edit.cut");
        assert_eq!(host.clip, "hello ");
        assert_eq!(host.text(), "world");
    }

    #[test]
    fn cut_on_empty_caret_is_inert() {
        let mut host = MockHost::new("abc");
        host.caret(3);
        run(&mut host, "edit.cut");
        assert_eq!(host.clip, "");
        assert_eq!(host.text(), "abc", "cut needs a non-empty selection");
    }

    #[test]
    fn paste_inserts_at_the_caret() {
        let mut host = MockHost::new("ab");
        host.clip = "X".into();
        host.caret(1);
        run(&mut host, "edit.paste");
        assert_eq!(host.text(), "aXb");
    }

    #[test]
    fn paste_replaces_a_selection() {
        let mut host = MockHost::new("hello");
        host.clip = "Z".into();
        host.select(0, 5);
        run(&mut host, "edit.paste");
        assert_eq!(host.text(), "Z");
    }

    #[test]
    fn paste_empty_clipboard_over_caret_is_a_noop() {
        let mut host = MockHost::new("ab");
        host.caret(1);
        run(&mut host, "edit.paste");
        assert_eq!(
            host.text(),
            "ab",
            "pasting nothing at a caret records no edit"
        );
    }

    #[test]
    fn unknown_command_is_not_handled() {
        let mut host = MockHost::new("x");
        assert!(!ClipboardPlugin.run_command("edit.somethingElse", &mut host));
    }
}
