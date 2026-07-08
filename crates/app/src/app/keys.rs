//! Keyboard input routing: the top-level key dispatch and the modal/sidebar/terminal branches.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    pub(super) fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::keymap::{Chord, Resolve};

        // Modal captures, in priority order.
        if self.handle_modal_key(key) {
            return;
        }
        // Terminal focus: forward keystrokes to the shell (except panel-management chords).
        if self.editor.focus == Focus::Panel && self.handle_terminal_key(key) {
            return;
        }
        // Completion popup: navigation / accept / dismiss keys are consumed here; anything
        // else falls through to edit the buffer, then re-syncs the popup below (plan §2.1).
        if self.editor.completion.is_some() && self.completion_key(key) {
            return;
        }
        // Sidebar focus: arrows/enter drive the explorer; Esc returns to the editor.
        if self.editor.focus == Focus::Sidebar && self.handle_sidebar_key(key) {
            return;
        }

        // Chord keymap resolution (defaults + config overrides).
        self.pending.push(Chord::from_event(key));
        match self.keymap.resolve(&self.pending) {
            Resolve::Command(id) => {
                self.pending.clear();
                self.editor.status_message = None;
                self.exec_id(&id);
            }
            Resolve::Pending => {
                // Keep the prefix armed; show it in the status bar.
                self.editor.status_message = Some(format!("{} …", chords_label(&self.pending)));
            }
            Resolve::None => self.text_entry_fallback(key),
        }

        // Keep the completion popup in sync with the edit just made, or drop it if a modal
        // opened on top of it.
        if self.editor.completion.is_some() {
            if self.editor.overlay.is_some()
                || self.editor.picker.is_some()
                || self.editor.find.is_some()
                || self.search.is_some()
            {
                self.editor.completion = None;
            } else {
                self.refresh_completion();
            }
        }
    }

    /// Route a key to an active modal (overlay / picker / search / find), in priority
    /// order. Returns `true` when a modal consumed the key.
    pub(super) fn handle_modal_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.editor.overlay.is_some() {
            self.overlay_key(key);
        } else if self.editor.picker.is_some() {
            self.picker_key(key);
        } else if self.search.is_some() {
            self.search_key(key);
        } else if self.editor.find.is_some() {
            self.find_key(key);
        } else {
            return false;
        }
        true
    }

    /// Handle a key while the sidebar is focused. Returns `true` when it was consumed;
    /// `false` lets it fall through to normal chord resolution.
    pub(super) fn handle_sidebar_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        if key.code == KeyCode::Esc {
            self.editor.focus = Focus::Editor;
            return true;
        }
        if let Some(id) = sidebar_command(key) {
            self.registry.dispatch_command(id, &mut self.editor);
            self.drain_workers();
            return true;
        }
        false
    }

    /// Forward a key to the active terminal. `terminal.*` management chords (e.g. the toggle)
    /// are still honored, so there is always a keyboard way to close / switch / minimize the
    /// panel from inside it; everything else becomes shell input. Returns `true` when handled.
    pub(super) fn handle_terminal_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        // A focused-but-empty or collapsed panel shouldn't hold the keyboard.
        if self.panel.active_terminal().is_none() || !self.panel.open || self.panel.minimized {
            self.editor.focus = Focus::Editor;
            return false;
        }
        let chord = Chord::from_event(key);
        if let crate::keymap::Resolve::Command(id) =
            self.keymap.resolve(std::slice::from_ref(&chord))
        {
            if id.starts_with("terminal.") {
                self.pending.clear();
                self.exec_id(&id);
                return true;
            }
        }
        let app_cursor = self
            .panel
            .active_terminal()
            .map(|t| t.application_cursor())
            .unwrap_or(false);
        if let Some(bytes) = crate::terminal::key_to_bytes(&key, app_cursor) {
            if let Some(t) = self.panel.active_terminal_mut() {
                t.send_input(&bytes);
            }
        }
        true
    }

    /// Route a bracketed paste to the terminal when it is focused, else into the document.
    pub(super) fn on_paste(&mut self, s: String) {
        if self.editor.focus == Focus::Panel && self.panel.open && !self.panel.minimized {
            if let Some(t) = self.panel.active_terminal_mut() {
                t.send_input(s.as_bytes());
                return;
            }
        }
        self.dispatch(Command::Paste(s));
    }

    /// Fallback for a key that resolved to nothing: printable text entry into the editor.
    pub(super) fn text_entry_fallback(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let single = self.pending.len() == 1;
        self.pending.clear();
        if !(single && self.editor.focus == Focus::Editor) {
            return;
        }
        if let KeyCode::Char(c) = key.code {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            if !ctrl && !alt {
                self.dispatch(Command::InsertChar(c));
            }
        }
    }
}

/// Human label for a pending chord prefix (shown in the status bar).
fn chords_label(chords: &[Chord]) -> String {
    use crossterm::event::KeyCode;
    chords
        .iter()
        .map(|c| {
            let mut s = String::new();
            if c.ctrl {
                s.push_str("Ctrl+");
            }
            if c.alt {
                s.push_str("Alt+");
            }
            if c.shift {
                s.push_str("Shift+");
            }
            match c.code {
                KeyCode::Char(ch) => s.push(ch),
                other => s.push_str(&format!("{other:?}")),
            }
            s
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map a key to an explorer command id when the sidebar is focused (keyboard parity).
fn sidebar_command(key: crossterm::event::KeyEvent) -> Option<&'static str> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Up => Some("explorer.up"),
        KeyCode::Down => Some("explorer.down"),
        KeyCode::Right => Some("explorer.expand"),
        KeyCode::Left => Some("explorer.collapse"),
        KeyCode::Enter => Some("explorer.activate"),
        _ => None,
    }
}
