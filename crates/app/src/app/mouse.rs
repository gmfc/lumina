//! Mouse routing: hit-testing, click/drag on the editor, tabs, and sidebar.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    pub(super) fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        let (col, row) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_left_down(col, row, m.modifiers),
            MouseEventKind::Down(MouseButton::Middle) => self.mouse_middle_down(col, row),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_left_drag(col, row),
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_anchor = None;
                self.tab_drag = None;
            }
            MouseEventKind::ScrollUp => self.scroll_at(col, row, -3),
            MouseEventKind::ScrollDown => self.scroll_at(col, row, 3),
            _ => {}
        }
    }

    /// Left button press: focus + place/add a cursor in the editor, or hit the tab bar
    /// or sidebar depending on which region was clicked.
    pub(super) fn mouse_left_down(
        &mut self,
        col: u16,
        row: u16,
        mods: crossterm::event::KeyModifiers,
    ) {
        if in_rect(self.regions.editor, col, row) {
            self.editor.focus = Focus::Editor;
            if mods.contains(crossterm::event::KeyModifiers::ALT) {
                // Alt+Click adds a cursor (multi-cursor).
                if let Some(off) = self.editor_offset_at(col, row) {
                    self.with_doc(|d| {
                        d.selections.push(Selection::caret(off));
                        d.selections.normalize();
                    });
                }
            } else {
                self.editor_click(col, row);
            }
        } else if in_rect(self.regions.tabs, col, row) {
            self.tab_bar_click(col);
        } else if self.regions.sidebar.is_some_and(|r| in_rect(r, col, row)) {
            self.editor.focus = Focus::Sidebar;
            self.sidebar_click(col, row);
        } else if self
            .regions
            .panel_header
            .is_some_and(|r| in_rect(r, col, row))
        {
            self.panel_header_click(col, row);
        } else if self
            .regions
            .panel_content
            .is_some_and(|r| in_rect(r, col, row))
            && self.panel.active_terminal().is_some()
        {
            self.editor.focus = Focus::Panel;
        }
    }

    /// Middle-click on the tab bar closes that tab (VS Code parity).
    pub(super) fn mouse_middle_down(&mut self, col: u16, row: u16) {
        if in_rect(self.regions.tabs, col, row) {
            if let Some((i, _)) = self.tab_at(col) {
                self.request_close(i);
            }
        }
    }

    /// Left button drag: reorder the dragged tab, or extend the editor selection.
    pub(super) fn mouse_left_drag(&mut self, col: u16, row: u16) {
        if let Some(from) = self.tab_drag {
            self.drag_tab(from, col, row);
        } else if let Some(anchor) = self.drag_anchor {
            if let Some(off) = self.editor_offset_at(col, row) {
                self.with_doc(|d| {
                    d.selections.set_single(Selection::new(anchor, off));
                });
            }
        }
    }

    /// Continue an in-progress tab drag, reordering when the cursor crosses onto a new tab.
    pub(super) fn drag_tab(&mut self, from: usize, col: u16, row: u16) {
        if !in_rect(self.regions.tabs, col, row) {
            return;
        }
        if let Some((to, _)) = self.tab_at(col) {
            if to != from {
                self.editor.workspace.move_tab(from, to);
                self.tab_drag = Some(to);
            }
        }
    }

    /// Char offset under a screen cell in the editor pane, or `None`.
    pub(super) fn editor_offset_at(&self, col: u16, row: u16) -> Option<usize> {
        let doc = self.editor.active_document()?;
        let geo = self.editor_geometry(doc);
        screen_to_char(doc, &geo, col, row)
    }

    pub(super) fn editor_geometry(&self, doc: &Document) -> PaneGeometry {
        let r = self.regions.editor;
        PaneGeometry {
            origin_x: r.x,
            origin_y: r.y,
            gutter: ui::gutter_width(doc),
            scroll_line: doc.view.scroll_line,
            scroll_col: doc.view.scroll_col,
            tab_width: doc.tab_width,
            height: r.height,
        }
    }

    /// Handle a left click in the editor: place cursor, or select word/line on multi-click.
    pub(super) fn editor_click(&mut self, col: u16, row: u16) {
        let Some(off) = self.editor_offset_at(col, row) else {
            return;
        };
        // Determine click count from timing + position.
        let now = Instant::now();
        let count = match &self.last_click {
            Some(c)
                if now.duration_since(c.at) < Duration::from_millis(400) && c.char_pos == off =>
            {
                (c.count % 3) + 1
            }
            _ => 1,
        };
        self.last_click = Some(ClickState {
            at: now,
            char_pos: off,
            count,
        });

        match count {
            2 => self.with_doc(|d| {
                let (s, e) = motion::word_at(d, off);
                d.selections.set_single(Selection::new(s, e));
            }),
            3 => self.with_doc(|d| {
                let (s, e) = motion::line_at(d, off);
                d.selections.set_single(Selection::new(s, e));
            }),
            _ => {
                self.drag_anchor = Some(off);
                self.with_doc(|d| d.selections.set_single(Selection::caret(off)));
            }
        }
    }

    /// Hit-test a tab-bar column, returning `(tab_index, on_close_marker)`.
    pub(super) fn tab_at(&self, col: u16) -> Option<(usize, bool)> {
        // Tabs render as " name marker " segments; recompute their extents to hit-test.
        let ws = &self.editor.workspace;
        let mut x = self.regions.tabs.x;
        for (i, &id) in ws.tabs.iter().enumerate() {
            let doc = ws.documents.get(id)?;
            let name = doc
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into());
            // Segment width: " name _ marker _ ".
            let label_w = 1 + name.chars().count() + 1 + 1 + 1;
            // Saturate: many/long tabs could push the running offset past u16::MAX, overflowing
            // (panic in debug, mis-hit in release). A click can't land past the screen anyway.
            let seg_end = x.saturating_add(u16::try_from(label_w).unwrap_or(u16::MAX));
            if col >= x && col < seg_end {
                // The marker (× / ●) sits near the segment's right edge.
                return Some((i, col >= seg_end.saturating_sub(2)));
            }
            x = seg_end;
        }
        None
    }

    pub(super) fn tab_bar_click(&mut self, col: u16) {
        if let Some((i, on_close)) = self.tab_at(col) {
            if on_close {
                self.request_close(i);
            } else {
                self.editor.workspace.focus_tab(i);
                self.tab_drag = Some(i); // arm a potential drag-to-reorder
            }
        }
    }

    pub(super) fn sidebar_click(&mut self, _col: u16, row: u16) {
        // Route to the explorer plugin's panel row, if present (Phase 4).
        if let Some(panel) = self.editor.panels.get("explorer.tree") {
            // Panel rows are drawn into the sidebar block's *inner* area (below the
            // " EXPLORER " title row), so hit-test against that content region — using the
            // outer region's top would select the row one line below the cursor.
            let inner_top = self
                .regions
                .sidebar_inner
                .or(self.regions.sidebar)
                .map(|r| r.y)
                .unwrap_or(0);
            let idx = row.saturating_sub(inner_top) as usize;
            if let Some(line) = panel.lines.get(idx) {
                if let Some(payload) = line.payload.clone() {
                    self.registry
                        .activate_panel_row("explorer.tree", &payload, &mut self.editor);
                }
            }
        }
    }

    pub(super) fn scroll_editor(&mut self, delta: isize) {
        if let Some(doc) = self.editor.active_document_mut() {
            let max = doc.len_lines().saturating_sub(1);
            let next = (doc.view.scroll_line as isize + delta).clamp(0, max as isize);
            doc.view.scroll_line = next as usize;
        }
    }

    /// Route a wheel scroll: the terminal's scrollback when over the panel, else the editor.
    pub(super) fn scroll_at(&mut self, col: u16, row: u16, delta: isize) {
        if self
            .regions
            .panel_content
            .is_some_and(|r| in_rect(r, col, row))
        {
            if let Some(t) = self.panel.active_terminal_mut() {
                t.scroll(delta);
            }
        } else {
            self.scroll_editor(delta);
        }
    }

    // --- terminal panel -------------------------------------------------------
}
