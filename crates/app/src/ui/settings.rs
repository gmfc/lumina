//! Renders the Settings tab: a scrollable list of sections and typed-widget rows, a
//! footer describing the focused setting, and a floating dropdown when one is open.
//! The layout is shared with the mouse hit-tester ([`settings_entry_at`]) so clicks
//! land on the row actually drawn.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Clear;
use ratatui::Frame;

use crate::app::App;
use crate::settings::{Entry, SettingsView, Widget};

use super::util::{put_str, CLR_ACCENT, CLR_SEL};

const HEADER_ROWS: u16 = 2;
const FOOTER_ROWS: u16 = 2;
const LABEL_COL: usize = 30;

/// The scrollable list region (below the title, above the footer).
fn list_area(area: Rect) -> Rect {
    let y = area.y + HEADER_ROWS;
    let h = area.height.saturating_sub(HEADER_ROWS + FOOTER_ROWS);
    Rect::new(area.x, y, area.width, h)
}

/// The first visible entry index, auto-scrolled to keep `selected` roughly centered.
fn scroll_offset(view: &SettingsView, rows: usize) -> usize {
    let total = view.entries.len();
    if total <= rows || rows == 0 {
        0
    } else {
        view.selected.saturating_sub(rows / 2).min(total - rows)
    }
}

/// The entry index drawn at absolute screen `row`, if any (for mouse hit-testing).
pub(crate) fn settings_entry_at(view: &SettingsView, area: Rect, row: u16) -> Option<usize> {
    let la = list_area(area);
    if la.height == 0 || row < la.y || row >= la.y + la.height {
        return None;
    }
    let rows = la.height as usize;
    let scroll = scroll_offset(view, rows);
    let idx = scroll + (row - la.y) as usize;
    (idx < view.entries.len()).then_some(idx)
}

/// The display string for a widget's value (a `‹ … ›` stepper look for adjustables).
fn widget_text(w: &Widget) -> String {
    match w {
        Widget::Toggle(true) => "[✓]".into(),
        Widget::Toggle(false) => "[ ]".into(),
        Widget::Select { options, selected } => {
            format!(
                "‹ {} ›",
                options.get(*selected).map(String::as_str).unwrap_or("")
            )
        }
        Widget::Number { value, .. } => format!("‹ {value} ›"),
        Widget::Text(s) => {
            if s.is_empty() {
                "(default)".into()
            } else {
                format!("\"{s}\"")
            }
        }
    }
}

pub(super) fn render_settings(f: &mut Frame, app: &App, area: Rect) {
    let Some(view) = &app.settings else {
        return;
    };
    if area.width < 8 || area.height < 6 {
        return;
    }
    let bg = Style::default().bg(Color::Rgb(24, 26, 31));
    // Paint the whole pane background.
    f.render_widget(Clear, area);
    let buf = f.buffer_mut();
    for y in area.y..area.y + area.height {
        put_str(
            buf,
            area.x,
            y,
            &" ".repeat(area.width as usize),
            bg,
            area.x + area.width,
        );
    }

    // Title.
    put_str(
        buf,
        area.x + 1,
        area.y,
        "⚙  Settings",
        bg.fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
        area.x + area.width,
    );

    // List.
    let la = list_area(area);
    let rows = la.height as usize;
    let scroll = scroll_offset(view, rows);
    let max_x = area.x + area.width;
    for (i, entry) in view.entries.iter().enumerate().skip(scroll).take(rows) {
        let y = la.y + (i - scroll) as u16;
        match entry {
            Entry::Header(title) => {
                put_str(
                    buf,
                    area.x + 1,
                    y,
                    &title.to_uppercase(),
                    bg.fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
                    max_x,
                );
            }
            Entry::Item(item) => {
                let selected = i == view.selected;
                let row_style = if selected {
                    Style::default().bg(CLR_SEL).fg(Color::White)
                } else {
                    bg.fg(Color::Gray)
                };
                // Fill the row so the selection bar spans the pane.
                put_str(
                    buf,
                    area.x,
                    y,
                    &" ".repeat(area.width as usize),
                    row_style,
                    max_x,
                );
                let marker = if selected { "▸ " } else { "  " };
                let mut label = format!("{marker}{}", item.label);
                let w = label.chars().count();
                if w < LABEL_COL {
                    label.push_str(&" ".repeat(LABEL_COL - w));
                }
                // The value, or the live edit buffer for the focused field.
                let value = match (selected, &view.editing) {
                    (true, Some(buf)) => format!("{buf}▏"),
                    _ => widget_text(&item.widget),
                };
                let val_style = if selected {
                    row_style.add_modifier(Modifier::BOLD)
                } else {
                    row_style.fg(CLR_ACCENT)
                };
                put_str(buf, area.x + 3, y, &label, row_style, max_x);
                put_str(
                    buf,
                    area.x + 3 + LABEL_COL as u16,
                    y,
                    &value,
                    val_style,
                    max_x,
                );
            }
        }
    }

    // Footer: the focused setting's description + the key hints.
    let footer_y = area.y + area.height - 1;
    let desc = view
        .selected_item()
        .map(|it| it.description.clone())
        .unwrap_or_default();
    put_str(
        buf,
        area.x + 1,
        footer_y - 1,
        &desc,
        bg.fg(Color::Gray),
        max_x,
    );
    put_str(
        buf,
        area.x + 1,
        footer_y,
        "↑↓ move · Space/Enter toggle/edit · ←→ adjust · saved to config.toml",
        bg.fg(Color::DarkGray),
        max_x,
    );

    // Dropdown overlay for an open Select.
    if let (Some(d), Some(item)) = (view.dropdown, view.selected_item()) {
        if let Widget::Select { options, .. } = &item.widget {
            render_dropdown(buf, view, &la, scroll, options, d, max_x);
        }
    }
}

/// Draw the open dropdown's option list just below the focused row.
fn render_dropdown(
    buf: &mut ratatui::buffer::Buffer,
    view: &SettingsView,
    la: &Rect,
    scroll: usize,
    options: &[String],
    highlighted: usize,
    max_x: u16,
) {
    let row_in_list = view.selected.saturating_sub(scroll) as u16;
    let base_y = la.y + row_in_list + 1;
    let width = options
        .iter()
        .map(|o| o.chars().count() + 4)
        .max()
        .unwrap_or(6)
        .max(8) as u16;
    let x = la.x + 3 + LABEL_COL as u16;
    for (i, opt) in options.iter().enumerate() {
        let y = base_y + i as u16;
        if y >= la.y + la.height {
            break;
        }
        let sel = i == highlighted;
        let style = if sel {
            Style::default().bg(CLR_ACCENT).fg(Color::Black)
        } else {
            Style::default().bg(Color::Rgb(44, 48, 56)).fg(Color::White)
        };
        let mut s = format!("  {opt}");
        let w = s.chars().count();
        if w < width as usize {
            s.push_str(&" ".repeat(width as usize - w));
        }
        put_str(buf, x, y, &s, style, max_x.min(x + width));
    }
}
