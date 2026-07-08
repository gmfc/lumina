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
pub(super) fn render_sidebar(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let focused = app.editor.focus == Focus::Sidebar;
    let border_style = if focused {
        Style::default().fg(CLR_ACCENT)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(border_style)
        .title(TSpan::styled(
            " EXPLORER ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Prefer a plugin-contributed panel (Phase 4+). Fall back to a root hint.
    if let Some(panel) = app.editor.panels.get("explorer.tree") {
        let lines: Vec<Line> = panel
            .lines
            .iter()
            .enumerate()
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
                "explorer plugin not loaded",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(hint), inner);
    }
    inner
}

fn style_for(key: &str) -> Style {
    match key {
        "dir" => Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
        "file" => Style::default().fg(Color::Gray),
        "match" => Style::default().fg(Color::Yellow),
        "dim" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::White),
    }
}
