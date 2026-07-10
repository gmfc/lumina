//! Shared low-level rendering helpers: the chrome palette, cell/string buffer writers, and
//! the syntax/diagnostic resolution used by the editor and status renderers.

use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthChar;

use editor_syntax::HighlightSpan;

use crate::theme::Theme;

pub(super) const CLR_BG: Color = Color::Reset;
pub(super) const CLR_SEL: Color = Color::Rgb(50, 60, 90);
pub(super) const CLR_ACCENT: Color = Color::Rgb(90, 130, 210);

/// Resolve syntax spans into a per-char style vector; for overlapping spans the **shortest**
/// (most specific) wins. On a length *tie*, the later span wins, matching tree-sitter's
/// "later/more-specific pattern overrides" convention (e.g. `@variable.builtin` captured over
/// the same node as `@variable`, where the builtin pattern comes later in the query).
pub(super) fn resolve_line_styles(
    spans: &[HighlightSpan],
    line_len: usize,
    theme: &Theme,
) -> Vec<Option<Style>> {
    let mut styles: Vec<Option<Style>> = vec![None; line_len];
    let mut best_len: Vec<usize> = vec![usize::MAX; line_len];
    for span in spans {
        let Some(style) = theme.style_for(&span.capture) else {
            continue;
        };
        let len = span.end.saturating_sub(span.start);
        for i in span.start..span.end.min(line_len) {
            if len <= best_len[i] {
                best_len[i] = len;
                styles[i] = Some(style);
            }
        }
    }
    styles
}

pub(super) fn char_cells(ch: char, col: usize, tab_width: usize) -> usize {
    if ch == '\t' {
        let tw = tab_width.max(1);
        tw - (col % tw)
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(1).max(1)
    }
}

pub(super) fn cell_at(
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

pub(super) fn put_str(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    s: &str,
    style: Style,
    max_x: u16,
) {
    for (cx, ch) in (x..).zip(s.chars()) {
        if cx >= max_x {
            break;
        }
        if let Some(cell) = cell_at(buf, cx, y) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

pub(super) fn display_len(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(1).max(1))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_len_counts_wide_chars() {
        assert_eq!(display_len("ab"), 2);
        assert_eq!(display_len("世界"), 4); // two wide (2-cell) chars
    }
}
