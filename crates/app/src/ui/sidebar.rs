//! The explorer sidebar: renders a plugin-contributed tree panel (or a root hint when none is
//! loaded) and returns its inner region for mouse hit-testing.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::editor::Focus;

use super::util::{CLR_ACCENT, CLR_SEL};

/// Render the sidebar and return its inner content region (below the title), so the mouse
/// router can hit-test panel rows against the same geometry the rows were drawn into.
pub(super) fn render_sidebar(f: &mut Frame, app: &mut App, area: Rect) -> (Rect, usize) {
    let focused = app.editor.focus == Focus::Sidebar;
    let border_style = if focused {
        Style::default().fg(CLR_ACCENT)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    // The panel and its title both come from the contribution, not a literal: any plugin that
    // declares a `PanelLocation::Sidebar` panel gets drawn here (invariant #3).
    let title = app
        .active_sidebar_panel()
        .map(|p| format!(" {} ", p.title.to_uppercase()))
        .unwrap_or_else(|| " EXPLORER ".to_string());
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(border_style)
        .title(TSpan::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The panel scrolls: it had no offset at all, so a tree longer than the sidebar was simply
    // clipped and the selection could walk off the bottom with nothing on screen moving.
    let panel_id = app.active_sidebar_panel_id();
    let (rows, selected) = panel_id
        .as_ref()
        .and_then(|id| app.editor.panels.get(id))
        .map_or((0, 0), |p| (p.lines.len(), p.selected));
    app.reconcile_sidebar_scroll(rows, selected, inner.height as usize);
    let first_row = app.editor.sidebar_scroll;
    let content = panel_id.as_ref().and_then(|id| app.editor.panels.get(id));
    if let Some(panel) = content {
        let lines: Vec<Line> = panel
            .lines
            .iter()
            .enumerate()
            .skip(first_row)
            .take(inner.height as usize)
            .map(|(i, l)| {
                let mut spans: Vec<TSpan> = Vec::new();
                spans.push(TSpan::raw("  ".repeat(l.depth)));
                for s in &l.spans {
                    spans.push(TSpan::styled(s.text.clone(), style_for(&s.style)));
                }
                let mut line = Line::from(spans);
                if i == panel.selected && focused {
                    line = line.style(Style::default().bg(CLR_SEL));
                }
                line
            })
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
    } else {
        let root = app.editor.workspace.root.display().to_string();
        let hint = vec![
            Line::from(TSpan::styled(
                root,
                Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
            )),
            Line::from(""),
            Line::from(TSpan::styled(
                match app.active_sidebar_panel() {
                    Some(p) => format!("{} has published nothing yet", p.title),
                    None => "no sidebar panel is contributed".to_string(),
                },
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(hint), inner);
    }
    (inner, first_row)
}

/// Map a panel `Span` style key to a concrete style. Shared by the sidebar and the bottom
/// results panel so plugin-published panels render consistently.
pub(super) fn style_for(key: &str) -> Style {
    match key {
        "dir" => Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
        "file" => Style::default().fg(Color::Gray),
        "match" => Style::default().fg(Color::Yellow),
        "dim" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::White),
    }
}
