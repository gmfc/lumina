//! Floating list overlays: the completion popup, the project-search results panel, and the
//! fuzzy picker (command palette / quick open / goto line).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::picker::PickerKind;

use super::gutter_width;
use super::util::{put_str, CLR_ACCENT, CLR_SEL};

/// The completion popup: a caret-anchored floating list (plan §2.1). Positioned via
/// `char_to_screen` on the popup anchor, below the caret line (flipped above when it would
/// overflow the pane), scrolled to keep the selection visible.
pub(super) fn render_completion(f: &mut Frame, app: &App, editor_area: Rect) {
    let Some(comp) = &app.editor.completion else {
        return;
    };
    if comp.filtered.is_empty() || editor_area.width < 8 || editor_area.height < 2 {
        return;
    }
    let Some(doc) = app.editor.active_document() else {
        return;
    };
    let geo = editor_core::view::PaneGeometry {
        origin_x: editor_area.x,
        origin_y: editor_area.y,
        gutter: gutter_width(doc),
        scroll_line: doc.view.scroll_line,
        scroll_col: doc.view.scroll_col,
        tab_width: doc.tab_width,
        height: editor_area.height,
    };
    let Some((ax, ay)) = editor_core::view::char_to_screen(doc, &geo, comp.anchor) else {
        return;
    };

    // Scroll a window of rows so the selection is always visible.
    let max_rows = 8usize
        .min(editor_area.height.saturating_sub(1) as usize)
        .max(1);
    let total = comp.filtered.len();
    let offset = if comp.selected >= max_rows {
        comp.selected + 1 - max_rows
    } else {
        0
    };
    let end = (offset + max_rows).min(total);
    let shown = &comp.filtered[offset..end];

    // Width from the widest "kind label  detail" row, clamped to the pane.
    let mut width = 14usize;
    for &idx in shown {
        let it = &comp.items[idx];
        let w = 3
            + it.label.chars().count()
            + it.detail
                .as_ref()
                .map(|d| 2 + d.chars().count())
                .unwrap_or(0);
        width = width.max(w);
    }
    let width = (width + 1)
        .min(editor_area.width.saturating_sub(1) as usize)
        .max(4) as u16;

    let rows = shown.len() as u16;
    // Prefer below the anchor; flip above when it would overflow the pane bottom.
    let below = ay.saturating_add(1);
    let y = if below + rows <= editor_area.y + editor_area.height {
        below
    } else {
        ay.saturating_sub(rows)
    };
    let x = ax.min(editor_area.x + editor_area.width.saturating_sub(width));
    let rect = Rect::new(x, y, width, rows);
    f.render_widget(Clear, rect);

    let buf = f.buffer_mut();
    for (i, &idx) in shown.iter().enumerate() {
        let it = &comp.items[idx];
        let selected = offset + i == comp.selected;
        let (fg, bg) = if selected {
            (Color::Black, CLR_ACCENT)
        } else {
            (Color::Gray, Color::Rgb(40, 44, 52))
        };
        let kind = crate::completion::kind_label(it.kind);
        let mut text = format!("{kind} {}", it.label);
        if let Some(d) = &it.detail {
            text.push_str("  ");
            text.push_str(d);
        }
        // Truncate then pad to the box width so the whole row carries the background.
        let mut s: String = text.chars().take(width as usize).collect();
        let len = s.chars().count();
        if len < width as usize {
            s.push_str(&" ".repeat(width as usize - len));
        }
        put_str(
            buf,
            x,
            y + i as u16,
            &s,
            Style::default().fg(fg).bg(bg),
            x + width,
        );
    }
}

/// Project-search results: a bottom panel with the query line and grouped hits.
pub(super) fn render_search(f: &mut Frame, app: &App, body: Rect) {
    let Some(search) = app.search() else {
        return;
    };
    let height = (body.height / 2).max(6).min(body.height);
    let rect = Rect::new(body.x, body.y + body.height - height, body.width, height);
    f.render_widget(Clear, rect);
    let status = if search.running {
        "searching…".to_string()
    } else {
        format!("{} result(s)", search.results.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_ACCENT))
        .title(TSpan::styled(
            format!(" Search: {}  [{status}] ", search.query),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(28, 30, 36)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let visible = inner.height as usize;
    let start = search.selected.saturating_sub(visible.saturating_sub(1));
    let mut lines = Vec::new();
    let mut last_file: Option<&std::path::Path> = None;
    for (i, hit) in search.results.iter().enumerate().skip(start).take(visible) {
        // Group header when the file changes.
        if last_file != Some(hit.path.as_path()) {
            last_file = Some(hit.path.as_path());
            let name = hit
                .path
                .strip_prefix(&app.editor.workspace.root)
                .unwrap_or(&hit.path)
                .to_string_lossy()
                .into_owned();
            lines.push(Line::from(TSpan::styled(
                name,
                Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
            )));
        }
        let selected = i == search.selected;
        let style = if selected {
            Style::default().fg(Color::White).bg(CLR_SEL)
        } else {
            Style::default().fg(Color::Gray)
        };
        let text: String = hit.text.chars().take(120).collect();
        lines.push(Line::from(vec![
            TSpan::styled(
                format!("  {:>4}: ", hit.line),
                Style::default().fg(Color::DarkGray),
            ),
            TSpan::styled(text, style),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The fuzzy picker overlay (command palette / quick open / goto line): a centered box
/// with a query line and a ranked, scrollable result list.
pub(super) fn render_picker(f: &mut Frame, app: &App, body: Rect) {
    let Some(picker) = &app.editor.picker else {
        return;
    };
    let width = 72u16.min(body.width.saturating_sub(4)).max(20);
    let max_rows = 12u16;
    let list_rows = (picker.filtered.len() as u16).min(max_rows);
    let height = (list_rows + 3).min(body.height);
    let rect = Rect::new(
        body.x + (body.width.saturating_sub(width)) / 2,
        body.y + 1,
        width,
        height,
    );
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_ACCENT))
        .title(TSpan::styled(
            format!(" {} ", picker.prompt_label()),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(Color::Rgb(30, 33, 39)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    // Query line. A subtle hint reminds the user that `>` switches to commands.
    let cursor = "›";
    let mut query_spans = vec![
        TSpan::styled(format!("{cursor} "), Style::default().fg(CLR_ACCENT)),
        TSpan::styled(
            format!("{}▏", picker.query),
            Style::default().fg(Color::White),
        ),
    ];
    if picker.query.is_empty() && picker.kind == PickerKind::File {
        query_spans.push(TSpan::styled(
            "  (type > for commands)",
            Style::default().fg(Color::DarkGray),
        ));
    }
    let mut lines = vec![Line::from(query_spans), Line::from("")];

    // Scroll the result window to keep the selection visible.
    let active = picker.active_items();
    let visible = inner.height.saturating_sub(2) as usize;
    let start = picker.selected.saturating_sub(visible.saturating_sub(1));
    for (row_idx, &item_idx) in picker.filtered.iter().enumerate().skip(start).take(visible) {
        let item = &active[item_idx];
        let selected = row_idx == picker.selected;
        let style = if selected {
            Style::default().fg(Color::White).bg(CLR_SEL)
        } else {
            Style::default().fg(Color::Gray)
        };
        let prefix = if selected { "▸ " } else { "  " };
        lines.push(Line::from(TSpan::styled(
            format!("{prefix}{}", item.label),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
