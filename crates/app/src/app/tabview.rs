//! Input for non-text tabs: the refusal notice and plugin viewers.
//!
//! These tabs are backed by an empty placeholder buffer (see [`crate::editor::TabView`]), so the
//! job here is twofold: give them the scrolling and actions they need, and make sure nothing
//! that would edit or dirty that placeholder ever reaches it.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::TabView;

/// Actions offered on a notice tab: `(command id, label)`. Only entries whose command is
/// actually registered are shown, and the chord comes from the live keymap — the same
/// data-driven approach as the welcome screen, so a remap or a disabled plugin is reflected
/// rather than advertised wrongly.
pub(crate) const NOTICE_ACTIONS: &[(&str, &str)] = &[
    ("file.openAnyway", "Open as text anyway"),
    ("view.openAsHex", "View as hex"),
    ("tab.close", "Close this tab"),
];

/// Actions offered on a *viewer* tab, same rules. `file.openAsText` is the escape hatch that
/// keeps a viewer from taking a text extension hostage.
pub(crate) const VIEWER_ACTIONS: &[(&str, &str)] = &[
    ("file.openAsText", "Open as text"),
    ("view.openAsHex", "View as hex"),
];

impl App {
    /// Route a key on a notice/viewer tab. Returns `true` when it was consumed.
    pub(super) fn tab_view_key(&mut self, key: KeyEvent) -> bool {
        // A chord prefix is armed (the user pressed Ctrl+K): its continuation key is a plain
        // char, so swallowing it here would make every `ctrl+k <key>` chord — Save All among
        // them — silently dead on exactly the tabs this runs for.
        if !self.pending.is_empty() {
            return false;
        }
        let page = self.tab_view_page().max(1);
        // A chord with Ctrl/Alt belongs to the keymap (Ctrl+W, Ctrl+P, the palette); only bare
        // and Shift-modified keys are ours.
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if plain => self.scroll_tab_view(1),
            KeyCode::Up | KeyCode::Char('k') if plain => self.scroll_tab_view(-1),
            KeyCode::PageDown => self.scroll_tab_view(page as isize),
            KeyCode::PageUp => self.scroll_tab_view(-(page as isize)),
            KeyCode::Home if plain => self.set_tab_view_scroll(0),
            KeyCode::End if plain => self.set_tab_view_scroll(usize::MAX),
            KeyCode::Enter if plain => self.tab_view_activate(),
            // Swallow ordinary typing: the buffer behind this tab is a placeholder aimed at a
            // real file, and letting it go dirty would put a Save prompt in front of the user
            // for content that doesn't exist.
            KeyCode::Char(_) if plain => {}
            _ => return false,
        }
        true
    }

    /// Enter takes the tab's primary action: on a notice, force a size-refused file open as
    /// text; on a viewer, open the file as text (the escape hatch from an extension claim).
    fn tab_view_activate(&mut self) {
        match self.editor.active_tab_view() {
            Some(TabView::Notice { .. }) => self.open_anyway(),
            Some(TabView::Viewer(_)) => self.open_as_text(),
            None => {}
        }
    }

    /// Scroll the active viewer tab by `delta` rows, clamped to its content. Notice tabs are a
    /// fixed centered box and don't scroll.
    pub(super) fn scroll_tab_view(&mut self, delta: isize) {
        let max = self.tab_view_max_scroll();
        if let Some(TabView::Viewer(v)) = self.editor.active_tab_view_mut() {
            let next = (v.scroll as isize + delta).max(0) as usize;
            v.scroll = next.min(max);
        }
    }

    /// Jump the active viewer to an absolute row (`usize::MAX` = the end).
    fn set_tab_view_scroll(&mut self, row: usize) {
        let max = self.tab_view_max_scroll();
        if let Some(TabView::Viewer(v)) = self.editor.active_tab_view_mut() {
            v.scroll = row.min(max);
        }
    }

    /// Rows of viewer body currently on screen. May be 0 on a pane too short to draw a body;
    /// callers that page apply their own `.max(1)` so a keypress always does something.
    fn tab_view_page(&self) -> usize {
        crate::ui::viewer_body_rows(self.regions.editor.height)
    }

    /// The largest scroll offset that still shows content — the last *page*, not the last row.
    /// Zero when the pane is too short to draw a body at all: there is nothing to scroll, and
    /// letting `scroll` wander would strand the view once the pane grew again.
    fn tab_view_max_scroll(&self) -> usize {
        let rows = self.tab_view_page();
        if rows == 0 {
            return 0;
        }
        let Some(TabView::Viewer(v)) = self.editor.active_tab_view() else {
            return 0;
        };
        v.content.lines.len().saturating_sub(rows)
    }
}
