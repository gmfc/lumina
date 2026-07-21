//! Single-character, vertical, and line-relative motions.

use crate::document::Document;
use crate::wrap::wrap_segments;

pub(super) fn next_grapheme(doc: &Document, pos: usize) -> usize {
    if pos >= doc.len_chars() {
        return pos;
    }
    // Grapheme-aware step: cluster from the current line's text.
    let line = doc.char_to_line(pos);
    let line_start = doc.line_to_char(line);
    let text = doc.line_text(line);
    let in_line = pos - line_start;
    let mut byte = char_to_byte(&text, in_line);
    // If at line end (before newline), just advance one char.
    let stepped = grapheme_after(&text, byte);
    if let Some(next_byte) = stepped {
        byte = next_byte;
        line_start + byte_to_char(&text, byte)
    } else {
        (pos + 1).min(doc.len_chars())
    }
}

pub(super) fn prev_grapheme(doc: &Document, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let line = doc.char_to_line(pos);
    let line_start = doc.line_to_char(line);
    if pos == line_start {
        // Move to end of previous line's content (before its newline).
        return pos - 1;
    }
    let text = doc.line_text(line);
    let in_line = pos - line_start;
    let byte = char_to_byte(&text, in_line);
    if let Some(prev_byte) = grapheme_before(&text, byte) {
        line_start + byte_to_char(&text, prev_byte)
    } else {
        pos - 1
    }
}

fn grapheme_after(text: &str, byte: usize) -> Option<usize> {
    let mut cursor = unicode_segmentation::GraphemeCursor::new(byte, text.len(), true);
    cursor.next_boundary(text, 0).ok().flatten()
}

fn grapheme_before(text: &str, byte: usize) -> Option<usize> {
    let mut cursor = unicode_segmentation::GraphemeCursor::new(byte, text.len(), true);
    cursor.prev_boundary(text, 0).ok().flatten()
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Move `delta` lines, preserving the sticky goal column (in display cells).
pub(super) fn vertical(doc: &Document, pos: usize, delta: isize) -> usize {
    let (line, _col) = doc.char_to_line_col(pos);
    let goal = doc
        .view
        .goal_col
        .unwrap_or_else(|| current_display_col(doc, pos));
    let target_line = (line as isize + delta).clamp(0, doc.len_lines() as isize - 1) as usize;
    let line_text = doc.line_text(target_line);
    let line_text = line_text.trim_end_matches(['\n', '\r']);
    let char_in_line = crate::view::display_col_to_char(line_text, goal, doc.tab_width);
    doc.line_to_char(target_line) + char_in_line
}

fn current_display_col(doc: &Document, pos: usize) -> usize {
    let (line, col) = doc.char_to_line_col(pos);
    let text = doc.line_text(line);
    let text = text.trim_end_matches(['\n', '\r']);
    crate::view::char_to_display_col(text, col, doc.tab_width)
}

/// Whether soft-wrap navigation applies right now (wrap on and the pane geometry is known).
pub(super) fn wrap_active(doc: &Document) -> bool {
    doc.view.wrap && doc.view.wrap_width > 0
}

/// The trailing-newline-trimmed body of `line`.
fn line_body(doc: &Document, line: usize) -> String {
    let t = doc.line_text(line);
    t.trim_end_matches(['\n', '\r']).to_string()
}

/// Display column of char offset `char_in_line` measured **within its visual row** (rows start at
/// display column 0), given the row's `[seg_start, _)` range.
fn col_within_segment(body: &str, seg_start: usize, char_in_line: usize, tab: usize) -> usize {
    let seg_prefix: String = body
        .chars()
        .skip(seg_start)
        .take(char_in_line - seg_start)
        .collect();
    let n = seg_prefix.chars().count();
    crate::view::char_to_display_col(&seg_prefix, n, tab)
}

/// Move `delta` **visual rows** under soft-wrap, preserving the goal column within the visual row.
/// Falls back to logical [`vertical`] when the pane geometry isn't known yet.
pub(super) fn vertical_visual(doc: &Document, pos: usize, delta: isize) -> usize {
    if !wrap_active(doc) {
        return vertical(doc, pos, delta);
    }
    let width = doc.view.wrap_width;
    let tab = doc.tab_width;
    let (line, char_in_line) = doc.char_to_line_col(pos);
    let cur_body = line_body(doc, line);
    let cur_segs = wrap_segments(&cur_body, width, tab);
    let mut seg_idx = cur_segs
        .partition_point(|&s| s <= char_in_line)
        .saturating_sub(1);
    let goal = doc
        .view
        .goal_col
        .unwrap_or_else(|| col_within_segment(&cur_body, cur_segs[seg_idx], char_in_line, tab));

    // Walk `delta` visual rows, crossing logical-line boundaries.
    let mut tline = line;
    let mut tsegs = cur_segs;
    for _ in 0..delta.unsigned_abs() {
        if delta > 0 {
            if seg_idx + 1 < tsegs.len() {
                seg_idx += 1;
            } else if tline + 1 < doc.len_lines() {
                tline += 1;
                tsegs = wrap_segments(&line_body(doc, tline), width, tab);
                seg_idx = 0;
            } else {
                break; // already the last visual row
            }
        } else if seg_idx > 0 {
            seg_idx -= 1;
        } else if tline > 0 {
            tline -= 1;
            tsegs = wrap_segments(&line_body(doc, tline), width, tab);
            seg_idx = tsegs.len() - 1;
        } else {
            break; // already the first visual row
        }
    }

    // Map the goal column onto the target visual row.
    let tbody = line_body(doc, tline);
    let tlen = tbody.chars().count();
    let ts_start = tsegs[seg_idx];
    let ts_end = tsegs.get(seg_idx + 1).copied().unwrap_or(tlen);
    let seg_text: String = tbody
        .chars()
        .skip(ts_start)
        .take(ts_end - ts_start)
        .collect();
    let off = crate::view::display_col_to_char(&seg_text, goal, tab);
    let mut char_in_target = (ts_start + off).min(ts_end);
    // `ts_end` on a *non-final* visual row is the next row's first char (and renders there), so a
    // goal past this row's width would skip a whole visual row. Clamp to the row's last char so
    // Down/Up move exactly one visual row and stay reversible.
    if char_in_target == ts_end && ts_end < tlen {
        char_in_target = ts_end - 1;
    }
    doc.line_to_char(tline) + char_in_target
}

/// `Home` under soft-wrap: the first char of the caret's **visual row**.
pub(super) fn visual_line_start(doc: &Document, pos: usize) -> usize {
    let (line, char_in_line) = doc.char_to_line_col(pos);
    let body = line_body(doc, line);
    let segs = wrap_segments(&body, doc.view.wrap_width, doc.tab_width);
    let (start, _) = crate::wrap::segment_of(&segs, body.chars().count(), char_in_line);
    doc.line_to_char(line) + start
}

/// `End` under soft-wrap: the end of the caret's **visual row** (its last char's trailing edge).
pub(super) fn visual_line_end(doc: &Document, pos: usize) -> usize {
    let (line, char_in_line) = doc.char_to_line_col(pos);
    let body = line_body(doc, line);
    let segs = wrap_segments(&body, doc.view.wrap_width, doc.tab_width);
    let (_, end) = crate::wrap::segment_of(&segs, body.chars().count(), char_in_line);
    doc.line_to_char(line) + end
}

pub(super) fn first_non_blank(doc: &Document, pos: usize) -> usize {
    let line = doc.char_to_line(pos);
    let start = doc.line_to_char(line);
    let text = doc.line_text(line);
    for (i, ch) in text.chars().enumerate() {
        if !ch.is_whitespace() {
            return start + i;
        }
    }
    start
}
