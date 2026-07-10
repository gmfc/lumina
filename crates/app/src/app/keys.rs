//! Keyboard input routing: the top-level key dispatch and the modal/sidebar/terminal branches.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    pub(super) fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::keymap::{Chord, Resolve};

        // Drop stale Settings state if its tab was closed.
        self.reconcile_settings();

        // Focus- and overlay-specific handlers get first refusal on the key.
        if self.capture_key(key) {
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
        // The completion popup re-syncs to the edit just made through its owner's `on_event`
        // (broadcast when the edit's DidChange drains); nothing to do here.
    }

    /// Give the active focus/overlay handlers first refusal on a key, in priority order.
    /// Returns `true` when one of them consumed it (so chord resolution is skipped).
    fn capture_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        // Modal overlays (confirm-close / prompt / picker).
        if self.handle_modal_key(key) {
            return true;
        }
        // Terminal focus: forward keystrokes to the shell (except panel-management chords).
        if self.editor.focus == Focus::Panel && self.handle_terminal_key(key) {
            return true;
        }
        // Caret popup (completion): its owner consumes navigation / accept / dismiss keys; other
        // keys fall through to editing, after which the plugin re-syncs on the resulting change.
        if self.editor.popup.is_some() && self.popup_key(key) {
            return true;
        }
        // Sidebar focus: arrows/enter drive the explorer; Esc returns to the editor.
        if self.editor.focus == Focus::Sidebar && self.handle_sidebar_key(key) {
            return true;
        }
        // Settings tab: its widgets consume nav/toggle/edit keys; un-owned chords fall through.
        if self.settings_active()
            && self.editor.focus == Focus::Editor
            && self.handle_settings_key(key)
        {
            self.pending.clear();
            return true;
        }
        // Vim modal layer: consumes normal/visual keys; Insert text and un-owned chords fall through.
        if self.handle_vim_key(key) {
            self.pending.clear();
            return true;
        }
        // Registry-contributed raw-key capturers (vim/terminal once they migrate to plugins) get
        // the last refusal before chord resolution. A no-op today: no built-in plugin overrides
        // `Plugin::capture_key`, so this preserves behavior until a capturer exists.
        if let Some(pk) = to_plugin_key(key) {
            if self.registry.capture_key(pk, &mut self.editor) {
                self.pending.clear();
                return true;
            }
        }
        false
    }

    /// Route a key to an active modal (confirm-close overlay / plugin prompt / picker), in
    /// priority order. Returns `true` when a modal consumed the key.
    pub(super) fn handle_modal_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.editor.overlay.is_some() {
            self.overlay_key(key);
        } else if self.editor.prompt.is_some() {
            self.prompt_key(key);
        } else if self.editor.picker.is_some() {
            self.picker_key(key);
        } else {
            return false;
        }
        true
    }

    /// Route a key to the plugin owning the active prompt (find/replace today). The app does no
    /// editing itself — the owner's `on_prompt_key` handles it — so this just translates the key
    /// and forwards it, then drains any effects the plugin queued.
    pub(super) fn prompt_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some((owner, prompt_id)) = self
            .editor
            .prompt
            .as_ref()
            .map(|p| (p.owner.clone(), p.prompt_id.clone()))
        else {
            return;
        };
        if let Some(pk) = to_plugin_key(key) {
            self.registry
                .dispatch_prompt_key(&owner, &prompt_id, pk, &mut self.editor);
            self.drain_workers();
        }
    }

    /// Offer a key to the owner of the active caret popup (completion). Returns `true` when the
    /// owner consumed it (navigation / accept / dismiss); `false` lets it fall through to editing.
    pub(super) fn popup_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        let Some(owner) = self.editor.popup.as_ref().map(|p| p.owner.clone()) else {
            return false;
        };
        let Some(pk) = to_plugin_key(key) else {
            return false;
        };
        if self
            .registry
            .dispatch_popup_key(&owner, pk, &mut self.editor)
        {
            self.drain_workers();
            true
        } else {
            false
        }
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
        if !(single && self.editor.focus == Focus::Editor) || self.settings_active() {
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

/// Translate a crossterm key event into the kernel's crossterm-free [`editor_plugin::Key`] so a
/// raw-key-capturing plugin never sees a terminal type. Returns `None` for keys the editor never
/// binds (media keys, caps-lock, …). The character keeps its delivered case — crossterm already
/// folds shift into the char — so a modal layer can tell `d` from `D`; `shift` is reported only
/// for the named keys, mirroring [`Chord::from_event`].
fn to_plugin_key(key: crossterm::event::KeyEvent) -> Option<editor_plugin::Key> {
    use crossterm::event::{KeyCode as Ct, KeyModifiers};
    use editor_plugin::KeyCode as Pk;
    let (code, is_char) = match key.code {
        Ct::Char(c) => (Pk::Char(c), true),
        Ct::Enter => (Pk::Enter, false),
        Ct::Tab => (Pk::Tab, false),
        Ct::BackTab => (Pk::BackTab, false),
        Ct::Esc => (Pk::Esc, false),
        Ct::Backspace => (Pk::Backspace, false),
        Ct::Delete => (Pk::Delete, false),
        Ct::Insert => (Pk::Insert, false),
        Ct::Up => (Pk::Up, false),
        Ct::Down => (Pk::Down, false),
        Ct::Left => (Pk::Left, false),
        Ct::Right => (Pk::Right, false),
        Ct::Home => (Pk::Home, false),
        Ct::End => (Pk::End, false),
        Ct::PageUp => (Pk::PageUp, false),
        Ct::PageDown => (Pk::PageDown, false),
        Ct::F(n) => (Pk::F(n), false),
        _ => return None,
    };
    Some(editor_plugin::Key {
        code,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        // Case already lives in the char; only report shift for the named keys.
        shift: !is_char && key.modifiers.contains(KeyModifiers::SHIFT),
    })
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
