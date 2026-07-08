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
mod tests;
