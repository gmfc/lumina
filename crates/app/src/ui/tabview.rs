//! Renders the two kinds of non-text tab: the refusal **notice** and a plugin **viewer**.
//!
//! Both are pure functions of state (invariant #8) — a viewer publishes its rows through
//! `Host::set_viewer_content` and this only reads them, exactly as the sidebar reads a panel's.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::tabview::NOTICE_ACTIONS;
use crate::app::App;
use crate::editor::{TabView, ViewerTab};
use crate::files;

use super::sidebar::style_for;
use super::util::{display_len, put_str, CLR_ACCENT};

/// Rows a viewer spends on chrome before its body: the title and the status/separator row.
const VIEWER_HEADER_ROWS: u16 = 2;

/// How many body rows a viewer tab has in a pane `height` rows tall (at least 1, so paging
/// always makes progress). Shared with the input side so scroll clamping matches what's drawn.
pub(crate) fn viewer_body_rows(height: u16) -> usize {
    height.saturating_sub(VIEWER_HEADER_ROWS).max(1) as usize
}

pub(super) fn render_tab_view(f: &mut Frame, app: &App, area: Rect, view: &TabView) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match view {
        TabView::Notice { path, refusal } => render_notice(f, app, area, path, *refusal),
        TabView::Viewer(v) => render_viewer(f, area, v),
    }
}

/// The "lumina can't show this as text" screen: what the file is, how big, why it was refused,
/// and what the user can do about it.
fn render_notice(
    f: &mut Frame,
    app: &App,
    area: Rect,
    path: &std::path::Path,
    refusal: files::Refusal,
) {
    let accent = Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let mut rows: Vec<Line> = vec![
        Line::from(TSpan::styled(refusal.label().to_string(), accent)),
        Line::from(""),
        Line::from(TSpan::styled(
            format!("{name} · {}", files::human_bytes(refusal.len())),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(TSpan::styled(reason(&refusal), dim)),
        Line::from(""),
    ];

    // Offer only actions that exist: `view.openAsHex` disappears with its plugin, and
    // `file.openAnyway` is meaningless for a binary that can't round-trip through the rope.
    let actions: Vec<(String, &str)> = NOTICE_ACTIONS
        .iter()
        .filter(|(id, _)| *id != "file.openAnyway" || refusal.is_overridable())
        .filter_map(|(id, label)| Some((app.keymap.binding_label(id)?, *label)))
        .collect();
    if !actions.is_empty() {
        let key_col = actions
            .iter()
            .map(|(k, _)| display_len(k))
            .max()
            .unwrap_or(0);
        for (keys, label) in &actions {
            let pad = " ".repeat(key_col - display_len(keys));
            rows.push(Line::from(vec![
                TSpan::styled(keys.clone(), Style::default().fg(CLR_ACCENT)),
                TSpan::raw(format!("{pad}  ")),
                TSpan::styled((*label).to_string(), Style::default().fg(Color::Gray)),
            ]));
        }
    }

    // Vertically centered, like the welcome screen.
    let top = area.height.saturating_sub(rows.len() as u16) / 2;
    let body = Rect::new(
        area.x,
        area.y + top,
        area.width,
        area.height.saturating_sub(top),
    );
    f.render_widget(
        Paragraph::new(rows).alignment(ratatui::layout::Alignment::Center),
        body,
    );
}

/// One sentence explaining the refusal, in the user's terms.
fn reason(refusal: &files::Refusal) -> String {
    match refusal {
        files::Refusal::Binary { .. } => {
            "lumina doesn't display this file as text — its bytes aren't text, and editing them \
             here would corrupt the file."
                .into()
        }
        files::Refusal::TooLarge { limit, .. } => format!(
            "Larger than the {} open limit (settings: max_file_size_mb).",
            files::human_bytes(*limit)
        ),
    }
}

/// A viewer tab: a title row, an optional status row, then the published rows.
fn render_viewer(f: &mut Frame, area: Rect, v: &ViewerTab) {
    let name = v
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rows = viewer_body_rows(area.height);
    let total = v.content.lines.len();
    let scroll = v.scroll.min(total.saturating_sub(1).max(0));

    let buf = f.buffer_mut();
    let max_x = area.x + area.width;

    // Header: "<Viewer Title> — <file name>", plus a scroll position when the body overflows.
    let header = format!("{} — {name}", v.title);
    put_str(
        buf,
        area.x,
        area.y,
        &header,
        Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
        max_x,
    );
    if total > rows {
        let pos = format!("{}–{} of {total}", scroll + 1, (scroll + rows).min(total));
        let x = max_x.saturating_sub(display_len(&pos) as u16);
        put_str(
            buf,
            x.max(area.x),
            area.y,
            &pos,
            Style::default().fg(Color::DarkGray),
            max_x,
        );
    }
    if let Some(status) = &v.content.status {
        put_str(
            buf,
            area.x,
            area.y + 1,
            status,
            Style::default().fg(Color::DarkGray),
            max_x,
        );
    }

    for row in 0..rows {
        let Some(line) = v.content.lines.get(scroll + row) else {
            break;
        };
        let y = area.y + VIEWER_HEADER_ROWS + row as u16;
        if y >= area.y + area.height {
            break;
        }
        let mut x = area.x + (line.depth * 2) as u16;
        for span in &line.spans {
            if x >= max_x {
                break;
            }
            put_str(buf, x, y, &span.text, style_for(&span.style), max_x);
            x += display_len(&span.text) as u16;
        }
    }
}
