//! Modal-overlay key handling (rename / save-as / confirm-close) and command-id execution.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Execute a command id. Resolution order puts the **plugin system first** (invariant #4:
    /// every action flows through one path, and a plugin can own or override an id): first the
    /// registry (built-in feature plugins like explorer / multi-cursor / git-nav, plus external
    /// plugins), then the app's built-in editing primitives (motions, edits, files, tabs, search,
    /// lsp — the `Command` table), then the handful of app-level actions that are neither.
    pub(super) fn exec_id(&mut self, id: &str) {
        if self.registry.dispatch_command(id, &mut self.editor) {
            self.drain_workers();
            return;
        }
        if let Some(cmd) = crate::commands::command_for_id(id) {
            self.dispatch(cmd);
            return;
        }
        match id {
            "config.reload" => self.reload_config(),
            // vim.enable/disable/toggle are the `vim` plugin's commands (resolved registry-first).
            "view.settings" => self.open_settings(),
            "lsp.panel.toggle" => self.toggle_lsp_panel(),
            other => {
                self.editor.status_message = Some(format!("Unknown command: {other}"));
            }
        }
    }

    /// Handle a key while the confirm-close overlay is open.
    pub(super) fn overlay_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(overlay) = self.editor.overlay.clone() else {
            return;
        };
        match overlay {
            crate::editor::Overlay::ConfirmClose { tab } => match key.code {
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
            },
            crate::editor::Overlay::Info(_) => {
                // Any key dismisses an info popup.
                self.editor.overlay = None;
            }
            crate::editor::Overlay::SaveAsInput { mut buffer } => match key.code {
                KeyCode::Esc => self.editor.overlay = None,
                KeyCode::Enter => {
                    self.editor.overlay = None;
                    self.save_as_to(&buffer);
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput { buffer });
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    buffer.push(c);
                    self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput { buffer });
                }
                _ => {}
            },
        }
    }
}
