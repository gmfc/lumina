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
            // Quitting past unsaved work — the same three outcomes as ConfirmClose, so there is
            // nothing new to learn, and the box names every file at risk.
            crate::editor::Overlay::ConfirmQuit { .. } => match key.code {
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.editor.overlay = None;
                    self.save_all_and_quit();
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.editor.overlay = None;
                    self.quit = true;
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c') => {
                    self.editor.overlay = None;
                }
                _ => {}
            },
            // Reloading over a dirty buffer: the discard is not undoable, so it is confirmed.
            crate::editor::Overlay::ConfirmReload => match key.code {
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.editor.overlay = None;
                    self.reload_from_disk_now();
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('c') => {
                    self.editor.overlay = None;
                }
                _ => {}
            },
            crate::editor::Overlay::Info(_) => {
                // `Esc` dismisses. Any other key also dismisses, but then **falls through** to
                // normal handling: a hover box that swallowed the first keystroke of a chord left
                // the user pressing a shortcut that did nothing for reasons nothing on screen
                // explained.
                self.editor.overlay = None;
                if key.code != KeyCode::Esc {
                    self.on_key(key);
                }
            }
            crate::editor::Overlay::SaveAsInput {
                mut buffer,
                error,
                overwrite,
            } => {
                // While an overwrite is pending the box is a confirmation, not a text field:
                // only the explicit `[O]` goes through, so a stray keystroke can't destroy a file.
                if let Some(target) = overwrite {
                    match key.code {
                        KeyCode::Char('o') | KeyCode::Char('O') => {
                            self.editor.overlay = None;
                            let raw = target.to_string_lossy().into_owned();
                            self.save_as_to(&raw);
                        }
                        KeyCode::Esc => self.editor.overlay = None,
                        _ => {
                            // Back to editing the path.
                            self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
                                buffer,
                                error: None,
                                overwrite: None,
                            });
                        }
                    }
                    return;
                }
                match key.code {
                    KeyCode::Esc => self.editor.overlay = None,
                    KeyCode::Enter => self.submit_save_as(&buffer),
                    KeyCode::Backspace => {
                        buffer.pop();
                        self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
                            buffer,
                            error: None,
                            overwrite: None,
                        });
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        buffer.push(c);
                        self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
                            buffer,
                            error: None,
                            overwrite: None,
                        });
                    }
                    _ => {
                        // Keep whatever the box was already reporting.
                        self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
                            buffer,
                            error,
                            overwrite: None,
                        });
                    }
                }
            }
            crate::editor::Overlay::ContextMenu {
                x,
                y,
                items,
                mut selected,
            } => match key.code {
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
            },
        }
    }
}
