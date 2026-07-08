//! The editor pane: gutter, line numbers, text with tab expansion, selections, and cursor.
//! Written directly into the cell buffer for precise control (plan §4). Per-cell styling and
//! gutter markers live in [`super::editor_cell`].

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::Frame;

use editor_core::Document;
use editor_lsp::Diagnostic;
use editor_syntax::DocHighlighter;

use crate::app::App;
use crate::editor::Focus;
use crate::theme::Theme;

use super::editor_cell::{
    cell_style, draw_char_cells, draw_diag_marker, draw_eof_tilde, draw_git_bar,
};
use super::gutter_width;
use super::util::{char_cells, diagnostics_on_line, put_str, resolve_line_styles};

/// Read-only state threaded through the per-line / per-cell editor renderers, so the
/// helpers take one context argument instead of a dozen positional ones.
pub(super) struct EditorCtx<'a> {
    pub(super) doc: &'a Document,
    pub(super) area: Rect,
    pub(super) text_x: u16,
    pub(super) gutter: u16,
    pub(super) first: usize,
    pub(super) hl: Option<&'a DocHighlighter>,
    pub(super) theme: &'a Theme,
    pub(super) sel_bg: Color,
    pub(super) gutter_fg: Color,
    pub(super) find_matches: &'a [(usize, usize)],
    pub(super) diags: &'a [Diagnostic],
    pub(super) sels: &'a [editor_core::Selection],
    /// `(bracket, partner)` char offsets to highlight, precomputed in `EditorState`.
    pub(super) bracket_match: Option<(usize, usize)>,
    /// Git change map for the gutter change-bar (plan §4.1), when enabled.
    pub(super) git: Option<&'a crate::git::LineStatuses>,
}

/// The editor pane: gutter + line numbers + text + selections + cursor.
pub(super) fn render_editor(f: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(doc) = app.editor.active_document() else {
        super::chrome::render_welcome(f, area);
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
