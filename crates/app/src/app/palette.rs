//! Fuzzy pickers (command palette / quick-open / goto-line) and their activation.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Open the Go-to-Line prompt (still an app command; the palette + quick-open are the
    /// `palette` plugin now, opening the same generic picker via `Host::open_picker`).
    pub(super) fn open_goto_line(&mut self) {
        self.editor.picker = Some(Picker::new(PickerKind::GotoLine, "Go to Line", Vec::new()));
    }

    pub(super) fn picker_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(picker) = &mut self.editor.picker else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.editor.picker = None,
            KeyCode::Up => picker.move_selection(-1),
            KeyCode::Down => picker.move_selection(1),
            KeyCode::Backspace => picker.backspace(),
            KeyCode::Enter => self.activate_picker(),
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(crossterm::event::KeyModifiers::CONTROL) =>
            {
                picker.input_char(c)
            }
            _ => {}
        }
    }

    pub(super) fn activate_picker(&mut self) {
        let Some(picker) = self.editor.picker.take() else {
            return;
        };
        // A plugin-owned picker (quick-open / command palette): route the chosen row back to the
        // owning plugin, which decides command-vs-file.
        if picker.owner.is_some() {
            if let Some(id) = picker.selected_item().map(|i| i.id.clone()) {
                let owner = picker.owner.clone().unwrap();
                let token = picker.token.clone().unwrap_or_default();
                self.registry
                    .activate_picker(&owner, &token, &id, &mut self.editor);
                self.drain_workers();
            }
            return;
        }
        // App-owned pickers (goto-line prompt, LSP locations list).
        match picker.kind {
            PickerKind::File => {
                // File pickers are plugin-owned now; nothing to do for an app-owned one.
            }
            PickerKind::GotoLine => {
                if let Ok(line) = picker.query.trim().parse::<usize>() {
                    self.goto_line(line.saturating_sub(1));
                }
            }
            PickerKind::Locations => {
                if let Some(loc) = picker
                    .selected_item()
                    .and_then(|item| item.id.parse::<usize>().ok())
                    .and_then(|i| self.editor.nav_locations.get(i).cloned())
                {
                    self.goto_location(loc);
                }
            }
        }
    }

    pub(super) fn goto_line(&mut self, line: usize) {
        self.with_doc(|d| {
            let l = line.min(d.len_lines().saturating_sub(1));
            let off = d.line_to_char(l);
            d.set_caret(off);
        });
    }

    // --- project search (Ctrl+Shift+F) -----------------------------------------
}
