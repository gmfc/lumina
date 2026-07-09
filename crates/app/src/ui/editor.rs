//! The editor pane: gutter, line numbers, text with tab expansion, selections, and cursor.
//! Written directly into the cell buffer for precise control (plan §4). The per-line render
//! loop and the per-cell styling / gutter decorations live together here — they share the
//! private [`EditorCtx`] and are only meaningful as a pair.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use editor_core::Document;
use editor_lsp::{Diagnostic, Severity};
use editor_syntax::DocHighlighter;

use crate::app::App;
use crate::editor::Focus;
use crate::theme::Theme;

use super::gutter_width;
use super::util::{
    cell_at, char_cells, diag_marker, diagnostics_on_line, put_str, resolve_line_styles,
    severity_color, severity_rank, CLR_BG, CLR_MATCH,
};

/// Read-only state threaded through the per-line / per-cell editor renderers, so the
/// helpers take one context argument instead of a dozen positional ones.
struct EditorCtx<'a> {
    doc: &'a Document,
    area: Rect,
    text_x: u16,
    gutter: u16,
    first: usize,
    hl: Option<&'a DocHighlighter>,
    theme: &'a Theme,
    sel_bg: Color,
    gutter_fg: Color,
    find_matches: &'a [(usize, usize)],
    diags: &'a [Diagnostic],
    sels: &'a [editor_core::Selection],
    /// `(bracket, partner)` char offsets to highlight, precomputed in `EditorState`.
    bracket_match: Option<(usize, usize)>,
    /// Git change map for the gutter change-bar (plan §4.1), when enabled.
    git: Option<&'a crate::git::LineStatuses>,
}

/// The editor pane: gutter + line numbers + text + selections + cursor.
pub(super) fn render_editor(f: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(doc) = app.editor.active_document() else {
        super::chrome::render_welcome(f, app, area);
        return;
    };

    let gutter = gutter_width(doc);
    // Syntax highlighting for the active document (cached, viewport-only).
    let active_id = app.editor.workspace.active_doc();
    let ctx = EditorCtx {
        doc,
        area,
        text_x: area.x + gutter,
        gutter,
        first: doc.view.scroll_line,
        hl: active_id.and_then(|id| app.editor.highlighters.get(&id)),
        theme: &app.theme,
        sel_bg: app.theme.selection_bg,
        gutter_fg: app.theme.gutter_fg,
        // Find matches to highlight (the current one is also the primary selection).
        find_matches: app
            .editor
            .find
            .as_ref()
            .map(|find| find.matches.as_slice())
            .unwrap_or(&[]),
        // LSP diagnostics for the active document.
        diags: active_id
            .and_then(|id| app.editor.diagnostics.get(&id))
            .map(|v| v.as_slice())
            .unwrap_or(&[]),
        // Selection spans, precomputed for quick membership tests.
        sels: doc.selections.ranges(),
        bracket_match: app.editor.bracket_match,
        git: if app.config.git_gutter {
            active_id.and_then(|id| app.editor.git_hunks.get(&id))
        } else {
            None
        },
    };

    let buf = f.buffer_mut();
    let mut primary_screen: Option<(u16, u16)> = None;
    for row in 0..area.height {
        if let Some(pos) = render_editor_row(buf, &ctx, row) {
            primary_screen = Some(pos);
        }
    }

    // Only the editor shows the hardware cursor when it holds focus; the terminal panel
    // places its own cursor when focused (and draws after the editor, so it would win anyway).
    if app.editor.focus == Focus::Editor {
        place_cursor(f, area, primary_screen);
    }
}

/// Show the hardware cursor at the primary caret, if it landed inside the pane.
fn place_cursor(f: &mut Frame, area: Rect, primary_screen: Option<(u16, u16)>) {
    if let Some((x, y)) = primary_screen {
        if x < area.x + area.width && y < area.y + area.height {
            f.set_cursor_position(Position::new(x, y));
        }
    }
}

/// Render one viewport row. Returns the primary caret's screen position when it falls on
/// this row (so the caller can place the hardware cursor).
fn render_editor_row(
    buf: &mut ratatui::buffer::Buffer,
    ctx: &EditorCtx,
    row: u16,
) -> Option<(u16, u16)> {
    let line_idx = ctx.first + row as usize;
    let y = ctx.area.y + row;
    if line_idx >= ctx.doc.len_lines() {
        draw_eof_tilde(buf, ctx.area.x, y);
        return None;
    }

    // Gutter: right-aligned line number.
    let num = format!("{:>width$} ", line_idx + 1, width = ctx.gutter as usize - 1);
    put_str(
        buf,
        ctx.area.x,
        y,
        &num,
        Style::default().fg(ctx.gutter_fg),
        ctx.area.x + ctx.gutter,
    );

    // Line text with tab expansion + selection background + cursor.
    let line_start = ctx.doc.line_to_char(line_idx);
    let line_text = ctx.doc.line_text(line_idx);
    let line_text = line_text.trim_end_matches(['\n', '\r']);

    // Diagnostics on this line → per-char severity + a gutter marker.
    let line_diags = diagnostics_on_line(ctx.diags, line_idx, line_text);
    draw_diag_marker(buf, ctx.area.x, y, &line_diags);

    // Git change-bar in the gutter's separator column, just left of the text (plan §4.1).
    draw_git_bar(buf, ctx, line_idx, y);

    // Resolve syntax colors per char (shortest span wins for overlaps).
    let char_styles = ctx
        .hl
        .map(|h| resolve_line_styles(h.line_spans(line_idx), line_text.chars().count(), ctx.theme))
        .unwrap_or_default();

    let mut primary_screen = None;
    let mut col: u16 = 0;
    for (ci, ch) in line_text.chars().enumerate() {
        let char_off = line_start + ci;
        let cells = char_cells(ch, col as usize, ctx.doc.tab_width);
        let sx = ctx.text_x + col;
        if sx >= ctx.area.x + ctx.area.width {
            break;
        }
        if ctx.doc.selections.primary().head == char_off {
            primary_screen = Some((sx, y));
        }
        let style = cell_style(ctx, &line_diags, &char_styles, ci, char_off);
        draw_char_cells(buf, sx, y, ch, style, cells);
        col += cells as u16;
    }

    // Cursor at end-of-line (past last char).
    let eol_off = line_start + ctx.doc.line_len_chars(line_idx);
    if ctx.doc.selections.primary().head == eol_off {
        primary_screen = Some((ctx.text_x + col, y));
    }
    primary_screen
}

/// Draw the git change-bar for `line_idx` in the gutter's separator column (plan §4.1).
fn draw_git_bar(buf: &mut ratatui::buffer::Buffer, ctx: &EditorCtx, line_idx: usize, y: u16) {
    let Some(git) = ctx.git else {
        return;
    };
    let Some(&status) = git.get(&line_idx) else {
        return;
    };
    if ctx.gutter == 0 {
        return;
    }
    let (glyph, key) = match status {
        crate::git::LineStatus::Added => ('▍', "git.add"),
        crate::git::LineStatus::Modified => ('▍', "git.modify"),
        crate::git::LineStatus::Deleted => ('▁', "git.delete"),
    };
    let color = ctx
        .theme
        .style_for(key)
        .and_then(|s| s.fg)
        .unwrap_or(Color::Gray);
    if let Some(cell) = cell_at(buf, ctx.area.x + ctx.gutter - 1, y) {
        cell.set_char(glyph);
        cell.set_style(Style::default().fg(color));
    }
}

/// Past EOF: draw a tilde like Vim.
fn draw_eof_tilde(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16) {
    if let Some(cell) = cell_at(buf, x, y) {
        cell.set_char('~');
        cell.set_style(Style::default().fg(Color::DarkGray));
    }
}

/// Draw the gutter marker for the highest-severity diagnostic on the line, if any.
fn draw_diag_marker(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    line_diags: &[(usize, usize, Severity)],
) {
    if let Some(sev) = line_diags.iter().map(|d| d.2).min_by_key(severity_rank) {
        if let Some(cell) = cell_at(buf, x, y) {
            cell.set_char(diag_marker(sev));
            cell.set_style(Style::default().fg(severity_color(sev)));
        }
    }
}

/// Compose the style for one character cell: syntax base, then find-match, diagnostic
/// underline, selection background, and secondary-cursor inversion, in that order.
fn cell_style(
    ctx: &EditorCtx,
    line_diags: &[(usize, usize, Severity)],
    char_styles: &[Option<Style>],
    ci: usize,
    char_off: usize,
) -> Style {
    // Base = syntax color; then selection bg; then secondary-cursor inversion.
    let mut style = char_styles
        .get(ci)
        .copied()
        .flatten()
        .unwrap_or_else(|| Style::default().bg(CLR_BG));
    let in_match = ctx
        .find_matches
        .iter()
        .any(|&(s, e)| char_off >= s && char_off < e);
    if in_match {
        style = style.bg(CLR_MATCH);
    }
    // Underline diagnostic ranges in their severity color.
    if let Some(&(_, _, sev)) = line_diags.iter().find(|&&(s, e, _)| ci >= s && ci < e) {
        style = style
            .fg(severity_color(sev))
            .add_modifier(Modifier::UNDERLINED);
    }
    // Bracket-match emphasis (plan §1.3): the caret's bracket + its partner. Applied before
    // the selection background so a selected bracket still shows the selection tint.
    if let Some((a, b)) = ctx.bracket_match {
        if char_off == a || char_off == b {
            if let Some(bm) = ctx.theme.style_for("bracket.match") {
                style = style.patch(bm);
            }
        }
    }
    let in_sel = ctx
        .sels
        .iter()
        .any(|s| char_off >= s.from() && char_off < s.to());
    if in_sel {
        style = style.bg(ctx.sel_bg);
    }
    let is_secondary_cursor = ctx
        .sels
        .iter()
        .enumerate()
        .any(|(i, s)| s.head == char_off && i != ctx.doc.selections.primary_index());
    if is_secondary_cursor {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// Write `ch` (tabs shown as blanks) at `sx`, filling the trailing cells of a wide glyph
/// or tab with styled blanks.
fn draw_char_cells(
    buf: &mut ratatui::buffer::Buffer,
    sx: u16,
    y: u16,
    ch: char,
    style: Style,
    cells: usize,
) {
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
}
