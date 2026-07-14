//! The editor pane: gutter, line numbers, text with tab expansion, selections, and cursor.
//! Written directly into the cell buffer for precise control (plan §4). The per-line render
//! loop and the per-cell styling / gutter decorations live together here — they share the
//! private [`EditorCtx`] and are only meaningful as a pair.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use editor_core::Document;
use editor_plugin::{Decoration, GutterMark, VirtualText};
use editor_syntax::DocHighlighter;
use unicode_width::UnicodeWidthChar;

use crate::app::App;
use crate::editor::Focus;
use crate::theme::Theme;

use super::gutter_width;
use super::util::{cell_at, char_cells, put_str, resolve_line_styles, CLR_BG};

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
    /// (layer-name) order. Painted over syntax. Find matches (the `find` plugin's "find.match"
    /// layer) and diagnostics (the app's "lsp.diag" layer) both arrive here.
    deco_spans: Vec<&'a Decoration>,
    /// Published gutter marks for the active doc, flattened across layers (same order): the git
    /// change-bar is separate, but diagnostic severity markers arrive here.
    deco_gutter: Vec<&'a GutterMark>,
    /// Published inline virtual text (inlay hints §7.2, code lens §6.4), flattened across layers.
    /// Rendered between the real characters, displacing them right on screen.
    deco_virtual: Vec<&'a VirtualText>,
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
) -> (Vec<&Decoration>, Vec<&GutterMark>, Vec<&VirtualText>) {
    let mut spans = Vec::new();
    let mut gutter = Vec::new();
    let mut virtual_text = Vec::new();
    if let Some(layers) = active.and_then(|id| app.editor.decorations.get(&id)) {
        let mut keys: Vec<&String> = layers.keys().collect();
        keys.sort();
        for k in keys {
            let set = &layers[k];
            spans.extend(set.spans.iter());
            gutter.extend(set.gutter.iter());
            virtual_text.extend(set.virtual_text.iter());
        }
    }
    (spans, gutter, virtual_text)
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
    let (deco_spans, deco_gutter, deco_virtual) = collect_decorations(app, active_id);
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
        deco_virtual,
        // Selection spans, precomputed for quick membership tests.
        sels: doc.selections.ranges(),
        bracket_match: app.editor.bracket_match,
        git: if app.config.git_gutter {
            active_id.and_then(|id| app.editor.git_hunks.get(&id))
        } else {
            None
        },
        vim_visual_char: matches!(
            app.editor.vim_view.as_ref().map(|v| v.mode),
            Some(editor_plugin::VimMode::Visual)
        ),
        vim_visual_lines: match app.editor.vim_view.as_ref().map(|v| v.mode) {
            Some(editor_plugin::VimMode::VisualLine) => {
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

    // Git change-bar in the gutter's separator column, just left of the text (plan §4.1).
    draw_git_bar(buf, ctx, line_idx, y);

    // Published gutter marks (diagnostics severity glyphs, …) in the leftmost gutter column.
    draw_deco_gutter(buf, ctx, line_idx, y);

    // Resolve syntax colors per char (shortest span wins for overlaps).
    let char_styles = ctx
        .hl
        .map(|h| resolve_line_styles(h.line_spans(line_idx), line_text.chars().count(), ctx.theme))
        .unwrap_or_default();

    draw_line_body(buf, ctx, y, line_idx, line_start, line_text, &char_styles)
}

/// Draw one line's text with tab expansion, inline virtual text (inlay hints / code lens),
/// hscroll clipping, and the caret. Returns the primary caret's screen position when it lands on
/// this line. Split out of [`render_editor_row`] (which does the gutter) to keep each focused.
fn draw_line_body(
    buf: &mut ratatui::buffer::Buffer,
    ctx: &EditorCtx,
    y: u16,
    line_idx: usize,
    line_start: usize,
    line_text: &str,
    char_styles: &[Option<Style>],
) -> Option<(u16, u16)> {
    let mut primary_screen = None;
    let view_width = ctx.area.width.saturating_sub(ctx.gutter) as usize;
    // Inline virtual text anchored on this line, sorted by offset so each is emitted just before
    // the character it anchors before (the tail trails the line end).
    let eol_off = line_start + ctx.doc.line_len_chars(line_idx);
    let mut vts: Vec<&VirtualText> = ctx
        .deco_virtual
        .iter()
        .copied()
        .filter(|v| v.offset >= line_start && v.offset <= eol_off)
        .collect();
    vts.sort_by_key(|v| v.offset);
    let mut vi = 0;
    // `col` is the absolute display column from the line start (tab stops are absolute); the
    // on-screen position is `col` shifted left by the horizontal scroll offset.
    let mut col: usize = 0;
    for (ci, ch) in line_text.chars().enumerate() {
        let char_off = line_start + ci;
        vi = drain_virtual_at(buf, ctx, y, &vts, vi, char_off, &mut col, view_width);
        let cells = char_cells(ch, col, ctx.doc.tab_width);
        let (screen, past_right) = draw_clipped_char(
            buf,
            ctx,
            y,
            ch,
            ci,
            char_off,
            col,
            cells,
            view_width,
            char_styles,
        );
        if let Some(pos) = screen {
            primary_screen = Some(pos);
        }
        col += cells;
        // Off the right edge and nothing trails this line → done.
        if past_right && vi >= vts.len() {
            break;
        }
    }

    // Caret at end-of-line (past the last char), placed at the real line width — before any
    // trailing virtual text.
    if ctx.doc.selections.primary().head == eol_off && col >= ctx.hscroll {
        let delta = col - ctx.hscroll;
        if delta <= view_width {
            primary_screen = Some((ctx.text_x + delta as u16, y));
        }
    }
    // Trailing virtual text anchored at the line end (end-of-line inlay hints, code lens).
    while vi < vts.len() {
        col = emit_virtual(buf, ctx, y, vts[vi], col, view_width);
        vi += 1;
    }
    primary_screen
}

/// Emit every virtual-text run anchored exactly before `char_off` (advancing `col`), returning the
/// next unconsumed index into the sorted `vts`.
#[allow(clippy::too_many_arguments)]
fn drain_virtual_at(
    buf: &mut ratatui::buffer::Buffer,
    ctx: &EditorCtx,
    y: u16,
    vts: &[&VirtualText],
    mut vi: usize,
    char_off: usize,
    col: &mut usize,
    view_width: usize,
) -> usize {
    while vi < vts.len() && vts[vi].offset == char_off {
        *col = emit_virtual(buf, ctx, y, vts[vi], *col, view_width);
        vi += 1;
    }
    vi
}

/// Draw one character at display column `col` with horizontal-scroll clipping. Returns the caret's
/// screen position if this char holds the primary caret, and whether it fell off the right edge.
#[allow(clippy::too_many_arguments)]
fn draw_clipped_char(
    buf: &mut ratatui::buffer::Buffer,
    ctx: &EditorCtx,
    y: u16,
    ch: char,
    ci: usize,
    char_off: usize,
    col: usize,
    cells: usize,
    view_width: usize,
    char_styles: &[Option<Style>],
) -> (Option<(u16, u16)>, bool) {
    let end = col + cells;
    if end <= ctx.hscroll {
        return (None, false); // wholly left of the viewport
    }
    let skip = ctx.hscroll.saturating_sub(col); // cells clipped off the left edge
    let delta = col + skip - ctx.hscroll; // == max(col, hscroll) - hscroll
    if delta >= view_width {
        return (None, true); // off the right edge
    }
    let sx = ctx.text_x + delta as u16;
    let screen = (ctx.doc.selections.primary().head == char_off).then_some((sx, y));
    let style = cell_style(ctx, char_styles, ci, char_off);
    draw_char_cells(buf, sx, y, ch, style, cells, skip);
    (screen, false)
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
/// (find-match, lsp.diag underline, …), bracket emphasis, selection background, and
/// secondary-cursor inversion, in that order.
fn cell_style(ctx: &EditorCtx, char_styles: &[Option<Style>], ci: usize, char_off: usize) -> Style {
    // Base = syntax color; then selection bg; then secondary-cursor inversion.
    let mut style = char_styles
        .get(ci)
        .copied()
        .flatten()
        .unwrap_or_else(|| Style::default().bg(CLR_BG));
    // Published decoration layers (find matches + diagnostics + …). Painted over syntax and under
    // the bespoke bracket/selection tints below. Deterministic layer order.
    for d in &ctx.deco_spans {
        if char_off >= d.range.0 && char_off < d.range.1 {
            style = style.patch(ctx.theme.decoration_style(&d.style));
        }
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
/// Draw one inline virtual-text run at display column `col` (with the same horizontal-scroll
/// clipping as real characters), returning the column just past it. The text carries no tabs, so
/// each char is its plain display width; the theme resolves `vt.style` to a concrete style.
fn emit_virtual(
    buf: &mut ratatui::buffer::Buffer,
    ctx: &EditorCtx,
    y: u16,
    vt: &VirtualText,
    mut col: usize,
    view_width: usize,
) -> usize {
    let style = ctx.theme.decoration_style(&vt.style);
    for ch in vt.display().chars() {
        let cells = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        let end = col + cells;
        if end <= ctx.hscroll {
            col = end;
            continue;
        }
        let skip = ctx.hscroll.saturating_sub(col);
        let delta = col + skip - ctx.hscroll;
        if delta < view_width {
            draw_char_cells(buf, ctx.text_x + delta as u16, y, ch, style, cells, skip);
        }
        col = end;
    }
    col
}

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
