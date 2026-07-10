//! Terminal-dock control: open/close/minimize, spawning, and header hit-handling.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Apply a plugin-queued terminal lifecycle action to the (app-owned) PTY panel.
    pub(super) fn apply_terminal_op(&mut self, op: editor_plugin::TerminalOp) {
        use editor_plugin::TerminalOp as T;
        match op {
            T::Toggle => self.toggle_terminal(),
            T::New => self.new_terminal(),
            T::Close => self.close_terminal(),
            T::Minimize => self.minimize_terminal(),
            T::Next => {
                if self.panel.open {
                    self.panel.next();
                }
            }
            T::Prev => {
                if self.panel.open {
                    self.panel.prev();
                }
            }
        }
    }

    /// Toggle the dock: open + focus when closed or minimized, else close it.
    pub(super) fn toggle_terminal(&mut self) {
        if self.panel.open && !self.panel.minimized {
            self.panel.open = false;
            self.editor.focus = Focus::Editor;
        } else {
            self.focus_terminal();
        }
    }

    /// Open (if needed), expand, and focus the panel, spawning a shell on first use.
    pub(super) fn focus_terminal(&mut self) {
        self.panel.open = true;
        self.panel.minimized = false;
        if self.panel.terminals.is_empty() {
            self.spawn_terminal();
        }
        if self.panel.active_terminal().is_some() {
            self.editor.focus = Focus::Panel;
        } else {
            self.panel.open = false;
        }
    }

    /// Open a brand-new terminal tab and focus the panel.
    pub(super) fn new_terminal(&mut self) {
        self.panel.open = true;
        self.panel.minimized = false;
        self.spawn_terminal();
        if self.panel.active_terminal().is_some() {
            self.editor.focus = Focus::Panel;
        }
    }

    /// Close the active terminal tab; close the dock and return to the editor if it was last.
    pub(super) fn close_terminal(&mut self) {
        if self.panel.close_active() {
            self.panel.open = false;
            self.editor.focus = Focus::Editor;
        }
    }

    /// Collapse the panel to its header row, or restore it.
    pub(super) fn minimize_terminal(&mut self) {
        if !self.panel.open {
            return;
        }
        self.panel.toggle_minimized();
        if self.panel.minimized {
            self.editor.focus = Focus::Editor;
        } else if self.panel.active_terminal().is_some() {
            self.editor.focus = Focus::Panel;
        }
    }

    /// Spawn a shell into a new tab, sized to the current panel region.
    pub(super) fn spawn_terminal(&mut self) {
        let cwd = self.editor.workspace.root.clone();
        let shell = crate::terminal::default_shell(self.config.terminal_shell.as_deref());
        let (rows, cols) = self.panel_content_size();
        let tx = self.worker_tx.clone();
        if !self.panel.open_new(&cwd, &shell, rows, cols, tx) {
            self.editor.status_message = Some("Failed to start terminal".into());
        }
    }

    /// A terminal's `(rows, cols)`, from the last-laid-out panel region with a pre-draw fallback.
    pub(super) fn panel_content_size(&self) -> (u16, u16) {
        if let Some(rect) = self.regions.panel_content {
            if rect.width > 0 && rect.height > 0 {
                return (rect.height, rect.width);
            }
        }
        (self.panel.height, self.regions.editor.width.max(80))
    }

    /// Resize every PTY to the drawn content region (a cheap no-op when unchanged).
    pub(super) fn sync_terminals(&mut self) {
        let Some(rect) = self.regions.panel_content else {
            return;
        };
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        for t in &mut self.panel.terminals {
            t.resize(rect.height, rect.width);
        }
    }

    /// Handle a left click on the panel header (minimize control, a tab, its close mark, or `+`).
    pub(super) fn panel_header_click(&mut self, col: u16, row: u16) {
        let Some(header) = self.regions.panel_header else {
            return;
        };
        if !in_rect(header, col, row) {
            return;
        }
        let mut x = header.x;
        for (label, hit) in self.panel.header_segments() {
            let w = label.chars().count() as u16;
            let seg_end = x.saturating_add(w);
            if col >= x && col < seg_end {
                self.activate_header_hit(hit, col, seg_end);
                return;
            }
            x = seg_end;
        }
    }

    /// Act on a header segment: the close mark sits in the tab's last two columns.
    pub(super) fn activate_header_hit(
        &mut self,
        hit: crate::terminal::HeaderHit,
        col: u16,
        seg_end: u16,
    ) {
        use crate::terminal::HeaderHit;
        match hit {
            HeaderHit::Minimize => self.minimize_terminal(),
            HeaderHit::New => self.new_terminal(),
            HeaderHit::Tab(i) => {
                self.panel.select(i);
                if col >= seg_end.saturating_sub(2) {
                    self.close_terminal();
                } else {
                    self.panel.open = true;
                    self.panel.minimized = false;
                    if self.panel.active_terminal().is_some() {
                        self.editor.focus = Focus::Panel;
                    }
                }
            }
        }
    }
}
