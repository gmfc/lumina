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
        let name = if app.is_settings_doc(id) {
            "⚙ Settings".to_string()
        } else {
            doc.path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into())
        };
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

/// The `LUMINA` block-letter banner, drawn on the empty-state screen.
const BANNER: &[&str] = &[
    "██╗     ██╗   ██╗███╗   ███╗██╗███╗   ██╗ █████╗ ",
    "██║     ██║   ██║████╗ ████║██║████╗  ██║██╔══██╗",
    "██║     ██║   ██║██╔████╔██║██║██╔██╗ ██║███████║",
    "██║     ██║   ██║██║╚██╔╝██║██║██║╚██╗██║██╔══██║",
    "███████╗╚██████╔╝██║ ╚═╝ ██║██║██║ ╚██╗██║██║  ██║",
    "╚══════╝ ╚═════╝ ╚═╝     ╚═╝╚═╝╚═╝  ╚═╝╚═╝╚═╝  ╚═╝",
];

/// The commands surfaced on the empty-state screen: `(command id, label, fallback keys)`. The
/// key shown is looked up live from the active keymap so config remaps are reflected; the
/// fallback stands in only when nothing is bound. Every one is also in the command palette.
const WELCOME_COMMANDS: &[(&str, &str, &str)] = &[
    ("view.quickOpen", "Open File", "Ctrl+P"),
    ("view.commandPalette", "Command Palette", "Ctrl+Shift+P"),
    ("file.new", "New File", "Ctrl+N"),
    ("file.save", "Save", "Ctrl+S"),
    ("search.find", "Find", "Ctrl+F"),
    ("edit.toggleComment", "Toggle Comment", "Ctrl+/"),
    ("view.toggleSidebar", "Toggle Sidebar", "Ctrl+B"),
    ("terminal.toggle", "Toggle Terminal", "Ctrl+`"),
    ("cursor.addNextMatch", "Add Cursor / Next Match", "Ctrl+D"),
    ("lsp.gotoDefinition", "Go to Definition", "F12"),
    ("view.gotoLine", "Go to Line", "Ctrl+G"),
    ("app.quit", "Quit", "Ctrl+Q"),
];

pub(super) fn render_welcome(f: &mut Frame, app: &App, area: Rect) {
    let accent = Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD);
    let mut rows: Vec<Line> = Vec::new();

    // Resolve each command's current key from the live keymap (falling back to the default),
    // so a user's `[keys]` config overrides show through here too.
    let commands: Vec<(String, &str)> = WELCOME_COMMANDS
        .iter()
        .map(|(id, label, fallback)| {
            let keys = app
                .keymap
                .binding_label(id)
                .unwrap_or_else(|| (*fallback).to_string());
            (keys, *label)
        })
        .collect();

    // Banner — only when it fits the pane width; otherwise a plain wordmark stands in.
    let banner_w = BANNER.iter().map(|l| display_len(l)).max().unwrap_or(0);
    if (banner_w as u16) <= area.width {
        for l in BANNER {
            rows.push(Line::from(TSpan::styled(*l, accent)));
        }
    } else {
        rows.push(Line::from(TSpan::styled("lumina", accent)));
    }
    rows.push(Line::from(""));
    rows.push(Line::from(TSpan::styled(
        "a mouse-first terminal code editor",
        Style::default().fg(Color::Gray),
    )));
    rows.push(Line::from(""));

    // Command hints. Keys accented, labels dim, aligned on a grid. Two columns when the pane
    // is wide enough, otherwise a single column so nothing is clipped.
    let key_col = commands
        .iter()
        .map(|(k, _)| display_len(k))
        .max()
        .unwrap_or(0);
    let label_col = commands
        .iter()
        .map(|(_, l)| display_len(l))
        .max()
        .unwrap_or(0);
    let cell_w = key_col + 2 + label_col;
    let two_col = (2 * cell_w + 3) <= area.width as usize;
    // Every cell is padded to the same width, so all command rows share a width and their
    // left edges align once each row is centered.
    let cell = |(keys, label): &(String, &str)| {
        let kpad = " ".repeat(key_col - display_len(keys));
        vec![
            TSpan::styled(keys.clone(), accent),
            TSpan::raw(format!("{kpad}  ")),
            TSpan::styled(
                format!("{label:<w$}", w = label_col),
                Style::default().fg(Color::Gray),
            ),
        ]
    };
    let per_row = if two_col { 2 } else { 1 };
    for pair in commands.chunks(per_row) {
        let mut spans = Vec::new();
        for (i, entry) in pair.iter().enumerate() {
            if i > 0 {
                spans.push(TSpan::raw("   "));
            }
            spans.extend(cell(entry));
        }
        rows.push(Line::from(spans));
    }

    // Center the block vertically, and each row horizontally, within the pane.
    let content_h = rows.len() as u16;
    let top = area.height.saturating_sub(content_h) / 2;
    let mut out: Vec<Line> = Vec::with_capacity(rows.len() + top as usize);
    for _ in 0..top {
        out.push(Line::from(""));
    }
    for row in rows {
        let w: usize = row.spans.iter().map(|s| display_len(&s.content)).sum();
        let left = (area.width as usize).saturating_sub(w) / 2;
        let mut spans = vec![TSpan::raw(" ".repeat(left))];
        spans.extend(row.spans);
        out.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(out), area);
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

    // Vim mode badge (and any pending count/operator) at the far left.
    if let Some(vim) = &app.editor.vim {
        let mut badge = format!(" -- {} -- ", vim.mode.label());
        if let Some(hint) = vim.pending_hint() {
            badge.push_str(&hint);
            badge.push(' ');
        }
        left = format!("{badge}{}", left.trim_start());
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
