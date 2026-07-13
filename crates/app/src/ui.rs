//! Rendering — a pure function of state (plan §4, invariant #8). No mutation of editor
//! state happens here; we only read it and write cells.
//!
//! The frame is assembled by [`draw`]; the pieces live in focused submodules:
//! - [`chrome`] — tab bar, status bar, welcome screen.
//! - [`editor`] — the text pane, its per-line loop, and per-cell decorations.
//! - [`sidebar`] — the explorer panel.
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
mod util;

pub(crate) use settings::settings_entry_at;

use chrome::{render_status, render_tabs};
use editor::render_editor;
use overlays::{render_overlay, render_prompt};
use panel::render_terminal_panel;
use pickers::{render_bottom_panel, render_completion, render_picker};
use settings::render_settings;
use sidebar::render_sidebar;

/// Draw one full frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [tabs_area, body, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    // Split the terminal dock off the bottom of the body (full width, below the editor).
    let panel_rows = terminal_panel_rows(app, body.height);
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

    let (editor_area, sidebar_area, sidebar_inner) = if app.editor.sidebar_visible {
        let [sidebar, editors] = Layout::horizontal([
            Constraint::Length(app.editor.sidebar_width),
            Constraint::Min(0),
        ])
        .areas(main_body);
        let inner = render_sidebar(f, app, sidebar);
        (editors, Some(sidebar), Some(inner))
    } else {
        (main_body, None, None)
    };

    render_tabs(f, app, tabs_area);
    if app.settings_active() {
        render_settings(f, app, editor_area);
    } else {
        render_editor(f, app, editor_area);
    }
    render_status(f, app, status_area);

    // The terminal dock draws after the editor so its cursor wins when the panel is focused.
    let (panel_header, panel_content) = match panel_area {
        Some(panel) => render_terminal_panel(f, app, panel),
        None => (None, None),
    };

    // Overlays draw last, on top of the body above the dock (plan §4).
    render_completion(f, app, editor_area);
    render_prompt(f, app, editor_area);
    render_bottom_panel(f, app, main_body);
    render_picker(f, app, main_body);
    render_overlay(f, app, main_body);

    // Record laid-out regions so the mouse router (which runs outside draw) can hit-test.
    app.regions = Regions {
        tabs: tabs_area,
        sidebar: sidebar_area,
        sidebar_inner,
        editor: editor_area,
        panel_header,
        panel_content,
    };
}

/// Rows the terminal dock occupies in the body: 0 when closed, 1 when minimized (header only),
/// else `height + 1` (header + content), always leaving at least one row for the editor.
fn terminal_panel_rows(app: &App, body_height: u16) -> u16 {
    let view = &app.editor.terminal_view;
    if !view.open || body_height <= 1 {
        0
    } else if view.minimized {
        1
    } else {
        (app.editor.terminal_height + 1).min(body_height.saturating_sub(1))
    }
}

/// Screen regions from the last frame, for mouse hit-testing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Regions {
    pub tabs: Rect,
    /// The full sidebar region (block + title + border) — used to detect sidebar clicks.
    pub sidebar: Option<Rect>,
    /// The sidebar's inner content region (panel rows), below the title. Row hit-testing
    /// maps against this, not `sidebar`, so clicks land on the row actually drawn there.
    pub sidebar_inner: Option<Rect>,
    pub editor: Rect,
    /// The terminal panel's header (tab bar) row, when the dock is open.
    pub panel_header: Option<Rect>,
    /// The terminal panel's content region (the active shell's grid), when expanded.
    pub panel_content: Option<Rect>,
}

/// Gutter width for a document (digits + one padding space). Shared with the mouse router.
pub fn gutter_width(doc: &Document) -> u16 {
    let digits = ((doc.len_lines().max(1)) as f64).log10().floor() as u16 + 1;
    digits.max(3) + 1
}
