//! Soft word-wrap layout: split a logical line into **visual rows** at word boundaries.
//!
//! Pure and terminal-free (CLAUDE.md invariant #6): the single source of truth for where a
//! wrapped line breaks. Rendering, vertical motion, and screen↔char mapping all consult
//! [`wrap_segments`], so they can never disagree about the layout. Cell widths come from the same
//! [`crate::view::char_cells`] model the renderer and column math use (tabs → next tab stop,
//! wide/CJK → 2 cells, zero-width → 1), so a wrapped row is always `<= width` cells.

use crate::view::char_cells;

/// Char offsets within `line` (which must **exclude** any trailing newline) where each visual row
/// begins when soft-wrapped to `width` cells. The first element is always `0`; the length is the
/// number of visual rows (always `>= 1`, even for an empty line).
///
/// Breaks at the last whitespace boundary that fits on the row; a single word wider than `width` is
/// hard-broken at the cell that would overflow, so no row ever exceeds `width`. `width == 0` is
/// degenerate (no usable space) and yields a single unwrapped row (`vec![0]`).
pub fn wrap_segments(line: &str, width: usize, tab_width: usize) -> Vec<usize> {
    let mut segments = vec![0];
    if width == 0 {
        return segments;
    }
    let chars: Vec<char> = line.chars().collect();
    let mut seg_start = 0; // char index where the current visual row starts
    let mut col = 0; // display column within the current row (rows start at column 0)
    let mut last_break: Option<usize> = None; // char index just past the last whitespace on this row
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let cells = char_cells(ch, col, tab_width);
        // This char would overflow the row, and the row already holds at least one char (a single
        // char wider than the whole width must still be placed, so never break an empty row).
        if col + cells > width && i > seg_start {
            // Prefer the last word boundary on this row; with none, hard-break before this char.
            let break_at = match last_break {
                Some(b) if b > seg_start => b,
                _ => i,
            };
            segments.push(break_at);
            seg_start = break_at;
            last_break = None;
            // The chars carried onto the new row ([break_at, i)) start at column 0 again — recompute
            // (tab expansion is column-relative, so their widths can differ on the new row).
            col = 0;
            for &c in &chars[break_at..i] {
                col += char_cells(c, col, tab_width);
            }
            continue; // re-evaluate chars[i] against the fresh row
        }
        if ch == ' ' || ch == '\t' {
            last_break = Some(i + 1);
        }
        col += cells;
        i += 1;
    }
    segments
}

/// The `[start, end)` char range of the visual row containing char offset `char_in_line`, where
/// `segments` is [`wrap_segments`] output for the line and `line_len` is its char count (excluding
/// the newline). `end` is the next segment start, or `line_len` for the last row.
pub fn segment_of(segments: &[usize], line_len: usize, char_in_line: usize) -> (usize, usize) {
    // Last segment whose start is `<= char_in_line`.
    let idx = segments
        .partition_point(|&s| s <= char_in_line)
        .saturating_sub(1);
    let start = segments[idx];
    let end = segments.get(idx + 1).copied().unwrap_or(line_len);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert the segments AND that no visual row exceeds `width` cells.
    fn check(line: &str, width: usize, tab: usize, expected: &[usize]) {
        let segs = wrap_segments(line, width, tab);
        assert_eq!(segs, expected, "segments for {line:?} @ w={width}");
        let chars: Vec<char> = line.chars().collect();
        for w in 0..segs.len() {
            let start = segs[w];
            let end = segs.get(w + 1).copied().unwrap_or(chars.len());
            let mut col = 0;
            for &c in &chars[start..end] {
                col += char_cells(c, col, tab);
            }
            // A row may exceed width only when it is a single char wider than width.
            assert!(
                col <= width || end - start == 1,
                "row {w} ({start}..{end}) is {col} cells > width {width}"
            );
        }
    }

    #[test]
    fn short_line_is_one_row() {
        check("hello", 20, 4, &[0]);
    }

    #[test]
    fn exact_width_does_not_wrap() {
        check("abcde", 5, 4, &[0]); // exactly 5 cells → one row, no spurious break
    }

    #[test]
    fn breaks_at_word_boundary() {
        // "the quick" (9) doesn't fit in 8 → break after "the " at offset 4.
        check("the quick", 8, 4, &[0, 4]);
    }

    #[test]
    fn over_long_word_hard_breaks() {
        // No whitespace to break on → hard break at the overflow cell.
        check("abcdefghij", 4, 4, &[0, 4, 8]);
    }

    #[test]
    fn word_then_long_word() {
        // "ab cdefghij" @ 5: "ab " | "cdefg" | "hij".
        check("ab cdefghij", 5, 4, &[0, 3, 8]);
    }

    #[test]
    fn empty_line_is_one_row() {
        check("", 10, 4, &[0]);
    }

    #[test]
    fn zero_width_is_degenerate_single_row() {
        assert_eq!(wrap_segments("anything", 0, 4), vec![0]);
    }

    #[test]
    fn tab_expands_to_tab_stop() {
        // Tab at col 0 with tab_width 4 spans 4 cells; "\tab" = 4 + 1 + 1 = 6 > 5 → 'b' wraps.
        // Break candidates: the tab counts as whitespace (last_break after it), so break after tab.
        check("\tab", 5, 4, &[0, 1]);
    }

    #[test]
    fn wide_chars_count_two_cells() {
        // Each CJK char is 2 cells; width 5 fits two (4 cells) then the third wraps.
        check("世界人", 5, 4, &[0, 2]);
    }

    #[test]
    fn wide_char_at_boundary_moves_whole() {
        // "a世" = 1 + 2 = 3 fits in 3; adding another wide char would need 5 > 3 → wrap whole char.
        check("a世界", 3, 4, &[0, 2]);
    }

    #[test]
    fn trailing_space_stays_on_row() {
        // The space after "foo" fits; "bar" wraps. Break after the space (offset 4).
        check("foo bar", 5, 4, &[0, 4]);
    }

    #[test]
    fn segment_of_finds_the_row() {
        let segs = wrap_segments("ab cdefghij", 5, 4); // [0, 3, 8], line_len 11
        assert_eq!(segment_of(&segs, 11, 0), (0, 3)); // in first row
        assert_eq!(segment_of(&segs, 11, 2), (0, 3)); // the space, last char of row 0
        assert_eq!(segment_of(&segs, 11, 3), (3, 8)); // start of row 1
        assert_eq!(segment_of(&segs, 11, 7), (3, 8)); // within row 1
        assert_eq!(segment_of(&segs, 11, 8), (8, 11)); // start of row 2
        assert_eq!(segment_of(&segs, 11, 11), (8, 11)); // end-of-line caret → last row
    }
}
