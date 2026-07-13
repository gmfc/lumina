//! The command dispatcher: turns a `Command` into edits and side effects.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// The single dispatcher — the only place editor state mutates (plan §5).
    pub fn dispatch(&mut self, cmd: Command) {
        self.editor.status_message = None;
        let page = self.page_height;

        match cmd {
            Command::Quit => self.quit = true,

            // --- motion / selection ---
            Command::Move(m) => self.with_doc(|d| edit::move_selections(d, m, page, false)),
            Command::Extend(m) => self.with_doc(|d| edit::move_selections(d, m, page, true)),
            Command::SelectAll => self.with_doc(|d| {
                let len = d.len_chars();
                d.selections = editor_core::Selections::single(Selection::new(0, len));
            }),
            Command::SelectWord => self.with_doc(edit::select_word),
            Command::SelectLine => self.with_doc(edit::select_line),

            // --- editing ---
            Command::InsertChar(c) => {
                let (pairs, indent) = (self.config.auto_pairs, self.config.auto_indent);
                let table = editor_core::PairTable::default();
                self.with_doc(|d| edit::insert_char_smart(d, c, &table, pairs, indent));
            }
            Command::InsertNewline => {
                let indent = self.config.auto_indent;
                let table = editor_core::PairTable::default();
                self.with_doc(|d| edit::insert_newline_smart(d, &table, indent));
            }
            Command::InsertText(s) => {
                self.with_doc(|d| edit::insert_text(d, &s, editor_core::GroupBreak::Force))
            }
            Command::DeleteBackward => {
                let pairs = self.config.auto_pairs;
                let table = editor_core::PairTable::default();
                self.with_doc(|d| edit::delete_backward_smart(d, &table, pairs));
            }
            Command::DeleteForward => self.with_doc(edit::delete_forward),
            Command::DeleteWordBackward => self.with_doc(edit::delete_word_backward),
            Command::DuplicateLine => self.with_doc(edit::duplicate_line),
            Command::CopyLineUp => self.with_doc(edit::copy_line_up),
            Command::DeleteLine => self.with_doc(edit::delete_lines),
            Command::InsertLineBelow => self.with_doc(edit::insert_line_below),
            Command::InsertLineAbove => self.with_doc(edit::insert_line_above),
            Command::MoveLineUp => self.with_doc(|d| edit::move_lines(d, -1)),
            Command::MoveLineDown => self.with_doc(|d| edit::move_lines(d, 1)),
            Command::ToggleComment => {
                let token = self
                    .editor
                    .active_document()
                    .and_then(|d| d.language.as_deref())
                    .map(line_comment_token)
                    .unwrap_or("//");
                self.with_doc(|d| edit::toggle_comment(d, token));
            }
            Command::Indent => self.with_doc(edit::indent),
            Command::Outdent => self.with_doc(edit::outdent),
            Command::TrimTrailingWhitespace => self.with_doc(|d| {
                edit::apply_save_hygiene(d, true, false);
            }),

            // multi-cursor is entirely the `multicursor` builtin plugin (all cursor.* ids
            // dispatch through the registry). clipboard copy/cut/paste is the `clipboard` builtin
            // plugin; a bracketed paste from the terminal inserts its payload via
            // `Command::InsertText` (see `on_paste`).

            // --- history ---
            Command::Undo => self.with_doc(|d| {
                edit::undo(d);
            }),
            Command::Redo => self.with_doc(|d| {
                edit::redo(d);
            }),

            // --- files / tabs ---
            Command::Save => self.save_or_save_as(),
            Command::SaveAs => self.open_save_as(),
            Command::SaveAll => self.save_all(),
            Command::NewFile => self.new_file(),
            Command::CloseTab => self.request_close(self.editor.workspace.active_tab),
            Command::CloseAllTabs => self.close_all_tabs(),
            Command::ReopenClosedTab => self.reopen_closed_tab(),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::GotoTab(i) => self.editor.workspace.focus_tab(i),

            // clipboard copy/cut/paste is the `clipboard` builtin plugin, dispatched through the
            // registry.

            // language server: the request-issuing commands are the `lsp` plugin and diagnostic
            // navigation is the `diagnostics` plugin — both dispatched through the registry.
            // git change navigation (git.nextHunk/git.prevHunk) is the `git-nav` builtin plugin,
            // dispatched through the registry.

            // --- ui ---
            Command::ToggleSidebar => self.editor.sidebar_visible = !self.editor.sidebar_visible,
            Command::FocusSidebar => self.editor.focus = Focus::Sidebar,
            Command::FocusEditor => self.editor.focus = Focus::Editor,
            // terminal-dock commands are the `terminal` builtin plugin, dispatched through the
            // registry; it owns the dock lifecycle and drives the PTYs through the RawPTY Host port.
            // Plugin-contributed command ids resolve registry-first in `exec_id`, never through
            // this table.
        }

        // Broadcast any events queued by the edit, and run queued commands/opens.
        self.drain_workers();
    }

    /// Run `f` on the active document if there is one, then notify plugins of what changed.
    ///
    /// This is the app-side edit chokepoint (typing, motions, the editing primitives). Like
    /// [`Host::apply_transaction`], it emits so reactive plugins (completion, diagnostics) re-sync:
    /// a bumped `revision` means the text changed ([`Event::DidChange`]); otherwise a moved
    /// selection means a pure caret move ([`Event::DidChangeCursor`]). Emitted events drain with
    /// the rest at the end of `dispatch`.
    pub(super) fn with_doc<F: FnOnce(&mut Document)>(&mut self, f: F) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(d) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        let (rev_before, sel_before) = (d.revision, d.selections.clone());
        f(d);
        let text_changed = d.revision != rev_before;
        let cursor_moved = d.selections != sel_before;
        if text_changed {
            self.editor.emit(editor_plugin::event::Event::DidChange(id));
        } else if cursor_moved {
            self.editor
                .emit(editor_plugin::event::Event::DidChangeCursor(id));
        }
    }
}

/// The line-comment token for a language id (used by `edit.toggleComment`).
fn line_comment_token(lang: &str) -> &'static str {
    match lang {
        "python" | "toml" | "yaml" | "shell" | "ruby" => "#",
        _ => "//",
    }
}
