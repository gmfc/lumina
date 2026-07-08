//! Frame chrome: the tab bar, the status bar, and the empty-state welcome screen.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;

use super::util::{diag_marker, display_len, CLR_ACCENT};

pub(super) fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
    let ws = &app.editor.workspace;
    let mut spans: Vec<TSpan> = Vec::new();
    if ws.tabs.is_empty() {
        spans.push(TSpan::styled(
            " lumina ",
            Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
        ));
    }
    for (i, &id) in ws.tabs.iter().enumerate() {
        let doc = &ws.documents[id];
        let name = doc
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into());
        let marker = if doc.external_conflict.is_some() {
            "⚠"
        } else if doc.dirty {
            "●"
        } else if doc.externally_reloaded {
            "↻"
        } else {
            "×"
        };
        let active = i == ws.active_tab;
        let style = if active {
            Style::default()
                .fg(Color::White)
                .bg(Color::Rgb(40, 44, 52))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(TSpan::styled(format!(" {name} {marker} "), style));
        spans.push(TSpan::raw(""));
    }
    let line = Line::from(spans);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::Rgb(30, 33, 39))),
        area,
    );
}

pub(super) fn render_welcome(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(TSpan::styled(
            "  lumina",
            Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(TSpan::styled(
            "  a mouse-first terminal code editor",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(TSpan::styled(
            "  Ctrl+P  open file      Ctrl+Shift+P  commands",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(TSpan::styled(
            "  Ctrl+B  toggle sidebar Ctrl+Q       quit",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

pub(super) fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let ws = &app.editor.workspace;
    let mut left;
    let mut right = String::new();

    if let Some(doc) = ws.active_document() {
        let head = doc.selections.primary().head;
        let (line, col) = doc.char_to_line_col(head);
        let name = doc
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".into());
        left = format!(" {name}{}", if doc.dirty { " ●" } else { "" });
        let le = match doc.line_ending {
            editor_core::LineEnding::Lf => "LF",
            editor_core::LineEnding::Crlf => "CRLF",
        };
        let enc = match doc.encoding {
            editor_core::Encoding::Utf8 => "UTF-8",
            editor_core::Encoding::Utf8Bom => "UTF-8-BOM",
            editor_core::Encoding::Utf16Le => "UTF-16 LE",
            editor_core::Encoding::Utf16Be => "UTF-16 BE",
        };
        let lang = doc.language.clone().unwrap_or_else(|| "text".into());
        right = format!("Ln {}, Col {}   {enc}  {le}  {lang} ", line + 1, col + 1);
    } else {
        left = " No file open".into();
    }

    if let Some(msg) = &app.editor.status_message {
        left = format!(" {msg}");
    } else if let Some((sev, msg)) = app.diagnostic_at_caret() {
        // The diagnostic under the caret, single-lined and truncated to fit (plan §2.2).
        let avail = (area.width as usize).saturating_sub(display_len(&right) + 4);
        let msg: String = msg.replace('\n', " ").chars().take(avail).collect();
        left = format!(" {} {msg}", diag_marker(sev));
    }

    let bg = Style::default().bg(CLR_ACCENT).fg(Color::Black);
    let pad = (area.width as usize).saturating_sub(display_len(&left) + display_len(&right));
    let line = Line::from(vec![
        TSpan::styled(left, bg),
        TSpan::styled(" ".repeat(pad), bg),
        TSpan::styled(right, bg),
    ]);
    f.render_widget(Paragraph::new(line).style(bg), area);
}
