//! Pure coordinate mapping + per-document view state.
//!
//! `char_to_display_col` / `display_col_to_char` are the single source of truth for
//! screen↔buffer column math (CLAUDE.md invariant #5, #6). They live in this library
//! crate — not `app` — precisely so they are unit-testable without a terminal.
//!
//! ## Width model
//! Every char occupies **at least one cell**. CJK/wide chars occupy two; tabs expand to
//! the next tab stop. Zero-width chars (combining marks, ZWJ) are clamped to one cell so
//! that `char → col → char` is a genuine identity per char index — the property the
//! coordinate suite pins. Grapheme *rendering* still clusters; these functions map the
//! rope's natural unit (the char) to columns.

use unicode_width::UnicodeWidthChar;

/// Display width, in terminal cells, of a single char sitting at column `col`.
/// Always `>= 1`, so the column sequence is strictly increasing across chars.
#[inline]
fn char_cells(ch: char, col: usize, tab_width: usize) -> usize {
    if ch == '\t' {
        let tw = tab_width.max(1);
        tw - (col % tw)
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(1).max(1)
    }
}

/// Column at which char `char_idx` *starts* (i.e. total display width of the chars
/// before it). `char_idx == line.chars().count()` yields the line's total width.
pub fn char_to_display_col(line: &str, char_idx: usize, tab_width: usize) -> usize {
    let mut col = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if i >= char_idx {
            break;
        }
        col += char_cells(ch, col, tab_width);
    }
    col
}

/// Char index whose cell range contains `col`. Clicking the *second* cell of a wide
/// char resolves to that char (its start column is the first cell) — the deliberate
/// asymmetry noted in the coordinate suite. A `col` past the line end returns the char
/// count (i.e. the caret goes to end-of-line).
pub fn display_col_to_char(line: &str, col: usize, tab_width: usize) -> usize {
    let mut running = 0usize;
    for (i, ch) in line.chars().enumerate() {
        let cw = char_cells(ch, running, tab_width);
        if col < running + cw {
            return i;
        }
        running += cw;
    }
    line.chars().count()
}

/// Per-document scroll + sticky-column state.
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    /// First visible document line (0-based).
    pub scroll_line: usize,
    /// Horizontal scroll in display columns (for long lines; 0 until wrapping/hscroll).
    pub scroll_col: usize,
    /// "Sticky" target column for vertical motion, in display cells.
    pub goal_col: Option<usize>,
}

impl ViewState {
    /// Scroll the viewport so `line` is visible within a window of `height` rows,
    /// keeping a small scrolloff margin.
    pub fn scroll_to_line(&mut self, line: usize, height: usize) {
        let margin = 3usize.min(height / 2);
        if line < self.scroll_line + margin {
            self.scroll_line = line.saturating_sub(margin);
        } else if line + margin >= self.scroll_line + height {
            self.scroll_line = (line + margin + 1).saturating_sub(height);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_identity_columns() {
        assert_eq!(char_to_display_col("abc", 0, 4), 0);
        assert_eq!(char_to_display_col("abc", 1, 4), 1);
        assert_eq!(char_to_display_col("abc", 3, 4), 3);
        assert_eq!(display_col_to_char("abc", 2, 4), 2);
    }

    #[test]
    fn tabs_expand_to_stops() {
        // tab_width 4: 'a' then tab -> tab occupies cols 1..4, next char at col 4.
        assert_eq!(char_to_display_col("a\tb", 2, 4), 4);
        // Clicking anywhere inside the tab lands on the tab char (index 1).
        assert_eq!(display_col_to_char("a\tb", 1, 4), 1);
        assert_eq!(display_col_to_char("a\tb", 3, 4), 1);
        assert_eq!(display_col_to_char("a\tb", 4, 4), 2);
    }

    #[test]
    fn wide_char_second_cell_resolves_to_char() {
        // '世' is width 2.
        let line = "a世b";
        assert_eq!(char_to_display_col(line, 1, 4), 1); // 世 starts at col 1
        assert_eq!(char_to_display_col(line, 2, 4), 3); // b starts at col 3
        assert_eq!(display_col_to_char(line, 1, 4), 1); // first cell of 世
        assert_eq!(display_col_to_char(line, 2, 4), 1); // second cell of 世 -> still 世
        assert_eq!(display_col_to_char(line, 3, 4), 2); // b
    }

    #[test]
    fn round_trip_holds_for_zero_width() {
        // Combining acute accent U+0301 is width 0; clamp to 1 keeps identity.
        let line = "e\u{0301}x";
        for idx in 0..=line.chars().count() {
            let col = char_to_display_col(line, idx, 4);
            assert_eq!(display_col_to_char(line, col, 4), idx);
        }
    }
}
