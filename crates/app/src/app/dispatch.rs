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

            // --- multi-cursor ---
            // add-next-match / select-all / add-above-below are contributed by the `multicursor`
            // builtin plugin (crates/builtins) and dispatched through the registry.
            Command::CursorsToLineEnds => self.with_doc(edit::cursors_to_line_ends),
            Command::Paste(s) => {
                let text = if s.is_empty() {
                    self.clipboard.get()
                } else {
                    s
                };
                self.with_doc(|d| edit::insert_text(d, &text, editor_core::GroupBreak::Force))
            }

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
            Command::OpenFile(p) => self.open_path(&p),
            Command::CloseTab => self.request_close(self.editor.workspace.active_tab),
            Command::CloseAllTabs => self.close_all_tabs(),
            Command::ReopenClosedTab => self.reopen_closed_tab(),
            Command::NextTab => self.cycle_tab(1),
            Command::PrevTab => self.cycle_tab(-1),
            Command::GotoTab(i) => self.editor.workspace.focus_tab(i),

            // --- search (find/replace is the `find` plugin; only project search is app-side) ---
            Command::ProjectSearch => self.open_search(),

            // --- clipboard ---
            Command::Copy => {
                if let Some(t) = self.selection_text() {
                    self.clipboard.set(t);
                }
            }
            Command::Cut => {
                if let Some(t) = self.selection_text() {
                    self.clipboard.set(t);
                    self.with_doc(edit::delete_backward);
                }
            }

            // --- language server ---
            Command::Hover => self.lsp_request(LspRequest::Hover),
            Command::GotoDefinition => self.lsp_request(LspRequest::Definition),
            Command::GotoImplementation => self.lsp_request(LspRequest::Implementation),
            Command::GotoTypeDefinition => self.lsp_request(LspRequest::TypeDefinition),
            Command::Completion => self.lsp_request(LspRequest::Completion),
            Command::RenameSymbol => self.open_rename(),
            Command::NextDiagnostic => self.goto_diagnostic(1),
            Command::PrevDiagnostic => self.goto_diagnostic(-1),
            Command::FindReferences => self.lsp_request(LspRequest::References),
            Command::DocumentSymbols => self.request_document_symbols(),
            // git change navigation (git.nextHunk/git.prevHunk) is the `git-nav` builtin plugin,
            // dispatched through the registry.

            // --- ui ---
            Command::ToggleSidebar => self.editor.sidebar_visible = !self.editor.sidebar_visible,
            Command::FocusSidebar => self.editor.focus = Focus::Sidebar,
            Command::FocusEditor => self.editor.focus = Focus::Editor,

            // --- terminal panel ---
            Command::ToggleTerminal => self.toggle_terminal(),
            Command::NewTerminal => self.new_terminal(),
            Command::CloseTerminal => self.close_terminal(),
            Command::MinimizeTerminal => self.minimize_terminal(),
            Command::NextTerminal => {
                if self.panel.open {
                    self.panel.next();
                }
            }
            Command::PrevTerminal => {
                if self.panel.open {
                    self.panel.prev();
                }
            }

            // A plugin-contributed command referenced by id.
            Command::Run(id) => {
                if !self.registry.dispatch_command(&id, &mut self.editor) {
                    self.editor.status_message = Some(format!("Unknown command: {id}"));
                }
            }
        }

        // Broadcast any events queued by the edit, and run queued commands/opens.
        self.drain_workers();
    }

    /// Run `f` on the active document if there is one.
    pub(super) fn with_doc<F: FnOnce(&mut Document)>(&mut self, f: F) {
        if let Some(d) = self.editor.active_document_mut() {
            f(d);
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
