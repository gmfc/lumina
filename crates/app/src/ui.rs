//! Rendering — a pure function of state (plan §4, invariant #8). No mutation of editor
//! state happens here; we only read it and write cells.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use editor_core::Document;

use crate::app::App;
use crate::editor::Focus;

const CLR_BG: Color = Color::Reset;
const CLR_GUTTER: Color = Color::DarkGray;
const CLR_SEL: Color = Color::Rgb(50, 60, 90);
const CLR_ACCENT: Color = Color::Rgb(90, 130, 210);

/// Draw one full frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [tabs_area, body, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    // Remember body height for PageUp/PageDown next tick.
    app.page_height = body.height.saturating_sub(0) as usize;

    let (editor_area, sidebar_area) = if app.editor.sidebar_visible {
        let [sidebar, editors] = Layout::horizontal([
            Constraint::Length(app.editor.sidebar_width),
            Constraint::Min(0),
        ])
        .areas(body);
        render_sidebar(f, app, sidebar);
        (editors, Some(sidebar))
    } else {
        (body, None)
    };

    render_tabs(f, app, tabs_area);
    render_editor(f, app, editor_area);
    render_status(f, app, status_area);

    // Record laid-out regions so the mouse router (which runs outside draw) can hit-test.
    app.regions = Regions {
        tabs: tabs_area,
        sidebar: sidebar_area,
        editor: editor_area,
    };
}

/// Screen regions from the last frame, for mouse hit-testing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Regions {
    pub tabs: Rect,
    pub sidebar: Option<Rect>,
    pub editor: Rect,
}

/// Gutter width for a document (digits + one padding space). Shared with the mouse router.
pub fn gutter_width(doc: &Document) -> u16 {
    let digits = ((doc.len_lines().max(1)) as f64).log10().floor() as u16 + 1;
    digits.max(3) + 1
}

fn render_tabs(f: &mut Frame, app: &App, area: Rect) {
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
        let marker = if doc.dirty { "●" } else { "×" };
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

fn render_sidebar(f: &mut Frame, app: &App, area: Rect) {
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

/// The editor pane: gutter + line numbers + text + selections + cursor. Written directly
/// into the cell buffer for precise control (plan §4).
fn render_editor(f: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(doc) = app.editor.active_document() else {
        render_welcome(f, area);
        return;
    };

    let gutter = gutter_width(doc);
    let text_x = area.x + gutter;
    let first = doc.view.scroll_line;
    let buf = f.buffer_mut();

    // Precompute the selection spans for quick membership tests.
    let sels = doc.selections.ranges();

    let mut primary_screen: Option<(u16, u16)> = None;

    for row in 0..area.height {
        let line_idx = first + row as usize;
        let y = area.y + row;
        if line_idx >= doc.len_lines() {
            // Past EOF: draw a tilde like Vim.
            if let Some(cell) = cell_at(buf, area.x, y) {
                cell.set_char('~');
                cell.set_style(Style::default().fg(Color::DarkGray));
            }
            continue;
        }

        // Gutter: right-aligned line number.
        let num = format!("{:>width$} ", line_idx + 1, width = gutter as usize - 1);
        put_str(
            buf,
            area.x,
            y,
            &num,
            Style::default().fg(CLR_GUTTER),
            area.x + gutter,
        );

        // Line text with tab expansion + selection background + cursor.
        let line_start = doc.line_to_char(line_idx);
        let line_text = doc.line_text(line_idx);
        let line_text = line_text.trim_end_matches(['\n', '\r']);

        let mut col: u16 = 0;
        for (ci, ch) in line_text.chars().enumerate() {
            let char_off = line_start + ci;
            let cells = char_cells(ch, col as usize, doc.tab_width);
            let sx = text_x + col;
            if sx >= area.x + area.width {
                break;
            }

            let in_sel = sels
                .iter()
                .any(|s| char_off >= s.from() && char_off < s.to());
            let is_secondary_cursor = sels
                .iter()
                .enumerate()
                .any(|(i, s)| s.head == char_off && i != doc.selections.primary_index());
            let is_primary_cursor = doc.selections.primary().head == char_off;

            if is_primary_cursor {
                primary_screen = Some((sx, y));
            }

            let mut style = Style::default().bg(CLR_BG);
            if in_sel {
                style = style.bg(CLR_SEL);
            }
            if is_secondary_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }

            let display = if ch == '\t' { ' ' } else { ch };
            if let Some(cell) = cell_at(buf, sx, y) {
                cell.set_char(display);
                cell.set_style(style);
            }
            // Fill remaining cells of a wide char / tab with styled blanks.
            for k in 1..cells {
                if let Some(cell) = cell_at(buf, sx + k as u16, y) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
            }
            col += cells as u16;
        }

        // Cursor at end-of-line (past last char).
        let eol_off = line_start + doc.line_len_chars(line_idx);
        if doc.selections.primary().head == eol_off {
            primary_screen = Some((text_x + col, y));
        }
    }

    if let Some((x, y)) = primary_screen {
        if x < area.x + area.width && y < area.y + area.height {
            f.set_cursor_position(Position::new(x, y));
        }
    }
}

fn render_welcome(f: &mut Frame, area: Rect) {
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

fn render_status(f: &mut Frame, app: &App, area: Rect) {
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
        let lang = doc.language.clone().unwrap_or_else(|| "text".into());
        right = format!("Ln {}, Col {}   UTF-8  {le}  {lang} ", line + 1, col + 1);
    } else {
        left = " No file open".into();
    }

    if let Some(msg) = &app.editor.status_message {
        left = format!(" {msg}");
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

// --- small buffer helpers ------------------------------------------------------

fn char_cells(ch: char, col: usize, tab_width: usize) -> usize {
    if ch == '\t' {
        let tw = tab_width.max(1);
        tw - (col % tw)
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(1).max(1)
    }
}

fn cell_at(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
) -> Option<&mut ratatui::buffer::Cell> {
    if x < buf.area.right() && y < buf.area.bottom() && x >= buf.area.left() && y >= buf.area.top()
    {
        Some(&mut buf[(x, y)])
    } else {
        None
    }
}

fn put_str(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, s: &str, style: Style, max_x: u16) {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= max_x {
            break;
        }
        if let Some(cell) = cell_at(buf, cx, y) {
            cell.set_char(ch);
            cell.set_style(style);
        }
        cx += 1;
    }
}

fn display_len(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(1).max(1))
        .sum()
}
