//! Rendering for the shared bottom dock: the tab strip header plus the active tab's content —
//! either the terminal's `vt100` grid or the LSP servers list.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::app::App;
use crate::editor::{DockTab, Focus};
use crate::lsp::LangState;
use crate::terminal::HeaderHit;

use super::util::{cell_at, put_str, CLR_ACCENT};

/// Render the shared bottom dock (tab strip + the active tab's content). Returns `(header,
/// terminal_content, lsp_content)` so the mouse router can hit-test exactly what was drawn; the two
/// content rects are mutually exclusive (only the active tab draws content).
pub(super) fn render_dock(
    f: &mut Frame,
    app: &App,
    area: Rect,
) -> (Option<Rect>, Option<Rect>, Option<Rect>) {
    if area.height == 0 || area.width == 0 {
        return (None, None, None);
    }
    let header = Rect::new(area.x, area.y, area.width, 1);
    render_dock_header(f, app, header);
    if app.dock_minimized() || area.height <= 1 {
        return (Some(header), None, None);
    }
    let content = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    match app.dock_active_tab() {
        Some(DockTab::Lsp) => {
            render_lsp_panel(f, app, content);
            (Some(header), None, Some(content))
        }
        _ => {
            render_terminal_content(f, app, content);
            (Some(header), Some(content), None)
        }
    }
}

/// The dock header: the minimize control, the `Terminal`/`LSP` tab buttons, and — when the terminal
/// tab is showing — its per-session tabs + `+`.
fn render_dock_header(f: &mut Frame, app: &App, area: Rect) {
    let dock_focused = matches!(app.editor.focus, Focus::Panel | Focus::LspPanel);
    let active_tab = app.dock_active_tab();
    let active_session = app.editor.terminal_view.active;
    let bg = Style::default().bg(Color::Rgb(30, 33, 39));
    let buf = f.buffer_mut();
    for x in area.x..area.right() {
        if let Some(cell) = cell_at(buf, x, area.y) {
            cell.set_char(' ');
            cell.set_style(bg);
        }
    }
    let mut x = area.x;
    for (label, hit) in app.dock_header_segments() {
        if x >= area.right() {
            break;
        }
        let style = header_seg_style(hit, active_tab, active_session, dock_focused);
        put_str(buf, x, area.y, &label, style, area.right());
        x = x.saturating_add(label.chars().count() as u16);
    }
}

/// Style for one header segment: the active dock tab and the active terminal session tab are
/// highlighted (accented when the dock is focused).
fn header_seg_style(
    hit: HeaderHit,
    active_tab: Option<DockTab>,
    active_session: usize,
    focused: bool,
) -> Style {
    let is_active = match hit {
        HeaderHit::DockTab(tab) => Some(tab) == active_tab,
        HeaderHit::Tab(i) => i == active_session,
        _ => false,
    };
    if is_active {
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
    } else if matches!(hit, HeaderHit::DockTab(_) | HeaderHit::Tab(_)) {
        Style::default().fg(Color::Gray).bg(Color::Rgb(30, 33, 39))
    } else {
        Style::default()
            .fg(Color::DarkGray)
            .bg(Color::Rgb(30, 33, 39))
    }
}

/// Render the LSP panel into `area`: a per-language status list (state glyph, language, resolved
/// command or install hint), and — when there are logs — a `server log` tail below a separator.
/// Empty state shows a hint. The status list is scrolled by `lsp_panel.scroll`.
fn render_lsp_panel(f: &mut Frame, app: &App, area: Rect) {
    let bg = Style::default().bg(Color::Rgb(24, 26, 31));
    let dim = bg.fg(Color::DarkGray);
    let buf = f.buffer_mut();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = cell_at(buf, x, y) {
                cell.set_char(' ');
                cell.set_style(bg);
            }
        }
    }
    let rows = app.lsp_status_rows();
    if rows.is_empty() {
        let hint = " No language servers active — open a source file to start one.";
        put_str(buf, area.x, area.y, hint, dim, area.right());
        return;
    }

    let total = area.height as usize;
    let logs = app.lsp_recent_logs(total);
    // When there are logs, split the panel: status list on top, a log tail below a separator.
    let rows_h = if logs.is_empty() {
        total
    } else {
        total.saturating_sub(1).div_ceil(2).max(1)
    };

    let scroll = app.editor.lsp_panel.scroll as usize;
    for (row, status) in rows.iter().skip(scroll).take(rows_h).enumerate() {
        let y = area.y + row as u16;
        let (glyph, glyph_style) = lang_glyph(status.state, bg);
        put_str(buf, area.x + 1, y, glyph, glyph_style, area.right());
        let lang = format!("{:<12}", status.lang);
        put_str(buf, area.x + 3, y, &lang, bg.fg(Color::White), area.right());
        put_str(
            buf,
            area.x + 16,
            y,
            &lang_detail(status),
            bg.fg(Color::Gray),
            area.right(),
        );
    }

    if logs.is_empty() {
        return;
    }
    let sep_y = area.y + rows_h as u16;
    put_str(buf, area.x, sep_y, " ── server log ", dim, area.right());
    let log_h = total.saturating_sub(rows_h + 1);
    let start = logs.len().saturating_sub(log_h);
    for (i, line) in logs[start..].iter().enumerate() {
        put_str(
            buf,
            area.x + 1,
            sep_y + 1 + i as u16,
            line,
            dim,
            area.right(),
        );
    }
}

/// The status glyph + its color for a language row.
fn lang_glyph(state: LangState, bg: Style) -> (&'static str, Style) {
    match state {
        LangState::Running => ("●", bg.fg(Color::Green)),
        LangState::Starting => ("◐", bg.fg(Color::Yellow)),
        LangState::Installed => ("○", bg.fg(Color::Gray)),
        LangState::NotInstalled => ("◌", bg.fg(Color::DarkGray)),
        LangState::Crashed => ("✗", bg.fg(Color::Red)),
    }
}

/// The right-hand detail text for a language row: state label + command / install hint / error.
fn lang_detail(status: &crate::lsp::LangStatus) -> String {
    match status.state {
        LangState::Running => format!("running   {}", status.command.as_deref().unwrap_or("")),
        LangState::Starting => format!("starting  {}", status.command.as_deref().unwrap_or("")),
        LangState::Installed => format!("ready     {}", status.command.as_deref().unwrap_or("")),
        LangState::NotInstalled => {
            format!(
                "not installed → {}",
                status.install.unwrap_or("(no known server)")
            )
        }
        LangState::Crashed => format!("crashed   {}", status.error.as_deref().unwrap_or("")),
    }
}

/// Render the active terminal's `vt100` grid into `area`, cell by cell, and place the hardware
/// cursor at the shell's cursor when the panel is focused.
fn render_terminal_content(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.editor.focus == Focus::Panel;
    let Some(term) = app.active_terminal() else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::LangStatus;

    fn row(state: LangState) -> LangStatus {
        LangStatus {
            lang: "rust".into(),
            state,
            command: Some("rust-analyzer".into()),
            install: Some("rustup component add rust-analyzer"),
            error: Some("boom".into()),
        }
    }

    #[test]
    fn lang_glyph_covers_every_state() {
        let bg = Style::default();
        assert_eq!(lang_glyph(LangState::Running, bg).0, "●");
        assert_eq!(lang_glyph(LangState::Starting, bg).0, "◐");
        assert_eq!(lang_glyph(LangState::Installed, bg).0, "○");
        assert_eq!(lang_glyph(LangState::NotInstalled, bg).0, "◌");
        assert_eq!(lang_glyph(LangState::Crashed, bg).0, "✗");
    }

    #[test]
    fn lang_detail_labels_each_state() {
        assert!(lang_detail(&row(LangState::Running)).starts_with("running"));
        assert!(lang_detail(&row(LangState::Running)).contains("rust-analyzer"));
        assert!(lang_detail(&row(LangState::Starting)).starts_with("starting"));
        assert!(lang_detail(&row(LangState::Installed)).starts_with("ready"));
        let not_installed = lang_detail(&row(LangState::NotInstalled));
        assert!(not_installed.contains("not installed"));
        assert!(not_installed.contains("rustup component add"));
        assert!(lang_detail(&row(LangState::Crashed)).contains("boom"));
    }
}
