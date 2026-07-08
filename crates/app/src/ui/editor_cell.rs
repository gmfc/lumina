//! Per-cell and per-line gutter decorations for the editor pane: the git change-bar, the EOF
//! tilde, the diagnostic marker, the composed character style, and wide-glyph cell filling.

use ratatui::style::{Color, Modifier, Style};

use editor_lsp::Severity;

use super::editor::EditorCtx;
use super::util::{cell_at, severity_color, severity_rank, CLR_BG, CLR_MATCH};

/// Draw the git change-bar for `line_idx` in the gutter's separator column (plan §4.1).
pub(super) fn draw_git_bar(
    buf: &mut ratatui::buffer::Buffer,
    ctx: &EditorCtx,
    line_idx: usize,
    y: u16,
) {
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
pub(super) fn draw_eof_tilde(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16) {
    if let Some(cell) = cell_at(buf, x, y) {
        cell.set_char('~');
        cell.set_style(Style::default().fg(Color::DarkGray));
    }
}

/// Draw the gutter marker for the highest-severity diagnostic on the line, if any.
pub(super) fn draw_diag_marker(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    line_diags: &[(usize, usize, Severity)],
) {
    if let Some(sev) = line_diags.iter().map(|d| d.2).min_by_key(severity_rank) {
        if let Some(cell) = cell_at(buf, x, y) {
            cell.set_char(crate::ui::util::diag_marker(sev));
            cell.set_style(Style::default().fg(severity_color(sev)));
        }
    }
}

/// Compose the style for one character cell: syntax base, then find-match, diagnostic
/// underline, selection background, and secondary-cursor inversion, in that order.
pub(super) fn cell_style(
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
pub(super) fn draw_char_cells(
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
