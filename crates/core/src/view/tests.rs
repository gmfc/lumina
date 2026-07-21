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
        scroll_col: 0,
        tab_width: tab,
        height: 100,
        wrap: false,
        wrap_width: 0,
        scroll_sub: 0,
    }
}

/// A wrapped-pane geometry for the visual-layout tests.
fn wgeo(gutter: u16, scroll_line: usize, scroll_sub: usize, wrap_width: usize) -> PaneGeometry {
    PaneGeometry {
        origin_x: 0,
        origin_y: 0,
        gutter,
        scroll_line,
        scroll_col: 0,
        tab_width: 4,
        height: 100,
        wrap: true,
        wrap_width,
        scroll_sub,
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

// --- soft-wrap visual layout ----------------------------------------------

#[test]
fn wrapped_click_maps_to_the_visual_row() {
    // "aaaa bbbb cccc" @ width 5 → rows "aaaa " (0..5), "bbbb " (5..10), "cccc" (10..14).
    let doc = Document::from_str("aaaa bbbb cccc");
    let g = wgeo(0, 0, 0, 5);
    assert_eq!(screen_to_char(&doc, &g, 0, 0), Some(0)); // row 0, col 0
    assert_eq!(screen_to_char(&doc, &g, 2, 1), Some(7)); // row 1, col 2 = char 5+2
    assert_eq!(screen_to_char(&doc, &g, 3, 2), Some(13)); // row 2, col 3 = char 10+3
                                                          // A click past a row's text clamps to that row's end.
    assert_eq!(screen_to_char(&doc, &g, 40, 2), Some(14)); // past end of last row → line end
}

#[test]
fn char_to_screen_round_trips_under_wrap() {
    // For every char offset, char_to_screen then screen_to_char returns the same offset, across a
    // multi-line doc with a wrapped long line and a wide char.
    let doc = Document::from_str("aaaa bbbb cccc\nshort\n世界 wide line here");
    let g = wgeo(4, 0, 0, 6);
    for off in 0..=doc.len_chars() {
        if let Some((x, y)) = char_to_screen(&doc, &g, off) {
            let back = screen_to_char(&doc, &g, x, y);
            // A caret at a segment boundary renders at the next row's start, which maps back to the
            // same offset; end-of-line offsets map back to the line end. Accept the identity.
            assert_eq!(
                back,
                Some(off),
                "round trip failed at offset {off} -> ({x},{y})"
            );
        }
    }
}

#[test]
fn visual_rows_honors_scroll_sub() {
    // Start one visual row down into the first (wrapped) logical line.
    let doc = Document::from_str("aaaa bbbb cccc\nnext");
    let rows = visual_rows(&doc, 5, 4, 0, 1, 10);
    assert_eq!(rows[0].line, 0);
    assert_eq!((rows[0].start, rows[0].end), (5, 10)); // second visual row of line 0
    assert!(
        !rows[0].first,
        "a continuation row does not carry the line number"
    );
    // Last logical line follows after line 0's remaining segments.
    assert!(rows.iter().any(|r| r.line == 1 && r.first));
}

#[test]
fn wrapped_scroll_keeps_caret_visible() {
    // 6 logical lines, each wrapping to ~2 visual rows at width 5.
    let doc = Document::from_str(&"aaaa bbbb\n".repeat(6));
    // Caret at document start → anchor stays at the top.
    assert_eq!(wrapped_scroll_anchor(&doc, 0, 4, 5, 4, 0, 0), (0, 0));
    // Caret on the last line, tiny viewport → anchor scrolls down so the caret is visible.
    let last = doc.len_chars();
    let (sl, ss) = wrapped_scroll_anchor(&doc, last, 4, 5, 4, 0, 0);
    // The caret's visual row must fall within the [sl,ss)+height window.
    let rows = visual_rows(&doc, 5, 4, sl, ss, 4);
    let (cl, cc) = doc.char_to_line_col(last);
    assert!(
        rows.iter()
            .any(|r| r.line == cl && cc >= r.start && cc <= r.end),
        "caret line {cl} not visible in rows {rows:?}"
    );
}

// --- horizontal scroll (long lines) ---------------------------------------

fn hgeo(gutter: u16, scroll_col: usize) -> PaneGeometry {
    PaneGeometry {
        origin_x: 0,
        origin_y: 0,
        gutter,
        scroll_line: 0,
        scroll_col,
        tab_width: 4,
        height: 100,
        wrap: false,
        wrap_width: 0,
        scroll_sub: 0,
    }
}

#[test]
fn screen_click_accounts_for_hscroll() {
    let doc = Document::from_str("abcdefghij");
    // Scrolled right by 3 columns: the leftmost text cell now shows char 'd' (index 3).
    let g = hgeo(4, 3);
    assert_eq!(screen_to_char(&doc, &g, 4, 0), Some(3));
    assert_eq!(screen_to_char(&doc, &g, 6, 0), Some(5));
}

#[test]
fn char_to_screen_hidden_when_scrolled_off_left() {
    let doc = Document::from_str("abcdefghij");
    let g = hgeo(4, 3);
    // Chars 0..3 are scrolled off the left edge -> not on screen.
    for off in 0..3 {
        assert_eq!(char_to_screen(&doc, &g, off), None);
    }
    // Char 3 sits at the first visible text cell (x = gutter).
    assert_eq!(char_to_screen(&doc, &g, 3), Some((4, 0)));
    assert_eq!(char_to_screen(&doc, &g, 5), Some((6, 0)));
}

#[test]
fn hscroll_round_trips_through_screen() {
    let doc = Document::from_str("the quick brown fox jumps");
    let g = hgeo(4, 7);
    for off in 0..doc.line_len_chars(0) {
        if let Some((x, y)) = char_to_screen(&doc, &g, off) {
            assert_eq!(screen_to_char(&doc, &g, x, y), Some(off));
        }
    }
}

#[test]
fn scroll_to_col_follows_caret_both_edges() {
    let mut v = ViewState::default();
    // Caret near the start keeps the view pinned to column 0.
    v.scroll_to_col(0, 20);
    assert_eq!(v.scroll_col, 0);
    // Caret past the right edge scrolls right so it stays visible.
    v.scroll_to_col(60, 20);
    assert!(v.scroll_col > 0);
    assert!(60 >= v.scroll_col && 60 < v.scroll_col + 20);
    // Caret back near the start scrolls left, returning to 0 at the very start.
    v.scroll_to_col(0, 20);
    assert_eq!(v.scroll_col, 0);
}

#[test]
fn scroll_to_col_noop_on_zero_width() {
    let mut v = ViewState {
        scroll_col: 5,
        ..Default::default()
    };
    v.scroll_to_col(100, 0);
    assert_eq!(v.scroll_col, 5);
}

#[test]
fn char_to_screen_inverts_click() {
    let doc = Document::from_str("abc\n\tdef\n世x");
    let g = PaneGeometry {
        origin_x: 2,
        origin_y: 1,
        gutter: 4,
        scroll_line: 0,
        scroll_col: 0,
        tab_width: 4,
        height: 100,
        wrap: false,
        wrap_width: 0,
        scroll_sub: 0,
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
