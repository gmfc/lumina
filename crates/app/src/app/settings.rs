//! The Settings tab controller: open/focus the tab, route its keys and clicks to the
//! form widgets, and — on any change — mutate the config, persist it to the config
//! file, live-apply what it can, and rebuild the view (see [`crate::settings`]).
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;
use crossterm::event::KeyEvent;

use crate::settings::{Entry, SettingsView, Widget};

/// A value produced by a widget interaction, applied to the config by [`App::apply_setting`].
enum SettingValue {
    Bool(bool),
    Int(i64),
    Text(String),
    /// A dropdown choice — the chosen option string.
    Select(String),
}

impl App {
    // --- open / lifecycle ------------------------------------------------

    /// Open the Settings tab (the `view.settings` command), or focus it if already open.
    pub(super) fn open_settings(&mut self) {
        if let Some(id) = self.settings_doc {
            if self.editor.workspace.documents.contains_key(id) {
                self.editor.workspace.focus_doc(id);
                self.editor.focus = Focus::Editor;
                if self.settings.is_none() {
                    self.settings = Some(self.build_settings_view());
                }
                return;
            }
        }
        // Back the tab with an empty buffer so it lives in the normal tab machinery.
        let id = self.editor.workspace.open_document(Document::from_str(""));
        self.settings_doc = Some(id);
        self.settings = Some(self.build_settings_view());
        self.editor.focus = Focus::Editor;
    }

    /// True when the Settings tab is the active tab (so it owns rendering + input).
    pub(crate) fn settings_active(&self) -> bool {
        self.settings.is_some()
            && self.settings_doc.is_some()
            && self.editor.workspace.active_doc() == self.settings_doc
    }

    /// True when `id` backs the Settings tab (so the tab bar can name it).
    pub(crate) fn is_settings_doc(&self, id: editor_core::DocId) -> bool {
        self.settings_doc == Some(id)
    }

    /// Drop the settings state if its backing tab was closed (called from the input path).
    pub(super) fn reconcile_settings(&mut self) {
        if let Some(id) = self.settings_doc {
            if !self.editor.workspace.documents.contains_key(id) {
                self.settings_doc = None;
                self.settings = None;
            }
        }
    }

    fn build_settings_view(&self) -> SettingsView {
        SettingsView::build(&self.config, &self.plugin_list())
    }

    /// Every plugin, enabled (loaded) plus disabled (from config), sorted by id.
    fn plugin_list(&self) -> Vec<(String, bool)> {
        let mut list: Vec<(String, bool)> = self
            .registry
            .plugin_ids()
            .map(|id| (id.to_string(), true))
            .collect();
        for id in &self.config.disabled_plugins {
            if !list.iter().any(|(x, _)| x == id) {
                list.push((id.clone(), false));
            }
        }
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }

    fn settings_view_mut(&mut self) -> &mut SettingsView {
        self.settings.as_mut().expect("settings tab open")
    }

    // --- key handling ----------------------------------------------------

    /// Route a key to the settings form. Returns `true` when consumed; `false` lets it
    /// fall through to the global keymap (so Ctrl+W, Ctrl+P, … still work).
    pub(super) fn handle_settings_key(&mut self, key: KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false; // leave global chords to the keymap
        }

        // Editing a text/number field.
        if self.settings.as_ref().is_some_and(|v| v.editing.is_some()) {
            match key.code {
                KeyCode::Enter => self.settings_commit_edit(),
                KeyCode::Esc => self.settings_view_mut().editing = None,
                KeyCode::Backspace => {
                    if let Some(b) = self.settings_view_mut().editing.as_mut() {
                        b.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(b) = self.settings_view_mut().editing.as_mut() {
                        b.push(c);
                    }
                }
                _ => {}
            }
            return true;
        }

        // A dropdown is open.
        if self.settings.as_ref().is_some_and(|v| v.dropdown.is_some()) {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.settings_dropdown_move(-1),
                KeyCode::Down | KeyCode::Char('j') => self.settings_dropdown_move(1),
                KeyCode::Enter | KeyCode::Char(' ') => self.settings_dropdown_commit(),
                KeyCode::Esc => self.settings_view_mut().dropdown = None,
                _ => {}
            }
            return true;
        }

        // Navigation + activation.
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.settings_view_mut().move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.settings_view_mut().move_selection(1),
            KeyCode::PageUp => self.settings_view_mut().move_selection(-8),
            KeyCode::PageDown => self.settings_view_mut().move_selection(8),
            KeyCode::Char(' ') | KeyCode::Enter => self.settings_activate(),
            KeyCode::Left | KeyCode::Char('h') => self.settings_adjust(-1),
            KeyCode::Right | KeyCode::Char('l') => self.settings_adjust(1),
            KeyCode::Esc => {} // consumed, no-op (close with Ctrl+W)
            _ => return false,
        }
        true
    }

    /// Space/Enter on the selected item: toggle, open a dropdown, or start editing.
    fn settings_activate(&mut self) {
        let Some(item) = self
            .settings
            .as_ref()
            .and_then(|v| v.selected_item().cloned())
        else {
            return;
        };
        match item.widget {
            Widget::Toggle(v) => self.apply_setting(&item.key, SettingValue::Bool(!v)),
            Widget::Select { selected, .. } => self.settings_view_mut().dropdown = Some(selected),
            Widget::Number { value, .. } => self.settings_start_edit(value.to_string()),
            Widget::Text(s) => self.settings_start_edit(s),
        }
    }

    /// Left/Right on the selected item: step a number, cycle a dropdown, or flip a toggle.
    fn settings_adjust(&mut self, delta: i64) {
        let Some(item) = self
            .settings
            .as_ref()
            .and_then(|v| v.selected_item().cloned())
        else {
            return;
        };
        match item.widget {
            Widget::Number { value, min, max } => {
                let nv = (value + delta).clamp(min, max);
                self.apply_setting(&item.key, SettingValue::Int(nv));
            }
            Widget::Select { options, selected } => {
                if options.is_empty() {
                    return;
                }
                let n = options.len() as i64;
                let ni = (selected as i64 + delta).rem_euclid(n) as usize;
                self.apply_setting(&item.key, SettingValue::Select(options[ni].clone()));
            }
            Widget::Toggle(v) => self.apply_setting(&item.key, SettingValue::Bool(!v)),
            Widget::Text(_) => {}
        }
    }

    fn settings_start_edit(&mut self, buffer: String) {
        self.settings_view_mut().editing = Some(buffer);
    }

    fn settings_commit_edit(&mut self) {
        let buf = self.settings.as_ref().and_then(|v| v.editing.clone());
        let item = self
            .settings
            .as_ref()
            .and_then(|v| v.selected_item().cloned());
        self.settings_view_mut().editing = None;
        let (Some(buf), Some(item)) = (buf, item) else {
            return;
        };
        match item.widget {
            Widget::Number { min, max, .. } => {
                if let Ok(n) = buf.trim().parse::<i64>() {
                    self.apply_setting(&item.key, SettingValue::Int(n.clamp(min, max)));
                }
            }
            Widget::Text(_) => self.apply_setting(&item.key, SettingValue::Text(buf)),
            _ => {}
        }
    }

    fn settings_dropdown_move(&mut self, delta: i64) {
        let len = self
            .settings
            .as_ref()
            .and_then(|v| v.selected_item())
            .map(|it| match &it.widget {
                Widget::Select { options, .. } => options.len(),
                _ => 0,
            })
            .unwrap_or(0);
        if len == 0 {
            return;
        }
        if let Some(view) = &mut self.settings {
            if let Some(d) = view.dropdown {
                view.dropdown = Some((d as i64 + delta).rem_euclid(len as i64) as usize);
            }
        }
    }

    fn settings_dropdown_commit(&mut self) {
        let chosen = self.settings.as_ref().and_then(|v| {
            let d = v.dropdown?;
            match &v.selected_item()?.widget {
                Widget::Select { options, .. } => options.get(d).cloned(),
                _ => None,
            }
        });
        let key = self
            .settings
            .as_ref()
            .and_then(|v| v.selected_item().map(|it| it.key.clone()));
        self.settings_view_mut().dropdown = None;
        if let (Some(key), Some(chosen)) = (key, chosen) {
            self.apply_setting(&key, SettingValue::Select(chosen));
        }
    }

    // --- mouse -----------------------------------------------------------

    /// Handle a click at screen cell `(col, row)` while the Settings tab is active:
    /// focus the clicked row and act on its widget (toggle, cycle a dropdown, or edit).
    pub(super) fn handle_settings_click(&mut self, _col: u16, row: u16) {
        let area = self.regions.editor;
        let entry_idx = self
            .settings
            .as_ref()
            .and_then(|view| crate::ui::settings_entry_at(view, area, row));
        let Some(entry_idx) = entry_idx else {
            return;
        };
        if !self
            .settings
            .as_ref()
            .is_some_and(|v| matches!(v.entries.get(entry_idx), Some(Entry::Item(_))))
        {
            return;
        }
        let view = self.settings_view_mut();
        view.dropdown = None;
        view.editing = None;
        view.selected = entry_idx;
        let Some(item) = self
            .settings
            .as_ref()
            .and_then(|v| v.selected_item().cloned())
        else {
            return;
        };
        match item.widget {
            Widget::Toggle(v) => self.apply_setting(&item.key, SettingValue::Bool(!v)),
            Widget::Select { .. } => self.settings_adjust(1),
            Widget::Number { value, .. } => self.settings_start_edit(value.to_string()),
            Widget::Text(s) => self.settings_start_edit(s),
        }
    }

    // --- apply / persist -------------------------------------------------

    fn apply_setting(&mut self, key: &str, value: SettingValue) {
        if let Some(id) = key.strip_prefix("plugin:") {
            if let SettingValue::Bool(enabled) = value {
                self.config.disabled_plugins.retain(|d| d != id);
                if !enabled {
                    self.config.disabled_plugins.push(id.to_string());
                }
                self.editor.status_message = Some(format!(
                    "Plugin '{id}' {} — restart to apply",
                    if enabled { "enabled" } else { "disabled" }
                ));
            }
            self.save_config();
            self.rebuild_settings();
            return;
        }

        match (key, value) {
            ("tab_width", SettingValue::Select(s)) => {
                if let Ok(n) = s.parse::<usize>() {
                    self.config.tab_width = n.clamp(1, 16);
                }
            }
            ("sidebar_width", SettingValue::Int(n)) => {
                self.config.sidebar_width = n.clamp(10, 120) as u16;
                self.editor.sidebar_width = self.config.sidebar_width;
            }
            ("terminal_height", SettingValue::Int(n)) => {
                self.config.terminal_height = n.clamp(3, 60) as u16;
                self.panel.height = self.config.terminal_height;
            }
            ("terminal_shell", SettingValue::Text(s)) => {
                let t = s.trim();
                self.config.terminal_shell = (!t.is_empty()).then(|| t.to_string());
            }
            ("follow_mode", SettingValue::Bool(b)) => {
                self.config.follow_mode = b;
                self.follow_mode = b;
            }
            ("poll_watch", SettingValue::Bool(b)) => self.config.poll_watch = b,
            ("auto_pairs", SettingValue::Bool(b)) => self.config.auto_pairs = b,
            ("auto_indent", SettingValue::Bool(b)) => self.config.auto_indent = b,
            ("trim_trailing_whitespace", SettingValue::Bool(b)) => {
                self.config.trim_trailing_whitespace = b
            }
            ("insert_final_newline", SettingValue::Bool(b)) => self.config.insert_final_newline = b,
            ("git_gutter", SettingValue::Bool(b)) => self.config.git_gutter = b,
            ("icons", SettingValue::Bool(b)) => self.config.icons = b,
            ("vim", SettingValue::Bool(b)) => {
                self.config.vim = b;
                self.set_vim(b);
            }
            _ => {}
        }
        self.save_config();
        self.rebuild_settings();
    }

    /// Persist the current config to the config file (the same one the watcher reloads).
    pub(super) fn save_config(&mut self) {
        let Some(path) = self.config_path.clone() else {
            self.editor.status_message = Some("No config path — settings not saved".into());
            return;
        };
        if let Err(e) = self.config.write_to(&path) {
            self.editor.status_message = Some(format!("Could not save settings: {e}"));
        }
    }

    /// Rebuild the form from the (now-changed) config, preserving the cursor + scroll.
    fn rebuild_settings(&mut self) {
        if self.settings.is_none() {
            return;
        }
        let (sel, scroll) = self
            .settings
            .as_ref()
            .map(|v| (v.selected, v.scroll))
            .unwrap();
        let mut view = self.build_settings_view();
        view.selected = sel.min(view.entries.len().saturating_sub(1));
        if !matches!(view.entries.get(view.selected), Some(Entry::Item(_))) {
            view.selected = view
                .entries
                .iter()
                .position(|e| matches!(e, Entry::Item(_)))
                .unwrap_or(0);
        }
        view.scroll = scroll;
        self.settings = Some(view);
    }
}
