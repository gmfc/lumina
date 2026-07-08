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

use crate::document::Document;

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

/// Explicit editor-pane geometry — everything `screen_to_char` needs, passed in so the
/// mapping stays a **pure function in this library crate** (CLAUDE.md invariant #6 corollary),
/// unit-testable without a terminal.
#[derive(Debug, Clone, Copy)]
pub struct PaneGeometry {
    /// Screen column of the pane's left edge.
    pub origin_x: u16,
    /// Screen row of the pane's top edge.
    pub origin_y: u16,
    /// Gutter width in cells (line-number column + padding).
    pub gutter: u16,
    /// First visible document line.
    pub scroll_line: usize,
    /// Tab stop width.
    pub tab_width: usize,
    /// Visible height in rows (for bounds checks).
    pub height: u16,
}

/// Map a screen cell `(col, row)` to a char offset in `doc`. Accounts for the pane origin,
/// the gutter, vertical scroll, tab expansion, and grapheme width — the one function every
/// mouse gesture uses (invariant #6). Returns `None` when the cell is outside the text area
/// or below the last line.
pub fn screen_to_char(doc: &Document, geo: &PaneGeometry, col: u16, row: u16) -> Option<usize> {
    let x = col.checked_sub(geo.origin_x + geo.gutter)?;
    let y = row.checked_sub(geo.origin_y)?;
    if y >= geo.height {
        return None;
    }
    let line = geo.scroll_line + y as usize;
    if line >= doc.len_lines() {
        return None;
    }
    let text = line_body(doc, line);
    let char_in_line = display_col_to_char(&text, x as usize, geo.tab_width);
    // Clamp to the line's own length (a click past EOL lands at end-of-line).
    let max = text.chars().count();
    Some(doc.line_to_char(line) + char_in_line.min(max))
}

/// Inverse of [`screen_to_char`] for rendering the cursor: map a char offset to the screen
/// cell where it starts. Returns `None` if the offset's line is scrolled out of view.
pub fn char_to_screen(doc: &Document, geo: &PaneGeometry, char_idx: usize) -> Option<(u16, u16)> {
    let (line, col_chars) = doc.char_to_line_col(char_idx);
    if line < geo.scroll_line {
        return None;
    }
    let y_off = line - geo.scroll_line;
    if y_off >= geo.height as usize {
        return None;
    }
    let text = line_body(doc, line);
    let display_col = char_to_display_col(&text, col_chars, geo.tab_width);
    let x = geo.origin_x + geo.gutter + display_col as u16;
    let y = geo.origin_y + y_off as u16;
    Some((x, y))
}

/// The text of `line` without its trailing newline.
fn line_body(doc: &Document, line: usize) -> String {
    let t = doc.line_text(line);
    t.trim_end_matches(['\n', '\r']).to_string()
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

    // --- screen_to_char exhaustive suite (invariant #6) -----------------------

    fn geo(gutter: u16, scroll: usize, tab: usize) -> PaneGeometry {
        PaneGeometry {
            origin_x: 0,
            origin_y: 0,
            gutter,
            scroll_line: scroll,
            tab_width: tab,
            height: 100,
        }
    }

    #[test]
    fn screen_click_accounts_for_gutter() {
        let doc = Document::from_str("hello\nworld");
        let g = geo(4, 0, 4);
        // Column 4 is the first text column (after a 4-wide gutter) -> char 0.
        assert_eq!(screen_to_char(&doc, &g, 4, 0), Some(0));
        // Column 6 -> char 2 on line 0.
        assert_eq!(screen_to_char(&doc, &g, 6, 0), Some(2));
        // Inside the gutter -> None (checked_sub underflows).
        assert_eq!(screen_to_char(&doc, &g, 2, 0), None);
    }

    #[test]
    fn screen_click_accounts_for_scroll() {
        let doc = Document::from_str("a\nb\nc\nd\ne");
        let g = geo(4, 2, 4); // line 2 ("c") is at the top
                              // row 0 maps to document line 2 -> char offset of "c".
        assert_eq!(screen_to_char(&doc, &g, 4, 0), Some(doc.line_to_char(2)));
        assert_eq!(screen_to_char(&doc, &g, 4, 1), Some(doc.line_to_char(3)));
    }

    #[test]
    fn screen_click_past_eol_lands_at_line_end() {
        let doc = Document::from_str("hi\nlonger line");
        let g = geo(4, 0, 4);
        // Click far right of the short first line -> end of "hi" (char 2).
        assert_eq!(screen_to_char(&doc, &g, 40, 0), Some(2));
    }

    #[test]
    fn screen_click_below_text_is_none() {
        let doc = Document::from_str("only one line");
        let g = geo(4, 0, 4);
        assert_eq!(screen_to_char(&doc, &g, 4, 5), None);
    }

    #[test]
    fn screen_click_on_empty_line() {
        let doc = Document::from_str("a\n\nb");
        let g = geo(4, 0, 4);
        // Line 1 is empty; any text-column click resolves to its start.
        assert_eq!(screen_to_char(&doc, &g, 10, 1), Some(doc.line_to_char(1)));
    }

    #[test]
    fn screen_click_with_tabs() {
        let doc = Document::from_str("\tx"); // tab then x, tab_width 4
        let g = geo(4, 0, 4);
        // Cells 4..8 are the tab; clicking any of them -> char 0 (the tab).
        assert_eq!(screen_to_char(&doc, &g, 4, 0), Some(0));
        assert_eq!(screen_to_char(&doc, &g, 7, 0), Some(0));
        // Cell 8 is 'x' -> char 1.
        assert_eq!(screen_to_char(&doc, &g, 8, 0), Some(1));
    }

    #[test]
    fn screen_click_with_wide_chars() {
        let doc = Document::from_str("世界x");
        let g = geo(4, 0, 4);
        // 世 occupies cells 4..6, 界 cells 6..8, x cell 8.
        assert_eq!(screen_to_char(&doc, &g, 4, 0), Some(0));
        assert_eq!(screen_to_char(&doc, &g, 5, 0), Some(0)); // second cell of 世
        assert_eq!(screen_to_char(&doc, &g, 6, 0), Some(1));
        assert_eq!(screen_to_char(&doc, &g, 8, 0), Some(2));
    }

    #[test]
    fn char_to_screen_inverts_click() {
        let doc = Document::from_str("abc\n\tdef\n世x");
        let g = PaneGeometry {
            origin_x: 2,
            origin_y: 1,
            gutter: 4,
            scroll_line: 0,
            tab_width: 4,
            height: 100,
        };
        // For every char that starts a cell, screen_to_char(char_to_screen(c)) == c.
        for line in 0..doc.len_lines() {
            let body = line_body(&doc, line);
            for col in 0..=body.chars().count() {
                let off = doc.line_to_char(line) + col;
                if let Some((x, y)) = char_to_screen(&doc, &g, off) {
                    assert_eq!(screen_to_char(&doc, &g, x, y), Some(off));
                }
            }
        }
    }
}
