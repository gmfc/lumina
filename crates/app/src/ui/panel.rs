//! Rendering for the bottom terminal dock: the header (tab bar) and the active shell's
//! `vt100` grid, plus cursor placement.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::app::App;
use crate::editor::Focus;
use crate::terminal::HeaderHit;

use super::util::{cell_at, put_str, CLR_ACCENT};

/// Render the terminal dock (header row + the active shell's grid). Returns the header and
/// content regions so the mouse router can hit-test against exactly what was drawn.
pub(super) fn render_terminal_panel(
    f: &mut Frame,
    app: &App,
    area: Rect,
) -> (Option<Rect>, Option<Rect>) {
    if area.height == 0 || area.width == 0 {
        return (None, None);
    }
    let header = Rect::new(area.x, area.y, area.width, 1);
    render_terminal_header(f, app, header);
    if app.panel.minimized || area.height <= 1 {
        return (Some(header), None);
    }
    let content = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    render_terminal_content(f, app, content);
    (Some(header), Some(content))
}

/// The header: the minimize control, one segment per terminal tab, and a `+` to add one.
fn render_terminal_header(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.editor.focus == Focus::Panel;
    let bg = Style::default().bg(Color::Rgb(30, 33, 39));
    let active = app.panel.active;
    let buf = f.buffer_mut();
    // Paint the whole row with the header background first.
    for x in area.x..area.right() {
        if let Some(cell) = cell_at(buf, x, area.y) {
            cell.set_char(' ');
            cell.set_style(bg);
        }
    }
    let mut x = area.x;
    for (label, hit) in app.panel.header_segments() {
        if x >= area.right() {
            break;
        }
        let style = header_seg_style(hit, active, focused);
        put_str(buf, x, area.y, &label, style, area.right());
        x = x.saturating_add(label.chars().count() as u16);
    }
}

/// Style for one header segment (active tab highlighted; accented when the panel is focused).
fn header_seg_style(hit: HeaderHit, active: usize, focused: bool) -> Style {
    match hit {
        HeaderHit::Tab(i) if i == active => {
            if focused {
                Style::default()
                    .fg(Color::Black)
                    .bg(CLR_ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(55, 58, 66))
                    .add_modifier(Modifier::BOLD)
            }
        }
        HeaderHit::Tab(_) => Style::default().fg(Color::Gray).bg(Color::Rgb(30, 33, 39)),
        _ => Style::default()
            .fg(Color::DarkGray)
            .bg(Color::Rgb(30, 33, 39)),
    }
}

/// Render the active terminal's `vt100` grid into `area`, cell by cell, and place the hardware
/// cursor at the shell's cursor when the panel is focused.
fn render_terminal_content(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.editor.focus == Focus::Panel;
    let Some(term) = app.panel.active_terminal() else {
        fill_blank(f.buffer_mut(), area);
        return;
    };
    let screen = term.screen();
    // Only show the cursor at the live view: `cursor_position()` is the live cursor, which does
    // not correspond to the grid the user sees while scrolled into history.
    let show_cursor = focused && term.at_live();
    draw_terminal_grid(f.buffer_mut(), screen, area);
    place_terminal_cursor(f, screen, area, show_cursor);
}

/// Fill `area` with blank cells (the panel is open but has no active shell).
fn fill_blank(buf: &mut ratatui::buffer::Buffer, area: Rect) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = cell_at(buf, x, y) {
                cell.set_char(' ');
                cell.set_style(Style::default());
            }
        }
    }
}

/// Draw a `vt100` screen grid into `area`, one buffer cell per grid cell.
fn draw_terminal_grid(buf: &mut ratatui::buffer::Buffer, screen: &vt100::Screen, area: Rect) {
    let (grid_rows, grid_cols) = screen.size();
    for row in 0..area.height.min(grid_rows) {
        for col in 0..area.width.min(grid_cols) {
            let Some(src) = screen.cell(row, col) else {
                continue;
            };
            let Some(dst) = cell_at(buf, area.x + col, area.y + row) else {
                continue;
            };
            dst.set_char(src.contents().chars().next().unwrap_or(' '));
            dst.set_style(terminal_cell_style(src));
        }
    }
}

/// Place the hardware cursor at the shell's cursor position when `show` (focused, live view).
fn place_terminal_cursor(f: &mut Frame, screen: &vt100::Screen, area: Rect, show: bool) {
    if !show || screen.hide_cursor() {
        return;
    }
    let (crow, ccol) = screen.cursor_position();
    if crow < area.height && ccol < area.width {
        f.set_cursor_position(Position::new(area.x + ccol, area.y + crow));
    }
}

/// Translate a `vt100` cell's colors and attributes into a ratatui style.
fn terminal_cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = crate::terminal::vt_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = crate::terminal::vt_color(cell.bgcolor()) {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}
