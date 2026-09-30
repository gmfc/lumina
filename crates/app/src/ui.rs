//! Rendering — a pure function of state (plan §4, invariant #8). No mutation of editor
//! state happens here; we only read it and write cells.
//!
//! The frame is assembled by [`draw`]; the pieces live in focused submodules:
//! - [`chrome`] — tab bar, status bar, welcome screen.
//! - [`editor`] — the text pane, its per-line loop, and per-cell decorations.
//! - [`sidebar`] — the explorer panel.
//! - [`tabview`] — non-text tabs: the large/binary-file notice and plugin viewers.
//! - [`panel`] — the terminal dock.
//! - [`overlays`] / [`pickers`] — modal boxes and floating lists.
//! - [`util`] — the shared chrome palette and cell/string helpers.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;

use editor_core::Document;

use crate::app::App;

mod chrome;
mod editor;
mod overlays;
mod panel;
mod pickers;
mod settings;
mod sidebar;
mod tabview;
mod util;
mod breadcrumb;

#[cfg(test)]
pub(crate) use chrome::fit_left;
pub(crate) use settings::settings_entry_at;
pub(crate) use tabview::viewer_body_rows;

use breadcrumb::render_breadcrumb;
use chrome::{render_status, render_tabs};
use editor::render_editor;
use overlays::{render_context_menu, render_overlay, render_prompt};
use panel::render_dock;
use pickers::{render_bottom_panel, render_completion, render_picker};
use settings::render_settings;
use sidebar::render_sidebar;
use tabview::render_tab_view;

/// Draw one full frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [tabs_area, body, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    // Split the shared bottom dock off the bottom of the body (full width, below the editor).
    let panel_rows = dock_rows(app, body.height);
    let (main_body, panel_area) = if panel_rows > 0 {
        let [main, panel] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(panel_rows)]).areas(body);
        (main, Some(panel))
    } else {
        (body, None)
    };

    // Remember the editor viewport height for PageUp/PageDown next tick. Mirror it onto
    // `EditorState` too, so the `vim` plugin can read it through `Host::viewport_height`.
    app.page_height = main_body.height.saturating_sub(0) as usize;
    app.editor.page_height = app.page_height;

    let (editor_area, sidebar_area, sidebar_inner, sidebar_first_row) =
        if app.editor.sidebar_visible {
            let [sidebar, editors] = Layout::horizontal([
                Constraint::Length(app.editor.sidebar_width),
                Constraint::Min(0),
            ])
            .areas(main_body);
            let (inner, first_row) = render_sidebar(f, app, sidebar);
            (editors, Some(sidebar), Some(inner), first_row)
        } else {
            (main_body, None, None, 0)
        };

    render_tabs(f, app, tabs_area);

    // Optional breadcrumb strip under the tabs (when LSP symbols are cached for the active doc).
    let (breadcrumb_area, editor_body) = {
        let want = app
            .editor
            .workspace
            .active_doc()
            .and_then(|id| app.editor.doc_symbols.get(&id))
            .is_some_and(|s| !s.is_empty());
        if want && editor_area.height > 1 {
            let [crumb, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(editor_area);
            if render_breadcrumb(f, app, crumb) {
                (Some(crumb), rest)
            } else {
                (None, editor_area)
            }
        } else {
            (None, editor_area)
        }
    };

    app.editor.ensure_splits();
    let mut editor_panes: Vec<(Vec<bool>, Rect)> = Vec::new();
    if app.settings_active() {
        render_settings(f, app, editor_body);
        editor_panes.push((Vec::new(), editor_body));
    } else if let Some(view) = app.editor.active_tab_view() {
        // A notice / plugin-viewer tab replaces the text pane (it has no text to draw).
        render_tab_view(f, app, editor_body, view);
        editor_panes.push((Vec::new(), editor_body));
    } else {
        // Lay out split panes; only the focused pane shows the hardware cursor.
        let layout = app
            .editor
            .splits
            .as_ref()
            .map(|t| t.layout(editor_body))
            .unwrap_or_else(|| vec![(Vec::new(), editor_body)]);
        editor_panes = layout.clone();
        let focus = app.editor.split_focus.clone();
        for (path, rect) in &layout {
            let focused = *path == focus;
            // Mirror this pane's doc+view for rendering.
            if let Some(tree) = &app.editor.splits {
                let pane = tree.focused_pane(path);
                if let Some(idx) = app.editor.workspace.tabs.iter().position(|&t| t == pane.doc) {
                    // Temporarily point active tab at this pane's doc for render helpers that
                    // read `active_document`. Restored after the loop via apply_focused_pane_view.
                    app.editor.workspace.active_tab = idx;
                }
                if let Some(doc) = app.editor.workspace.documents.get_mut(pane.doc) {
                    doc.view.scroll_line = pane.view.scroll_line;
                    doc.view.scroll_col = pane.view.scroll_col;
                    doc.view.scroll_sub = pane.view.scroll_sub;
                }
            }
            // Suppress cursor on non-focused panes by temporarily clearing Editor focus.
            let prev_focus = app.editor.focus;
            if !focused {
                // Keep focus as Editor but render_editor checks focus for cursor — we pass a flag
                // via temporarily setting focus away only when drawing non-focused panes.
                app.editor.focus = crate::editor::Focus::Sidebar;
            }
            render_editor(f, app, *rect);
            app.editor.focus = prev_focus;
        }
        app.editor.apply_focused_pane_view();
    }
    let lsp_status = render_status(f, app, status_area);

    // The dock draws after the editor so its cursor wins when the terminal tab is focused.
    let (panel_header, panel_content, lsp_content) = match panel_area {
        Some(panel) => render_dock(f, app, panel),
        None => (None, None, None),
    };

    // Overlays draw last, on top of the body above the dock (plan §4).
    render_completion(f, app, editor_body);
    render_prompt(f, app, editor_body);
    let bottom_panel = render_bottom_panel(f, app, main_body);
    render_picker(f, app, main_body);
    render_overlay(f, app, main_body);
    // The context menu draws last (on top) and hands back its per-item rects for click routing.
    let context_menu = render_context_menu(f, app, main_body);

    // Record laid-out regions so the mouse router (which runs outside draw) can hit-test.
    app.regions = Regions {
        tabs: tabs_area,
        sidebar: sidebar_area,
        sidebar_inner,
        sidebar_first_row,
        editor: editor_body,
        editor_panes,
        breadcrumb: breadcrumb_area,
        panel_header,
        panel_content,
        lsp_content,
        lsp_status,
        context_menu,
        bottom_panel,
    };
}

/// Rows the shared bottom dock occupies in the body: 0 when no tab is open, 1 when the active tab
/// is minimized (header only), else `height + 1` (tab strip + content), always leaving at least one
/// row for the editor.
fn dock_rows(app: &App, body_height: u16) -> u16 {
    if app.dock_active_tab().is_none() || body_height <= 1 {
        0
    } else if app.dock_minimized() {
        1
    } else {
        (app.editor.terminal_height + 1).min(body_height.saturating_sub(1))
    }
}

/// Screen regions from the last frame, for mouse hit-testing.
#[derive(Debug, Clone, Default)]
pub struct Regions {
    pub tabs: Rect,
    /// The full sidebar region (block + title + border) — used to detect sidebar clicks.
    pub sidebar: Option<Rect>,
    /// The sidebar's inner content region (panel rows), below the title. Row hit-testing
    /// maps against this, not `sidebar`, so clicks land on the row actually drawn there.
    pub sidebar_inner: Option<Rect>,
    /// Index of the sidebar panel's first *drawn* row. The panel scrolls, so a click's row
    /// offset within `sidebar_inner` is relative to this, not to the panel's row 0.
    pub sidebar_first_row: usize,
    /// Full editor area (union of all split panes) — used for coarse hit-tests / overlays.
    pub editor: Rect,
    /// Per-pane editor rects with their split focus path (for click-to-focus).
    pub editor_panes: Vec<(Vec<bool>, Rect)>,
    /// Breadcrumb strip rect (1 row under tabs), when shown.
    pub breadcrumb: Option<Rect>,
    /// The dock's header (tab strip) row, when the dock is open.
    pub panel_header: Option<Rect>,
    /// The terminal tab's content region (the active shell's grid), when it is the expanded tab.
    pub panel_content: Option<Rect>,
    /// The LSP tab's content region (the scrollable status list), when it is the expanded tab.
    pub lsp_content: Option<Rect>,
    /// The footer LSP indicator's clickable region (click → toggle the LSP panel).
    pub lsp_status: Option<Rect>,
    /// The right-click context menu's per-item click rects (top to bottom), when it is open.
    pub context_menu: Option<Vec<Rect>>,
    /// The bottom results dock's content region and the index of its first visible row, when a
    /// contributed bottom panel is showing. Row hit-testing needs both: the panel scrolls, so the
    /// rect alone cannot say which row a click landed on.
    pub bottom_panel: Option<(Rect, usize)>,
}

/// Gutter width for a document (digits + one padding space). Shared with the mouse router.
pub fn gutter_width(doc: &Document) -> u16 {
    let digits = ((doc.len_lines().max(1)) as f64).log10().floor() as u16 + 1;
    digits.max(3) + 1
}
