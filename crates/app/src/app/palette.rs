//! Fuzzy pickers (command palette / quick-open / goto-line) and their activation.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Open the command palette: built-in commands + plugin-contributed commands.
    pub(super) fn open_palette(&mut self) {
        let mut items: Vec<PickerItem> = crate::commands::palette_entries()
            .iter()
            .map(|(id, title)| PickerItem {
                id: id.to_string(),
                label: title.to_string(),
            })
            .collect();
        for spec in self.registry.commands() {
            items.push(PickerItem {
                id: spec.id.clone(),
                label: spec.title.clone(),
            });
        }
        self.editor.picker = Some(Picker::new(PickerKind::Command, "Command", items));
    }

    /// Open quick-open: fuzzy-filter files under the project root (ignore-walked).
    pub(super) fn open_quick_open(&mut self) {
        let root = self.editor.workspace.root.clone();
        let mut items = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.flatten().take(10_000) {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let path = entry.path();
                let label = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned();
                items.push(PickerItem {
                    id: path.to_string_lossy().into_owned(),
                    label,
                });
            }
        }
        self.editor.picker = Some(Picker::new(PickerKind::File, "Go to File", items));
    }

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
        match picker.kind {
            PickerKind::Command => {
                if let Some(item) = picker.selected_item() {
                    let id = item.id.clone();
                    self.exec_id(&id);
                }
            }
            PickerKind::File => {
                if let Some(item) = picker.selected_item() {
                    let path = std::path::PathBuf::from(&item.id);
                    self.open_path(&path);
                }
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
