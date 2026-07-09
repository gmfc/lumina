//! Shared low-level rendering helpers: the chrome palette, cell/string buffer writers, and
//! the syntax/diagnostic resolution used by the editor and status renderers.

use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthChar;

use editor_lsp::{Diagnostic, Severity};
use editor_syntax::HighlightSpan;

use crate::theme::Theme;

pub(super) const CLR_BG: Color = Color::Reset;
pub(super) const CLR_SEL: Color = Color::Rgb(50, 60, 90);
pub(super) const CLR_ACCENT: Color = Color::Rgb(90, 130, 210);
pub(super) const CLR_MATCH: Color = Color::Rgb(90, 74, 30);

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

/// Char-range diagnostics (start, end, severity) on `line_idx`, converting LSP UTF-16
/// columns to char columns against the line's text.
pub(super) fn diagnostics_on_line(
    diags: &[Diagnostic],
    line_idx: usize,
    line_text: &str,
) -> Vec<(usize, usize, Severity)> {
    use editor_lsp::position::utf16_to_char_col;
    let line = line_idx as u32;
    diags
        .iter()
        .filter(|d| d.line == line)
        .map(|d| {
            let start = utf16_to_char_col(line_text, d.start_char16);
            let end = if d.end_line == d.line {
                utf16_to_char_col(line_text, d.end_char16)
            } else {
                line_text.chars().count()
            };
            (start, end.max(start + 1), d.severity)
        })
        .collect()
}

pub(super) fn severity_rank(sev: &Severity) -> u8 {
    match sev {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
        Severity::Hint => 3,
    }
}

pub(super) fn severity_color(sev: Severity) -> Color {
    match sev {
        Severity::Error => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Info => Color::Blue,
        Severity::Hint => Color::DarkGray,
    }
}

pub(super) fn diag_marker(sev: Severity) -> char {
    match sev {
        Severity::Error => 'E',
        Severity::Warning => 'W',
        Severity::Info => 'i',
        Severity::Hint => 'h',
    }
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

    fn diag(line: u32, s: u32, e: u32, sev: Severity) -> Diagnostic {
        Diagnostic {
            line,
            start_char16: s,
            end_line: line,
            end_char16: e,
            severity: sev,
            message: String::new(),
        }
    }

    #[test]
    fn diagnostics_map_to_char_ranges_per_line() {
        let diags = vec![
            diag(0, 0, 3, Severity::Error),
            diag(1, 2, 5, Severity::Warning),
        ];
        let l0 = diagnostics_on_line(&diags, 0, "let x");
        assert_eq!(l0, vec![(0, 3, Severity::Error)]);
        let l1 = diagnostics_on_line(&diags, 1, "  abc");
        assert_eq!(l1, vec![(2, 5, Severity::Warning)]);
        // No diagnostics on line 2.
        assert!(diagnostics_on_line(&diags, 2, "").is_empty());
    }

    #[test]
    fn error_outranks_warning_for_gutter() {
        let sevs = [Severity::Warning, Severity::Error, Severity::Hint];
        let min = sevs.iter().copied().min_by_key(severity_rank).unwrap();
        assert_eq!(min, Severity::Error);
        assert_eq!(diag_marker(Severity::Error), 'E');
    }
}
