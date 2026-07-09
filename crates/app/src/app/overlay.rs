//! Modal-overlay key handling (rename / save-as / confirm-close) and command-id execution.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Execute a command id: built-in editor command, app-level action, or plugin command.
    pub(super) fn exec_id(&mut self, id: &str) {
        if let Some(cmd) = crate::commands::command_for_id(id) {
            self.dispatch(cmd);
            return;
        }
        match id {
            "config.reload" => self.reload_config(),
            "view.toggleTheme" => self.toggle_theme(),
            other => {
                if !self.registry.dispatch_command(other, &mut self.editor) {
                    self.editor.status_message = Some(format!("Unknown command: {other}"));
                } else {
                    self.drain_workers();
                }
            }
        }
    }

    /// Handle a key while a modal overlay is open, dispatching to the per-overlay handler.
    pub(super) fn overlay_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some(overlay) = self.editor.overlay.clone() else {
            return;
        };
        match overlay {
            crate::editor::Overlay::ConfirmClose { tab } => self.confirm_close_key(key, tab),
            crate::editor::Overlay::Info(_) => {
                // Any key dismisses an info popup.
                self.editor.overlay = None;
            }
            crate::editor::Overlay::RenameInput {
                path,
                language,
                line,
                character,
                buffer,
            } => self.rename_input_key(key, path, language, line, character, buffer),
            crate::editor::Overlay::SaveAsInput { buffer } => self.save_as_input_key(key, buffer),
        }
    }

    /// Keys for the confirm-close prompt: (s)ave & close, (d)iscard & close, or cancel.
    fn confirm_close_key(&mut self, key: crossterm::event::KeyEvent, tab: usize) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.editor.workspace.focus_tab(tab);
                self.save_active();
                self.remember_closed(tab);
                self.close_and_forget(tab);
                self.editor.overlay = None;
            }
            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('y') => {
                self.remember_closed(tab);
                self.close_and_forget(tab);
                self.editor.overlay = None;
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c') => {
                self.editor.overlay = None;
            }
            _ => {}
        }
    }

    /// Keys for the rename-symbol text input.
    fn rename_input_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        path: std::path::PathBuf,
        language: String,
        line: u32,
        character: u32,
        mut buffer: String,
    ) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => self.editor.overlay = None,
            KeyCode::Enter => {
                self.editor.overlay = None;
                if !buffer.is_empty() {
                    self.lsp
                        .request_rename(&path, &language, line, character, &buffer);
                }
            }
            KeyCode::Backspace => {
                buffer.pop();
                self.set_rename_overlay(path, language, line, character, buffer);
            }
            KeyCode::Char(c) if !is_ctrl(&key) => {
                buffer.push(c);
                self.set_rename_overlay(path, language, line, character, buffer);
            }
            _ => {}
        }
    }

    /// Rebuild the rename overlay after a buffer edit.
    fn set_rename_overlay(
        &mut self,
        path: std::path::PathBuf,
        language: String,
        line: u32,
        character: u32,
        buffer: String,
    ) {
        self.editor.overlay = Some(crate::editor::Overlay::RenameInput {
            path,
            language,
            line,
            character,
            buffer,
        });
    }

    /// Keys for the save-as path input.
    fn save_as_input_key(&mut self, key: crossterm::event::KeyEvent, mut buffer: String) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => self.editor.overlay = None,
            KeyCode::Enter => {
                self.editor.overlay = None;
                self.save_as_to(&buffer);
            }
            KeyCode::Backspace => {
                buffer.pop();
                self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput { buffer });
            }
            KeyCode::Char(c) if !is_ctrl(&key) => {
                buffer.push(c);
                self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput { buffer });
            }
            _ => {}
        }
    }

    /// Open the rename prompt, prefilled with the identifier under the cursor.
    pub(super) fn open_rename(&mut self) {
        let Some((path, language, line, character)) = self.lsp_position() else {
            return;
        };
        let buffer = self
            .editor
            .active_document()
            .map(|d| {
                let head = d.selections.primary().head;
                let (s, e) = motion::word_at(d, head);
                d.text.slice(s..e).to_string()
            })
            .unwrap_or_default();
        self.editor.overlay = Some(crate::editor::Overlay::RenameInput {
            path,
            language,
            line,
            character,
            buffer,
        });
    }
}

/// Whether a key event carries the Ctrl modifier (so text inputs ignore Ctrl-chords).
fn is_ctrl(key: &crossterm::event::KeyEvent) -> bool {
    key.modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL)
}
