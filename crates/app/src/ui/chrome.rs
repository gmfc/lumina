//! Frame chrome: the tab bar, the status bar, and the empty-state welcome screen.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;

use super::util::{display_len, CLR_ACCENT};

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
/// Only commands with a real default key belong here; palette-only actions (Vim mode, theme
/// switching) are pointed at from the footer instead, since they have no chord to show.
const WELCOME_COMMANDS: &[(&str, &str, &str)] = &[
    ("view.commandPalette", "Command Palette", "Ctrl+Shift+P"),
    ("view.quickOpen", "Open File", "Ctrl+P"),
    ("file.new", "New File", "Ctrl+N"),
    ("file.save", "Save", "Ctrl+S"),
    ("search.find", "Find", "Ctrl+F"),
    ("edit.toggleComment", "Toggle Comment", "Ctrl+/"),
    ("lsp.gotoDefinition", "Go to Definition", "F12"),
    ("lsp.rename", "Rename Symbol", "F2"),
    ("cursor.addNextMatch", "Add Cursor / Next Match", "Ctrl+D"),
    ("view.gotoLine", "Go to Line", "Ctrl+G"),
    ("view.settings", "Settings", "Ctrl+,"),
    ("terminal.toggle", "Toggle Terminal", "Ctrl+`"),
    ("app.quit", "Quit", "Ctrl+Q"),
];

/// Build the command-hint rows: two columns when the pane is wide enough, else one so nothing
/// is clipped. Keys are accented and labels dim, and every cell is padded to a shared width so
/// the rows' left edges line up once each row is centered.
fn command_hint_rows(
    commands: &[(String, &'static str)],
    width: u16,
    accent: Style,
) -> Vec<Line<'static>> {
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
    let two_col = (2 * cell_w + 3) <= width as usize;
    let cell = |(keys, label): &(String, &'static str)| {
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
    let mut rows = Vec::new();
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
    rows
}

pub(super) fn render_welcome(f: &mut Frame, app: &App, area: Rect) {
    let accent = Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD);
    let mut rows: Vec<Line> = Vec::new();

    // Resolve each command's current key from the live keymap (falling back to the default),
    // so a user's `[keys]` config overrides show through here too.
    let commands: Vec<(String, &'static str)> = WELCOME_COMMANDS
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

    // Command hints, as a one- or two-column grid depending on the pane width.
    rows.extend(command_hint_rows(&commands, area.width, accent));

    // A dim footer pointing at the command palette — the home of the actions with no default
    // key (Vim mode, theme switching) and everything else. Dropped rather than clipped when it
    // won't fit the pane's width or its remaining height, so the command grid stays intact.
    let palette_key = app
        .keymap
        .binding_label("view.commandPalette")
        .unwrap_or_else(|| "Ctrl+Shift+P".to_string());
    let footer = format!("Vim mode · themes · all commands — {palette_key}");
    let fits_width = display_len(&footer) <= area.width as usize;
    let fits_height = rows.len() + 2 <= area.height as usize;
    if fits_width && fits_height {
        rows.push(Line::from(""));
        rows.push(Line::from(TSpan::styled(
            footer,
            Style::default().fg(Color::DarkGray),
        )));
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
    } else if let Some(item) = app
        .editor
        .status_items
        .get("lsp.diag")
        .filter(|s| !s.is_empty())
    {
        // The caret diagnostic, published by the `diagnostics` plugin (glyph + message), single-
        // lined and truncated to fit (plan §2.2).
        let avail = (area.width as usize).saturating_sub(display_len(&right) + 4);
        let msg: String = item.replace('\n', " ").chars().take(avail).collect();
        left = format!(" {msg}");
    }

    // Vim mode badge (and any pending count/operator) at the far left, from the plugin's mirror.
    if let Some(vim) = &app.editor.vim_view {
        let mut badge = format!(" -- {} -- ", vim.mode.label());
        if let Some(hint) = &vim.pending {
            badge.push_str(hint);
            badge.push(' ');
        }
        left = format!("{badge}{}", left.trim_start());
    }

    // LSP work-done progress (§1.5): an animated spinner + the active operation, shown just left
    // of the position cluster so it stays visible during indexing. Truncated to keep the bar sane.
    if let Some(prog) = app
        .editor
        .status_items
        .get("lsp.progress")
        .filter(|s| !s.is_empty())
    {
        let text: String = prog.replace('\n', " ").chars().take(48).collect();
        right = format!("{} {text}   {right}", spinner_frame());
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

/// The current Braille spinner frame, advanced ~10×/s off a process-wide start instant (the run
/// loop redraws each ~16 ms, so it animates without any per-tick state to thread through).
fn spinner_frame() -> char {
    use std::sync::OnceLock;
    use std::time::Instant;
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    static START: OnceLock<Instant> = OnceLock::new();
    let i = (START.get_or_init(Instant::now).elapsed().as_millis() / 100) as usize % FRAMES.len();
    FRAMES[i]
}
