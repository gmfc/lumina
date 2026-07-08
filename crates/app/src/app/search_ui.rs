//! Project-wide search: launching the worker and browsing/opening results.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    pub(super) fn open_search(&mut self) {
        let mut st = crate::search::SearchState::default();
        if let Some(t) = self.selection_text() {
            st.query = t;
        }
        self.search = Some(st);
    }

    /// Kick off a background project search for the current query.
    pub(super) fn run_project_search(&mut self) {
        let (query, case) = match &self.search {
            Some(s) if !s.query.is_empty() => (s.query.clone(), s.case_sensitive),
            _ => return,
        };
        if let Some(s) = &mut self.search {
            s.running = true;
            s.results.clear();
        }
        self.last_search_run = query.clone();
        crate::worker::spawn_search(
            self.editor.workspace.root.clone(),
            query,
            case,
            self.worker_tx.clone(),
        );
    }

    pub(super) fn search_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.search = None,
            KeyCode::Up => {
                if let Some(s) = &mut self.search {
                    s.move_selection(-1);
                }
            }
            KeyCode::Down => {
                if let Some(s) = &mut self.search {
                    s.move_selection(1);
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = &mut self.search {
                    s.query.pop();
                }
            }
            KeyCode::Enter => {
                let changed = self
                    .search
                    .as_ref()
                    .map(|s| s.query != self.last_search_run)
                    .unwrap_or(false);
                if changed {
                    self.run_project_search();
                } else {
                    self.open_search_hit();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(s) = &mut self.search {
                    s.query.push(c);
                }
            }
            _ => {}
        }
    }

    pub(super) fn open_search_hit(&mut self) {
        let hit = self.search.as_ref().and_then(|s| s.selected_hit()).cloned();
        if let Some(hit) = hit {
            self.open_path(&hit.path);
            self.goto_line(hit.line.saturating_sub(1));
        }
    }

    // --- multi-cursor ----------------------------------------------------------
}
