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
pub(crate) fn char_cells(ch: char, col: usize, tab_width: usize) -> usize {
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
    /// First visible display column (horizontal scroll offset for long lines).
    pub scroll_col: usize,
    /// Tab stop width.
    pub tab_width: usize,
    /// Visible height in rows (for bounds checks).
    pub height: u16,
    /// Soft word-wrap on for this pane (mirrors the doc's `view.wrap`).
    pub wrap: bool,
    /// Text width in cells to wrap at (mirrors the doc's `view.wrap_width`).
    pub wrap_width: usize,
    /// Which visual row of `scroll_line` is the first on screen (mirrors `view.scroll_sub`).
    pub scroll_sub: usize,
}

/// One visible screen row under soft-wrap: the logical line it belongs to and the `[start, end)`
/// char range (within that line) it displays. `first` marks the line's initial row (the one that
/// shows the line number; continuations blank the gutter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub first: bool,
}

/// Lay out up to `height` visible screen rows for soft-wrap, starting at visual row `scroll_sub`
/// of logical line `scroll_line`. Stops at end-of-document. The single source of truth for the
/// visible visual-row layout — rendering, click mapping, and caret placement all consult it, so
/// they can't disagree.
pub fn visual_rows(
    doc: &Document,
    width: usize,
    tab_width: usize,
    scroll_line: usize,
    scroll_sub: usize,
    height: usize,
) -> Vec<VisualRow> {
    let mut rows = Vec::with_capacity(height);
    let mut line = scroll_line;
    let mut seg = scroll_sub;
    while rows.len() < height && line < doc.len_lines() {
        let body = line_body(doc, line);
        let len = body.chars().count();
        let segs = crate::wrap::wrap_segments(&body, width, tab_width);
        // A stale `scroll_sub` (line changed length) is clamped into range.
        let mut s = seg.min(segs.len().saturating_sub(1));
        while s < segs.len() && rows.len() < height {
            let start = segs[s];
            let end = segs.get(s + 1).copied().unwrap_or(len);
            rows.push(VisualRow {
                line,
                start,
                end,
                first: s == 0,
            });
            s += 1;
        }
        line += 1;
        seg = 0; // only the top line honors scroll_sub
    }
    rows
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
    if geo.wrap && geo.wrap_width > 0 {
        // Walk the visible visual-row layout to the clicked screen row, then map the click column
        // (no horizontal scroll under wrap) onto that row's char range.
        let rows = visual_rows(
            doc,
            geo.wrap_width,
            geo.tab_width,
            geo.scroll_line,
            geo.scroll_sub,
            geo.height as usize,
        );
        let vr = rows.get(y as usize)?;
        let body = line_body(doc, vr.line);
        let seg_text: String = body
            .chars()
            .skip(vr.start)
            .take(vr.end - vr.start)
            .collect();
        let off = display_col_to_char(&seg_text, x as usize, geo.tab_width);
        let char_in_line = (vr.start + off).min(vr.end);
        return Some(doc.line_to_char(vr.line) + char_in_line);
    }
    let line = geo.scroll_line + y as usize;
    if line >= doc.len_lines() {
        return None;
    }
    let text = line_body(doc, line);
    // Shift the click into buffer-column space by the horizontal scroll offset.
    let display_col = x as usize + geo.scroll_col;
    let char_in_line = display_col_to_char(&text, display_col, geo.tab_width);
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
    if geo.wrap && geo.wrap_width > 0 {
        let rows = visual_rows(
            doc,
            geo.wrap_width,
            geo.tab_width,
            geo.scroll_line,
            geo.scroll_sub,
            geo.height as usize,
        );
        // The row that contains this char (`start <= col < end`); at a segment boundary the caret
        // renders at the *next* row's start. End-of-line falls on the last row (`col == end`).
        let idx = rows
            .iter()
            .position(|r| r.line == line && col_chars >= r.start && col_chars < r.end)
            .or_else(|| {
                rows.iter()
                    .rposition(|r| r.line == line && col_chars == r.end)
            })?;
        let vr = rows[idx];
        let body = line_body(doc, line);
        let prefix: String = body
            .chars()
            .skip(vr.start)
            .take(col_chars - vr.start)
            .collect();
        let col_in_row = char_to_display_col(&prefix, prefix.chars().count(), geo.tab_width);
        let x = geo
            .origin_x
            .saturating_add(geo.gutter)
            .saturating_add(u16::try_from(col_in_row).unwrap_or(u16::MAX));
        let y = geo.origin_y.saturating_add(idx as u16);
        return Some((x, y));
    }
    let y_off = line - geo.scroll_line;
    if y_off >= geo.height as usize {
        return None;
    }
    let text = line_body(doc, line);
    let display_col = char_to_display_col(&text, col_chars, geo.tab_width);
    // Horizontally scrolled off the left edge → not visible.
    let visible_col = display_col.checked_sub(geo.scroll_col)?;
    // Saturate rather than truncate/overflow: a caret past column ~65535 (an extremely long
    // line) would otherwise wrap to a bogus small X or panic on overflow in debug builds.
    let x = geo
        .origin_x
        .saturating_add(geo.gutter)
        .saturating_add(u16::try_from(visible_col).unwrap_or(u16::MAX));
    let y = geo.origin_y.saturating_add(y_off as u16);
    Some((x, y))
}

/// The text of `line` without its trailing newline.
fn line_body(doc: &Document, line: usize) -> String {
    let t = doc.line_text(line);
    t.trim_end_matches(['\n', '\r']).to_string()
}

/// The number of visual rows logical `line` occupies when wrapped to `width`.
fn line_row_count(doc: &Document, line: usize, width: usize, tab: usize) -> usize {
    crate::wrap::wrap_segments(&line_body(doc, line), width, tab).len()
}

/// Move the `(line, sub)` visual-row anchor `n` rows up (toward the document start), clamping at the
/// first visual row of the document.
fn step_up(
    doc: &Document,
    mut line: usize,
    mut sub: usize,
    n: usize,
    width: usize,
    tab: usize,
) -> (usize, usize) {
    for _ in 0..n {
        if sub > 0 {
            sub -= 1;
        } else if line > 0 {
            line -= 1;
            sub = line_row_count(doc, line, width, tab) - 1;
        } else {
            break;
        }
    }
    (line, sub)
}

/// Move the `(line, sub)` visual-row anchor `n` rows down (toward the document end), clamping at the
/// last visual row of the document.
fn step_down(
    doc: &Document,
    mut line: usize,
    mut sub: usize,
    n: usize,
    width: usize,
    tab: usize,
) -> (usize, usize) {
    for _ in 0..n {
        if sub + 1 < line_row_count(doc, line, width, tab) {
            sub += 1;
        } else if line + 1 < doc.len_lines() {
            line += 1;
            sub = 0;
        } else {
            break;
        }
    }
    (line, sub)
}

/// Compute the `(scroll_line, scroll_sub)` visual-row anchor that keeps `caret`'s visual row within
/// a `height`-row window (with a small scrolloff margin), given the current anchor. Pure and
/// terminal-free — the wrap analogue of [`ViewState::scroll_to_line`]. When the caret is far outside
/// the current window it re-anchors so the caret sits `margin` rows from the top.
pub fn wrapped_scroll_anchor(
    doc: &Document,
    caret: usize,
    height: usize,
    width: usize,
    tab: usize,
    scroll_line: usize,
    scroll_sub: usize,
) -> (usize, usize) {
    if height == 0 || width == 0 {
        return (scroll_line, scroll_sub);
    }
    let margin = 3usize.min(height / 2);
    // The caret's visual row, identified by its segment's start offset within its line.
    let (cline, cchar) = doc.char_to_line_col(caret);
    let csegs = crate::wrap::wrap_segments(&line_body(doc, cline), width, tab);
    let ci = csegs.partition_point(|&s| s <= cchar).saturating_sub(1);
    let cseg_start = csegs[ci];

    // Look a full window (plus margin) down from the anchor to locate the caret.
    let rows = visual_rows(
        doc,
        width,
        tab,
        scroll_line,
        scroll_sub,
        height + margin + 1,
    );
    if let Some(p) = rows
        .iter()
        .position(|r| r.line == cline && r.start == cseg_start)
    {
        if p < margin {
            return step_up(doc, scroll_line, scroll_sub, margin - p, width, tab);
        }
        let last_comfortable = height.saturating_sub(margin + 1);
        if p > last_comfortable {
            return step_down(
                doc,
                scroll_line,
                scroll_sub,
                p - last_comfortable,
                width,
                tab,
            );
        }
        return (scroll_line, scroll_sub); // already comfortably visible
    }
    // Caret above the anchor, or a large jump below it → re-anchor with the caret `margin` from top.
    step_up(doc, cline, ci, margin, width, tab)
}

/// Per-document scroll + sticky-column state.
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    /// First visible document line (0-based).
    pub scroll_line: usize,
    /// Horizontal scroll in display columns (for long lines; 0 until wrapping/hscroll).
    /// Pinned to 0 while [`Self::wrap`] is on (soft-wrap replaces horizontal scrolling).
    pub scroll_col: usize,
    /// "Sticky" target column for vertical motion, in display cells.
    pub goal_col: Option<usize>,
    /// Soft word-wrap on for this document. Kept per-doc so `editor-core` motions/mapping can read
    /// it, but driven by an app-wide toggle that mirrors the same value onto every open document.
    pub wrap: bool,
    /// The editor pane's text width in cells, as laid out on the last frame. The app refreshes this
    /// each frame (like `page_height`) so wrap-aware motions/mapping see the geometry without a
    /// terminal. `0` before the first layout → treated as "no wrap this call".
    pub wrap_width: usize,
    /// Which visual row of the **top visible logical line** is the first row on screen, enabling
    /// smooth per-visual-row scrolling under wrap. Always 0 when `wrap` is off; reset to 0 whenever
    /// `scroll_line` moves to a different logical line.
    pub scroll_sub: usize,
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

    /// Scroll the viewport so display column `col` is visible within a window of `width`
    /// text cells, keeping a small horizontal scrolloff. The mirror of [`Self::scroll_to_line`]
    /// for long lines: `scroll_col` returns to 0 as the caret nears the start, and grows as it
    /// runs past the right edge.
    pub fn scroll_to_col(&mut self, col: usize, width: usize) {
        if width == 0 {
            return;
        }
        let margin = 4usize.min(width.saturating_sub(1) / 2);
        if col < self.scroll_col + margin {
            self.scroll_col = col.saturating_sub(margin);
        } else if col + margin >= self.scroll_col + width {
            self.scroll_col = (col + margin + 1).saturating_sub(width);
        }
    }
}

#[cfg(test)]
mod tests;
