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
