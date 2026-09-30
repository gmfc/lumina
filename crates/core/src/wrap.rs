//! Soft word-wrap layout: split a logical line into **visual rows** at word boundaries.
//!
//! Pure and terminal-free (CLAUDE.md invariant #6): the single source of truth for where a
//! wrapped line breaks. Rendering, vertical motion, and screen↔char mapping all consult
//! [`wrap_segments`], so they can never disagree about the layout. Cell widths come from the same
//! [`crate::view::char_cells`] model the renderer and column math use (tabs → next tab stop,
//! wide/CJK → 2 cells, zero-width → 1), so a wrapped row is always `<= width` cells.
//!
//! Continuation rows inherit the logical line's leading whitespace as a virtual indent (capped so
//! the remaining width stays usable). The first visual row has `indent_cells == 0`; later rows
//! share the same capped continuation indent, and wrap against `width - indent_cells`.

use crate::view::char_cells;

/// One visual-row break within a soft-wrapped logical line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrapSegment {
    /// Char offset within the line (excluding trailing newline) where this visual row begins.
    pub start: usize,
    /// Leading virtual indent in display cells for this row (`0` on the first row).
    pub indent_cells: usize,
}

/// Minimum cells left for text on a continuation row after applying continuation indent.
/// Keeps heavily-indented lines from collapsing to a one-cell wrap column.
fn min_remaining(width: usize) -> usize {
    (width / 4).max(4).min(width.saturating_sub(1).max(1))
}

/// Display cells of leading whitespace on `line`, capped so a continuation row still has a usable
/// text width when wrapped to `width`.
pub fn continuation_indent(line: &str, width: usize, tab_width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let mut raw = 0usize;
    for ch in line.chars() {
        if ch != ' ' && ch != '\t' {
            break;
        }
        raw += char_cells(ch, raw, tab_width);
    }
    raw.min(width.saturating_sub(min_remaining(width)))
}

/// Char offsets within `line` (which must **exclude** any trailing newline) where each visual row
/// begins when soft-wrapped to `width` cells. The first element always starts at `0` with
/// `indent_cells == 0`; the length is the number of visual rows (always `>= 1`, even for an empty
/// line).
///
/// Breaks at the last whitespace boundary that fits on the row; a single word wider than the row's
/// effective width is hard-broken at the cell that would overflow, so no row's text ever exceeds
/// its usable width. `width == 0` is degenerate (no usable space) and yields a single unwrapped row.
pub fn wrap_segments(line: &str, width: usize, tab_width: usize) -> Vec<WrapSegment> {
    let mut segments = vec![WrapSegment {
        start: 0,
        indent_cells: 0,
    }];
    if width == 0 {
        return segments;
    }
    let cont = continuation_indent(line, width, tab_width);
    let chars: Vec<char> = line.chars().collect();
    let mut seg_start = 0; // char index where the current visual row starts
    let mut indent = 0usize; // virtual indent of the current row
    let mut col = 0; // display column of text within the current row (past indent)
    let mut last_break: Option<usize> = None; // char index just past the last whitespace on this row
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let cells = char_cells(ch, indent + col, tab_width);
        let row_width = width.saturating_sub(indent);
        // This char would overflow the row, and the row already holds at least one char (a single
        // char wider than the whole width must still be placed, so never break an empty row).
        if col + cells > row_width && i > seg_start {
            // Prefer the last word boundary on this row; with none, hard-break before this char.
            let break_at = match last_break {
                Some(b) if b > seg_start => b,
                _ => i,
            };
            indent = cont;
            segments.push(WrapSegment {
                start: break_at,
                indent_cells: indent,
            });
            seg_start = break_at;
            last_break = None;
            // The chars carried onto the new row ([break_at, i)) start after the continuation
            // indent — recompute (tab expansion is column-relative).
            col = 0;
            for &c in &chars[break_at..i] {
                col += char_cells(c, indent + col, tab_width);
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
pub fn segment_of(
    segments: &[WrapSegment],
    line_len: usize,
    char_in_line: usize,
) -> (usize, usize) {
    // Last segment whose start is `<= char_in_line`.
    let idx = segments
        .partition_point(|s| s.start <= char_in_line)
        .saturating_sub(1);
    let start = segments[idx].start;
    let end = segments.get(idx + 1).map(|s| s.start).unwrap_or(line_len);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert the segments AND that no visual row's text exceeds its usable width.
    fn check(line: &str, width: usize, tab: usize, expected: &[(usize, usize)]) {
        let segs = wrap_segments(line, width, tab);
        let got: Vec<(usize, usize)> = segs.iter().map(|s| (s.start, s.indent_cells)).collect();
        assert_eq!(got, expected, "segments for {line:?} @ w={width}");
        let chars: Vec<char> = line.chars().collect();
        for (w, seg) in segs.iter().enumerate() {
            let start = seg.start;
            let end = segs.get(w + 1).map(|s| s.start).unwrap_or(chars.len());
            let mut col = 0;
            for &c in &chars[start..end] {
                col += char_cells(c, seg.indent_cells + col, tab);
            }
            let usable = width.saturating_sub(seg.indent_cells);
            // A row may exceed usable width only when it is a single char wider than that width.
            assert!(
                col <= usable || end - start == 1,
                "row {w} ({start}..{end}) is {col} cells > usable {usable}"
            );
        }
    }

    #[test]
    fn short_line_is_one_row() {
        check("hello", 20, 4, &[(0, 0)]);
    }

    #[test]
    fn exact_width_does_not_wrap() {
        check("abcde", 5, 4, &[(0, 0)]); // exactly 5 cells → one row, no spurious break
    }

    #[test]
    fn breaks_at_word_boundary() {
        // "the quick" (9) doesn't fit in 8 → break after "the " at offset 4.
        check("the quick", 8, 4, &[(0, 0), (4, 0)]);
    }

    #[test]
    fn over_long_word_hard_breaks() {
        // No whitespace to break on → hard break at the overflow cell.
        check("abcdefghij", 4, 4, &[(0, 0), (4, 0), (8, 0)]);
    }

    #[test]
    fn word_then_long_word() {
        // "ab cdefghij" @ 5: "ab " | "cdefg" | "hij".
        check("ab cdefghij", 5, 4, &[(0, 0), (3, 0), (8, 0)]);
    }

    #[test]
    fn empty_line_is_one_row() {
        check("", 10, 4, &[(0, 0)]);
    }

    #[test]
    fn zero_width_is_degenerate_single_row() {
        assert_eq!(
            wrap_segments("anything", 0, 4),
            vec![WrapSegment {
                start: 0,
                indent_cells: 0
            }]
        );
    }

    #[test]
    fn tab_expands_to_tab_stop() {
        // Tab at col 0 with tab_width 4 spans 4 cells; "\tab" = 4 + 1 + 1 = 6 > 5 → 'b' wraps.
        // Break candidates: the tab counts as whitespace (last_break after it), so break after tab.
        check("\tab", 5, 4, &[(0, 0), (1, 1)]);
    }

    #[test]
    fn wide_chars_count_two_cells() {
        // Each CJK char is 2 cells; width 5 fits two (4 cells) then the third wraps.
        check("世界人", 5, 4, &[(0, 0), (2, 0)]);
    }

    #[test]
    fn wide_char_at_boundary_moves_whole() {
        // "a世" = 1 + 2 = 3 fits in 3; adding another wide char would need 5 > 3 → wrap whole char.
        check("a世界", 3, 4, &[(0, 0), (2, 0)]);
    }

    #[test]
    fn trailing_space_stays_on_row() {
        // The space after "foo" fits; "bar" wraps. Break after the space (offset 4).
        check("foo bar", 5, 4, &[(0, 0), (4, 0)]);
    }

    #[test]
    fn continuation_indent_matches_leading_whitespace() {
        // "    hello world" @ width 12: first row fits "    hello " (10 cells), then "world"
        // continues with 4-cell indent.
        let line = "    hello world";
        let segs = wrap_segments(line, 12, 4);
        assert!(segs.len() >= 2, "expected wrap: {segs:?}");
        assert_eq!(segs[0].indent_cells, 0);
        assert_eq!(segs[1].indent_cells, 4);
        assert_eq!(segs[1].start, 10); // after "    hello "
    }

    #[test]
    fn continuation_indent_is_capped() {
        // A 20-space indent on width 10 would leave nothing; cap leaves a usable remainder.
        let line = format!("{}abcdef", " ".repeat(20));
        let segs = wrap_segments(&line, 10, 4);
        assert!(segs.len() > 1);
        let cont = segs[1].indent_cells;
        assert!(cont < 10, "indent {cont} must leave room");
        assert!(cont <= 10 - min_remaining(10));
    }

    #[test]
    fn segment_of_finds_the_row() {
        let segs = wrap_segments("ab cdefghij", 5, 4); // starts [0, 3, 8], line_len 11
        assert_eq!(segment_of(&segs, 11, 0), (0, 3)); // in first row
        assert_eq!(segment_of(&segs, 11, 2), (0, 3)); // the space, last char of row 0
        assert_eq!(segment_of(&segs, 11, 3), (3, 8)); // start of row 1
        assert_eq!(segment_of(&segs, 11, 7), (3, 8)); // within row 1
        assert_eq!(segment_of(&segs, 11, 8), (8, 11)); // start of row 2
        assert_eq!(segment_of(&segs, 11, 11), (8, 11)); // end-of-line caret → last row
    }
}
