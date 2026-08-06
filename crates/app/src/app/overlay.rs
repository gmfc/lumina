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
            "help.keybindings" => self.open_keybindings_help(),
            "help.commands" => self.exec_id("view.commandPalette"),
            "view.notifications" => self.open_notification_log(),
            "view.dismissNotice" => self.editor.dismiss_status(),
            other => {
                let palette = self.chord_for("view.commandPalette", "Ctrl+Shift+P");
                self.editor.notify_error(format!(
                    "There is no command “{other}” — press {palette} to browse the commands there are"
                ));
            }
        }
    }

    /// Enter in the Save As box: vet the typed path, then write, ask, or explain.
    ///
    /// Save As used to assign the path and write immediately, so typing the name of an existing
    /// file replaced it with no prompt and a typo'd directory surfaced only as `"Save failed"`
    /// after the fact. Both are answered in the box the user is already looking at.
    fn submit_save_as(&mut self, raw: &str) {
        let Some(path) = self.resolve_save_as(raw) else {
            self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
                buffer: raw.to_string(),
                error: Some("Type a file name".to_string()),
                overwrite: None,
            });
            return;
        };
        if let Some(problem) = self.save_as_problem(&path) {
            self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
                buffer: raw.to_string(),
                error: Some(problem),
                overwrite: None,
            });
            return;
        }
        // Re-saving the buffer's own file isn't an overwrite worth confirming — that is Save.
        let is_own_file = self
            .editor
            .active_document()
            .and_then(|d| d.path.clone())
            .is_some_and(|p| p == path);
        if path.exists() && !is_own_file {
            self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
                buffer: raw.to_string(),
                error: None,
                overwrite: Some(path),
            });
            return;
        }
        self.editor.overlay = None;
        self.save_as_to(raw);
    }

    /// Route a key to the handler for whichever overlay is open. One arm per overlay, each in
    /// its own function: the dispatch stays readable as the set of overlays grows.
    pub(super) fn overlay_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::editor::Overlay;
        let Some(overlay) = self.editor.overlay.clone() else {
            return;
        };
        match overlay {
            Overlay::ConfirmClose { tab } => self.confirm_close_key(key, tab),
            Overlay::ConfirmQuit { .. } => self.confirm_quit_key(key),
            Overlay::ConfirmReload => self.confirm_reload_key(key),
            Overlay::Info(_) => self.info_key(key),
            Overlay::SaveAsInput {
                buffer,
                error,
                overwrite,
            } => self.save_as_key(key, buffer, error, overwrite),
            Overlay::ContextMenu {
                x,
                y,
                items,
                selected,
            } => self.context_menu_key(key, x, y, items, selected),
        }
    }

    /// Closing a dirty tab: save & close / discard / cancel.
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
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c') => self.editor.overlay = None,
            _ => {}
        }
    }

    /// Quitting past unsaved work — the same three outcomes as [`Self::confirm_close_key`], so
    /// there is nothing new to learn, and the box names every file at risk.
    fn confirm_quit_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.editor.overlay = None;
                self.save_all_and_quit();
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.editor.overlay = None;
                self.quit = true;
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c') => self.editor.overlay = None,
            _ => {}
        }
    }

    /// Reloading over a dirty buffer: the discard is not undoable, so it is confirmed.
    fn confirm_reload_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.editor.overlay = None;
                self.reload_from_disk_now();
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c') => self.editor.overlay = None,
            _ => {}
        }
    }

    /// The hover box. `Esc` dismisses; any other key also dismisses but then **falls through** to
    /// normal handling — a box that swallowed the first keystroke of a chord left the user
    /// pressing a shortcut that did nothing, for reasons nothing on screen explained.
    fn info_key(&mut self, key: crossterm::event::KeyEvent) {
        self.editor.overlay = None;
        if key.code != crossterm::event::KeyCode::Esc {
            self.on_key(key);
        }
    }

    /// The Save As box: a path field, or — once a target has been resolved onto an existing file
    /// — an overwrite confirmation.
    fn save_as_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        buffer: String,
        error: Option<String>,
        overwrite: Option<PathBuf>,
    ) {
        use crossterm::event::KeyCode;
        // While an overwrite is pending the box is a confirmation, not a text field: only the
        // explicit `[O]` goes through, so a stray keystroke can't destroy a file.
        if let Some(target) = overwrite {
            match key.code {
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    self.editor.overlay = None;
                    self.save_as_to(&target.to_string_lossy());
                }
                KeyCode::Esc => self.editor.overlay = None,
                // Anything else backs out of the confirmation to the path field.
                _ => self.reopen_save_as(buffer, None),
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.editor.overlay = None,
            KeyCode::Enter => self.submit_save_as(&buffer),
            KeyCode::Backspace => {
                let mut buffer = buffer;
                buffer.pop();
                self.reopen_save_as(buffer, None);
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                let mut buffer = buffer;
                buffer.push(c);
                self.reopen_save_as(buffer, None);
            }
            // An un-owned key changes nothing, so the box keeps whatever it was reporting.
            _ => self.reopen_save_as(buffer, error),
        }
    }

    /// Re-open the Save As box on `buffer`, with no overwrite pending. Editing the path always
    /// clears a stale overwrite target: it no longer describes what is typed.
    fn reopen_save_as(&mut self, buffer: String, error: Option<String>) {
        self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
            buffer,
            error,
            overwrite: None,
        });
    }

    /// The right-click menu: move the selection, run the highlighted item, or dismiss.
    fn context_menu_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        x: u16,
        y: u16,
        items: Vec<crate::editor::ContextMenuItem>,
        selected: usize,
    ) {
        use crossterm::event::KeyCode;
        let mut selected = selected;
        match key.code {
            KeyCode::Up => {
                selected = selected.checked_sub(1).unwrap_or(items.len() - 1);
                self.editor.overlay = Some(crate::editor::Overlay::ContextMenu {
                    x,
                    y,
                    items,
                    selected,
                });
            }
            KeyCode::Down => {
                selected = (selected + 1) % items.len();
                self.editor.overlay = Some(crate::editor::Overlay::ContextMenu {
                    x,
                    y,
                    items,
                    selected,
                });
            }
            KeyCode::Enter => {
                // Close the menu before running the command — the command may open its own
                // overlay (e.g. rename's prompt), which must not be clobbered.
                self.editor.overlay = None;
                if let Some(item) = items.get(selected) {
                    self.exec_id(&item.command);
                }
            }
            KeyCode::Esc => self.editor.overlay = None,
            _ => {}
        }
    }
}
