//! The editor pane: gutter, line numbers, text with tab expansion, selections, and cursor.
//! Written directly into the cell buffer for precise control (plan §4). The per-line render
//! loop and the per-cell styling / gutter decorations live together here — they share the
//! private [`EditorCtx`] and are only meaningful as a pair.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use editor_core::Document;
use editor_lsp::{Diagnostic, Severity};
use editor_plugin::{Decoration, GutterMark};
use editor_syntax::DocHighlighter;

use crate::app::App;
use crate::editor::Focus;
use crate::theme::Theme;

use super::gutter_width;
use super::util::{
    cell_at, char_cells, diag_marker, diagnostics_on_line, put_str, resolve_line_styles,
    severity_color, severity_rank, CLR_BG,
};

/// Read-only state threaded through the per-line / per-cell editor renderers, so the
/// helpers take one context argument instead of a dozen positional ones.
struct EditorCtx<'a> {
    doc: &'a Document,
    area: Rect,
    text_x: u16,
    gutter: u16,
    /// Horizontal scroll offset in display columns (long-line hscroll).
    hscroll: usize,
    first: usize,
    hl: Option<&'a DocHighlighter>,
    theme: &'a Theme,
    sel_bg: Color,
    gutter_fg: Color,
    /// Published decoration spans for the active doc, flattened across layers in deterministic
    /// (layer-name) order. Painted over syntax. Find matches now arrive here (the `find` plugin
    /// publishes a "find.match" layer), as will diagnostics/etc. as they migrate.
    deco_spans: Vec<&'a Decoration>,
    /// Published gutter marks for the active doc, flattened across layers (same order).
    deco_gutter: Vec<&'a GutterMark>,
    diags: &'a [Diagnostic],
    sels: &'a [editor_core::Selection],
    /// `(bracket, partner)` char offsets to highlight, precomputed in `EditorState`.
    bracket_match: Option<(usize, usize)>,
    /// Git change map for the gutter change-bar (plan §4.1), when enabled.
    git: Option<&'a crate::git::LineStatuses>,
    /// In Vim charwise Visual mode: extend the primary selection's highlight by one
    /// char so the block under the cursor is shown (Vim's inclusive selection).
    vim_visual_char: bool,
    /// In Vim linewise Visual mode: the whole-line `[start, end)` char range to tint.
    vim_visual_lines: Option<(usize, usize)>,
}

/// Flatten the active document's published decoration layers into span + gutter-mark lists, in
/// deterministic (layer-name) order so precedence is stable frame to frame. Empty until a plugin
/// publishes a layer via `Host::set_decorations`.
fn collect_decorations(
    app: &App,
    active: Option<editor_core::DocId>,
) -> (Vec<&Decoration>, Vec<&GutterMark>) {
    let mut spans = Vec::new();
    let mut gutter = Vec::new();
    if let Some(layers) = active.and_then(|id| app.editor.decorations.get(&id)) {
        let mut keys: Vec<&String> = layers.keys().collect();
        keys.sort();
        for k in keys {
            let set = &layers[k];
            spans.extend(set.spans.iter());
            gutter.extend(set.gutter.iter());
        }
    }
    (spans, gutter)
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
    let (deco_spans, deco_gutter) = collect_decorations(app, active_id);
    let ctx = EditorCtx {
        doc,
        area,
        text_x: area.x + gutter,
        gutter,
        hscroll: doc.view.scroll_col,
        first: doc.view.scroll_line,
        hl: active_id.and_then(|id| app.editor.highlighters.get(&id)),
        theme: &app.theme,
        sel_bg: app.theme.selection_bg,
        gutter_fg: app.theme.gutter_fg,
        deco_spans,
        deco_gutter,
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
        vim_visual_char: matches!(
            app.editor.vim.as_ref().map(|v| v.mode),
            Some(crate::vim::Mode::Visual)
        ),
        vim_visual_lines: match app.editor.vim.as_ref().map(|v| v.mode) {
            Some(crate::vim::Mode::VisualLine) => {
                let p = doc.selections.primary();
                let fl = doc.char_to_line(p.from());
                let ll = doc.char_to_line(p.to());
                let start = doc.line_to_char(fl);
                let end = if ll + 1 < doc.len_lines() {
                    doc.line_to_char(ll + 1)
                } else {
                    doc.len_chars()
                };
                Some((start, end))
            }
            _ => None,
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

    // Published gutter marks (from decoration layers), drawn in the leftmost gutter column.
    draw_deco_gutter(buf, ctx, line_idx, y);

    // Resolve syntax colors per char (shortest span wins for overlaps).
    let char_styles = ctx
        .hl
        .map(|h| resolve_line_styles(h.line_spans(line_idx), line_text.chars().count(), ctx.theme))
        .unwrap_or_default();

    let mut primary_screen = None;
    // Text cells available after the gutter (the horizontal viewport).
    let view_width = ctx.area.width.saturating_sub(ctx.gutter) as usize;
    // `col` tracks the absolute display column from the line start (tab stops are absolute);
    // the on-screen position is that column shifted left by the horizontal scroll offset.
    let mut col: usize = 0;
    for (ci, ch) in line_text.chars().enumerate() {
        let char_off = line_start + ci;
        let cells = char_cells(ch, col, ctx.doc.tab_width);
        let end = col + cells;
        // Wholly left of the viewport (a tab/wide char may straddle the left edge).
        if end <= ctx.hscroll {
            col = end;
            continue;
        }
        // Cells clipped off the left edge for a char straddling `hscroll`.
        let skip = ctx.hscroll.saturating_sub(col);
        let delta = col + skip - ctx.hscroll; // == max(col, hscroll) - hscroll
        if delta >= view_width {
            break; // reached the right edge
        }
        let sx = ctx.text_x + delta as u16;
        if ctx.doc.selections.primary().head == char_off {
            primary_screen = Some((sx, y));
        }
        let style = cell_style(ctx, &line_diags, &char_styles, ci, char_off);
        draw_char_cells(buf, sx, y, ch, style, cells, skip);
        col = end;
    }

    // Cursor at end-of-line (past last char).
    let eol_off = line_start + ctx.doc.line_len_chars(line_idx);
    if ctx.doc.selections.primary().head == eol_off && col >= ctx.hscroll {
        let delta = col - ctx.hscroll;
        if delta <= view_width {
            primary_screen = Some((ctx.text_x + delta as u16, y));
        }
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

/// Draw published gutter marks (from decoration layers) for `line_idx` in the leftmost gutter
/// column. Later layers overwrite earlier ones on the same line (deterministic layer order).
fn draw_deco_gutter(buf: &mut ratatui::buffer::Buffer, ctx: &EditorCtx, line_idx: usize, y: u16) {
    if ctx.gutter == 0 {
        return;
    }
    for mark in ctx.deco_gutter.iter().filter(|m| m.line == line_idx) {
        if let Some(cell) = cell_at(buf, ctx.area.x, y) {
            cell.set_char(mark.glyph);
            cell.set_style(ctx.theme.decoration_style(&mark.style));
        }
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

/// Whether the char at `char_off` falls inside a selection. In Vim linewise Visual
/// mode the whole spanned line range is tinted; in charwise Visual the primary
/// selection is inclusive of the char under the cursor.
fn is_selected(ctx: &EditorCtx, char_off: usize) -> bool {
    if let Some((ls, le)) = ctx.vim_visual_lines {
        return char_off >= ls && char_off < le;
    }
    let primary = ctx.doc.selections.primary_index();
    ctx.sels.iter().enumerate().any(|(i, s)| {
        let to = if ctx.vim_visual_char && i == primary {
            s.to() + 1
        } else {
            s.to()
        };
        char_off >= s.from() && char_off < to
    })
}

/// Compose the style for one character cell: syntax base, then published decoration layers
/// (find-match, …), diagnostic underline, bracket emphasis, selection background, and
/// secondary-cursor inversion, in that order.
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
    // Published decoration layers (find matches today; diagnostics/etc. as they migrate). Painted
    // over syntax and under the bespoke highlights that still live below and the selection tint.
    // Deterministic layer order.
    for d in &ctx.deco_spans {
        if char_off >= d.range.0 && char_off < d.range.1 {
            style = style.patch(ctx.theme.decoration_style(&d.style));
        }
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
    if is_selected(ctx, char_off) {
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
/// or tab with styled blanks. `skip` is the number of leading cells clipped off the left
/// by horizontal scroll: when non-zero the glyph itself is off-screen, so only its
/// trailing (blank) cells are drawn — which is exactly right for tabs and acceptable for a
/// wide char whose first cell is scrolled away.
fn draw_char_cells(
    buf: &mut ratatui::buffer::Buffer,
    sx: u16,
    y: u16,
    ch: char,
    style: Style,
    cells: usize,
    skip: usize,
) {
    let visible = cells.saturating_sub(skip);
    // The glyph occupies the char's first cell; it shows only when nothing is clipped left.
    let head = if skip == 0 && ch != '\t' { ch } else { ' ' };
    for k in 0..visible {
        if let Some(cell) = cell_at(buf, sx + k as u16, y) {
            cell.set_char(if k == 0 { head } else { ' ' });
            cell.set_style(style);
        }
    }
}
