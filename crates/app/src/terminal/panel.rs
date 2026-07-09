//! The bottom terminal dock: a set of terminal tabs plus its open / minimized / size state,
//! and the header layout the renderer and mouse router share.

use std::path::Path;

use crate::worker::WorkerTx;

use super::session::Terminal;

/// A clickable region of the panel header, returned by hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderHit {
    /// The minimize / restore control.
    Minimize,
    /// The tab for terminal at this index.
    Tab(usize),
    /// The "new terminal" (`+`) control.
    New,
}

/// The bottom terminal dock: a set of terminal tabs plus its open / minimized / size state.
pub struct TerminalPanel {
    pub terminals: Vec<Terminal>,
    pub active: usize,
    /// Whether the panel occupies space in the layout at all.
    pub open: bool,
    /// When open, whether it is collapsed to just its header row.
    pub minimized: bool,
    /// Desired content height (rows) when expanded.
    pub height: u16,
    next_id: u64,
}

impl TerminalPanel {
    pub fn new(height: u16) -> TerminalPanel {
        TerminalPanel {
            terminals: Vec::new(),
            active: 0,
            open: false,
            minimized: false,
            height: height.clamp(3, 60),
            next_id: 1,
        }
    }

    pub fn active_terminal(&self) -> Option<&Terminal> {
        self.terminals.get(self.active)
    }

    pub fn active_terminal_mut(&mut self) -> Option<&mut Terminal> {
        self.terminals.get_mut(self.active)
    }

    /// The terminal with `id`, if still present (routes reader-thread messages).
    pub fn terminal_mut(&mut self, id: u64) -> Option<&mut Terminal> {
        self.terminals.iter_mut().find(|t| t.id == id)
    }

    /// Spawn a new terminal tab and make it active. Returns `false` if spawning failed.
    pub fn open_new(
        &mut self,
        cwd: &Path,
        shell: &str,
        rows: u16,
        cols: u16,
        tx: WorkerTx,
    ) -> bool {
        let id = self.next_id;
        match Terminal::new(id, cwd, shell, rows, cols, tx) {
            Some(term) => {
                self.next_id += 1;
                self.terminals.push(term);
                self.active = self.terminals.len() - 1;
                true
            }
            None => false,
        }
    }

    /// Close the active tab (its `Drop` kills the shell). Returns `true` if the panel is now
    /// empty (the caller closes the dock and returns focus to the editor).
    pub fn close_active(&mut self) -> bool {
        if self.terminals.is_empty() {
            return true;
        }
        let removed = self.active;
        self.terminals.remove(removed);
        self.active = index_after_close(self.terminals.len() + 1, self.active, removed);
        self.terminals.is_empty()
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.terminals.len() {
            self.active = idx;
        }
    }

    pub fn next(&mut self) {
        self.active = next_index(self.terminals.len(), self.active);
    }

    pub fn prev(&mut self) {
        self.active = prev_index(self.terminals.len(), self.active);
    }

    pub fn toggle_minimized(&mut self) {
        self.minimized = !self.minimized;
    }

    /// The header laid out left-to-right as `(label, hit)` segments. Labels use only width-1
    /// glyphs, so callers may treat one char as one display column for layout / hit-testing.
    pub fn header_segments(&self) -> Vec<(String, HeaderHit)> {
        let mut segs = Vec::with_capacity(self.terminals.len() + 2);
        let ctrl = if self.minimized { " ▸ " } else { " ▾ " };
        segs.push((ctrl.to_string(), HeaderHit::Minimize));
        for (i, t) in self.terminals.iter().enumerate() {
            let mark = if t.exited { '·' } else { '×' };
            segs.push((
                format!(" {}: {} {mark} ", i + 1, t.title),
                HeaderHit::Tab(i),
            ));
        }
        segs.push((" + ".to_string(), HeaderHit::New));
        segs
    }
}

/// Next tab index (wraps); `active` when the list is empty.
fn next_index(len: usize, active: usize) -> usize {
    if len == 0 {
        0
    } else {
        (active + 1) % len
    }
}

/// Previous tab index (wraps); `active` when the list is empty.
fn prev_index(len: usize, active: usize) -> usize {
    if len == 0 {
        0
    } else {
        (active + len - 1) % len
    }
}

/// New active index after removing `removed` from a list that had `old_len` items.
fn index_after_close(old_len: usize, active: usize, removed: usize) -> usize {
    let new_len = old_len.saturating_sub(1);
    if new_len == 0 {
        0
    } else if removed < active {
        active - 1
    } else {
        active.min(new_len - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_index_math() {
        assert_eq!(next_index(3, 0), 1);
        assert_eq!(next_index(3, 2), 0);
        assert_eq!(next_index(0, 0), 0);
        assert_eq!(prev_index(3, 0), 2);
        assert_eq!(prev_index(3, 1), 0);

        // Closing before the active tab shifts the active index down.
        assert_eq!(index_after_close(3, 2, 0), 1);
        // Closing the active last tab clamps to the new last.
        assert_eq!(index_after_close(3, 2, 2), 1);
        // Closing after the active tab leaves it put.
        assert_eq!(index_after_close(3, 0, 2), 0);
        // Closing the only tab.
        assert_eq!(index_after_close(1, 0, 0), 0);
    }

    #[test]
    fn panel_flags_and_header() {
        let mut panel = TerminalPanel::new(80);
        // Clamped to the sane range.
        assert_eq!(panel.height, 60);
        assert!(!panel.open && !panel.minimized);
        assert!(panel.active_terminal().is_none());
        // Closing an empty panel reports "now empty".
        assert!(panel.close_active());

        panel.toggle_minimized();
        assert!(panel.minimized);
        // With no terminals the header is just the two controls.
        let segs = panel.header_segments();
        assert_eq!(segs.first().map(|s| s.1), Some(HeaderHit::Minimize));
        assert_eq!(segs.last().map(|s| s.1), Some(HeaderHit::New));
        assert_eq!(segs.len(), 2);
    }
}
