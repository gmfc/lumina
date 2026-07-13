//! App-side terminal support: the PTY sessions the `terminal` plugin's lifecycle drives.
//!
//! The dock lifecycle (open/minimized/active/tab-order) lives in the `terminal` builtin plugin,
//! which publishes a [`editor_plugin::TerminalView`] the renderer reads. The app keeps the concrete
//! PTY sessions (`EditorState.terminals`, keyed by `TerminalId`): it resizes them to the drawn
//! region, looks up the active one for key/mouse input and rendering, and routes header clicks back
//! to the plugin. Part of the [`crate::app`] module.

use super::*;
use crate::terminal::{HeaderHit, Terminal};

impl App {
    /// The active terminal (the plugin's `active` tab), if the dock has one.
    pub(crate) fn active_terminal(&self) -> Option<&Terminal> {
        let view = &self.editor.terminal_view;
        let id = view.order.get(view.active)?;
        self.editor.terminals.get(id)
    }

    /// The active terminal, mutably (for key / paste / scroll input).
    pub(super) fn active_terminal_mut(&mut self) -> Option<&mut Terminal> {
        let view = &self.editor.terminal_view;
        let id = *view.order.get(view.active)?;
        self.editor.terminals.get_mut(&id)
    }

    /// The header laid out left-to-right as `(label, hit)` segments — the minimize control, one tab
    /// per terminal (from the plugin's `order`, titled/marked from the app-owned session), and `+`.
    /// Labels use only width-1 glyphs, so callers treat one char as one display column.
    pub(crate) fn terminal_header_segments(&self) -> Vec<(String, HeaderHit)> {
        let view = &self.editor.terminal_view;
        let mut segs = Vec::with_capacity(view.order.len() + 2);
        let ctrl = if view.minimized { " ▸ " } else { " ▾ " };
        segs.push((ctrl.to_string(), HeaderHit::Minimize));
        for (i, id) in view.order.iter().enumerate() {
            let (title, exited) = self
                .editor
                .terminals
                .get(id)
                .map(|t| (t.title.as_str(), t.exited))
                .unwrap_or(("", false));
            let mark = if exited { '·' } else { '×' };
            segs.push((format!(" {}: {title} {mark} ", i + 1), HeaderHit::Tab(i)));
        }
        segs.push((" + ".to_string(), HeaderHit::New));
        segs
    }

    /// Resize every PTY to the drawn content region (a cheap no-op when unchanged).
    pub(super) fn sync_terminals(&mut self) {
        let Some(rect) = self.regions.panel_content else {
            return;
        };
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        for t in self.editor.terminals.values_mut() {
            t.resize(rect.height, rect.width);
        }
    }

    /// Handle a left click on the panel header: hit-test a segment and hand the `terminal` plugin
    /// the corresponding lifecycle action (`minimize` / `new` / `select:N` / `close:N`). The close
    /// mark sits in a tab's last two columns.
    pub(super) fn panel_header_click(&mut self, col: u16, row: u16) {
        let Some(header) = self.regions.panel_header else {
            return;
        };
        if !in_rect(header, col, row) {
            return;
        }
        let mut x = header.x;
        for (label, hit) in self.terminal_header_segments() {
            let w = label.chars().count() as u16;
            let seg_end = x.saturating_add(w);
            if col >= x && col < seg_end {
                let payload = match hit {
                    HeaderHit::Minimize => "minimize".to_string(),
                    HeaderHit::New => "new".to_string(),
                    HeaderHit::Tab(i) if col >= seg_end.saturating_sub(2) => format!("close:{i}"),
                    HeaderHit::Tab(i) => format!("select:{i}"),
                };
                self.registry
                    .dispatch_owned("terminal", &payload, &mut self.editor);
                self.drain_workers();
                return;
            }
            x = seg_end;
        }
    }
}
