//! Single-character, vertical, and line-relative motions.

use crate::document::Document;

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
