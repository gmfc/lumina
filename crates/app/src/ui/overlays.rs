//! Modal overlays drawn on top of the body: the confirm/hover/rename/save-as boxes and the
//! find/replace widget.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use editor_plugin::{Prompt, PromptPlacement};

use crate::app::App;
use crate::editor::Overlay;

use super::util::CLR_ACCENT;

pub(super) fn render_overlay(f: &mut Frame, app: &App, body: Rect) {
    let Some(overlay) = &app.editor.overlay else {
        return;
    };
    match overlay {
        Overlay::ConfirmClose { tab } => {
            let name = app
                .editor
                .workspace
                .tabs
                .get(*tab)
                .and_then(|&id| app.editor.workspace.documents.get(id))
                .and_then(|d| d.path.as_ref())
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into());
            let text = vec![
                Line::from(TSpan::styled(
                    format!(" {name} has unsaved changes"),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(" [S] Save & close   [D] Discard   [Esc] Cancel "),
            ];
            let rect = centered(body, 44, 5);
            f.render_widget(Clear, rect);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(CLR_ACCENT))
                .style(Style::default().bg(Color::Rgb(30, 33, 39)));
            f.render_widget(Paragraph::new(text).block(block), rect);
        }
        Overlay::Info(body_text) => {
            // A hover/info popup: wrap the text into a centered box, capped in size.
            let lines: Vec<Line> = body_text
                .lines()
                .take(body.height.saturating_sub(4) as usize)
                .map(|l| Line::from(l.to_string()))
                .collect();
            // On a very narrow terminal the available width can fall below the 20-col floor;
            // `usize::clamp` panics if `max < min`, so take the wider of the two as the ceiling.
            let max_w = (body.width.saturating_sub(8) as usize).max(20);
            let w = body_text
                .lines()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(20)
                .clamp(20, max_w) as u16;
            let h = (lines.len() as u16 + 2).min(body.height.saturating_sub(2));
            let rect = centered(body, w + 4, h);
            f.render_widget(Clear, rect);
            let block = Block::default()
                .title(" Hover ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(CLR_ACCENT))
                .style(Style::default().bg(Color::Rgb(30, 33, 39)));
            f.render_widget(Paragraph::new(lines).block(block), rect);
        }
        Overlay::RenameInput { buffer, .. } => {
            let text = vec![
                Line::from(TSpan::styled(
                    " Rename symbol",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!(" › {buffer}▏")),
                Line::from(TSpan::styled(
                    " [Enter] Apply   [Esc] Cancel ",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let rect = centered(body, 44, 6);
            f.render_widget(Clear, rect);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(CLR_ACCENT))
                .style(Style::default().bg(Color::Rgb(30, 33, 39)));
            f.render_widget(Paragraph::new(text).block(block), rect);
        }
        Overlay::SaveAsInput { buffer } => {
            let text = vec![
                Line::from(TSpan::styled(
                    " Save As",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!(" › {buffer}▏")),
                Line::from(TSpan::styled(
                    " [Enter] Save   [Esc] Cancel ",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let rect = centered(body, 60, 6);
            f.render_widget(Clear, rect);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(CLR_ACCENT))
                .style(Style::default().bg(Color::Rgb(30, 33, 39)));
            f.render_widget(Paragraph::new(text).block(block), rect);
        }
    }
}

/// A generic modal input prompt (find/replace today) — a pure function of `app.editor.prompt`.
/// The owning plugin publishes the [`Prompt`]; the app just draws it here.
pub(super) fn render_prompt(f: &mut Frame, app: &App, editor_area: Rect) {
    let Some(prompt) = &app.editor.prompt else {
        return;
    };
    match prompt.placement {
        PromptPlacement::TopRight => render_prompt_top_right(f, prompt, editor_area),
        PromptPlacement::Center => render_prompt_centered(f, prompt, editor_area),
    }
}

/// The find/replace shape: a top-right overlay (VS Code-shaped) with toggles + a count.
fn render_prompt_top_right(f: &mut Frame, prompt: &Prompt, editor_area: Rect) {
    let height = if prompt.fields.len() >= 2 { 4 } else { 3 };
    let width = 46u16.min(editor_area.width);
    let rect = Rect::new(
        editor_area.x + editor_area.width.saturating_sub(width),
        editor_area.y,
        width,
        height.min(editor_area.height),
    );
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_ACCENT))
        .style(Style::default().bg(Color::Rgb(30, 33, 39)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line> = prompt
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let label_fg = if i == prompt.focused {
                Color::White
            } else {
                Color::Gray
            };
            Line::from(vec![
                TSpan::styled(format!("{} ", field.label), Style::default().fg(label_fg)),
                TSpan::styled(
                    format!("{}▏", field.value),
                    Style::default().fg(Color::White),
                ),
            ])
        })
        .collect();

    let toggle = |on: bool, label: &str| {
        let style = if on {
            Style::default().fg(Color::Black).bg(CLR_ACCENT)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        TSpan::styled(format!(" {label} "), style)
    };
    let mut toggle_row: Vec<TSpan> = Vec::new();
    for (i, tog) in prompt.toggles.iter().enumerate() {
        if i > 0 {
            toggle_row.push(TSpan::raw(" "));
        }
        toggle_row.push(toggle(tog.on, &tog.label));
    }
    if let Some(status) = &prompt.status {
        toggle_row.push(TSpan::styled(
            format!(" {status} "),
            Style::default().fg(Color::Gray),
        ));
    }
    lines.push(Line::from(toggle_row));
    if let Some(err) = &prompt.error {
        lines.push(Line::from(TSpan::styled(
            format!(" {err}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// A centered single-column prompt (title + fields + footer hint) for rename/save-as-shaped
/// inputs. Currently unused by any migrated feature, but keeps the port complete.
fn render_prompt_centered(f: &mut Frame, prompt: &Prompt, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(title) = &prompt.title {
        lines.push(Line::from(TSpan::styled(
            format!(" {title}"),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
    }
    for field in &prompt.fields {
        lines.push(Line::from(format!(" › {}▏", field.value)));
    }
    if let Some(err) = &prompt.error {
        lines.push(Line::from(TSpan::styled(
            format!(" {err}"),
            Style::default().fg(Color::Red),
        )));
    }
    if let Some(footer) = &prompt.footer {
        lines.push(Line::from(TSpan::styled(
            format!(" {footer}"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = centered(area, 60, h);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_ACCENT))
        .style(Style::default().bg(Color::Rgb(30, 33, 39)));
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// A rectangle of `w`×`h` centered within `area` (clamped to fit).
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}
