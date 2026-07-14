//! Motion coverage: hjkl/word/find/screen/goto/paragraph/percent/scroll motions
//! plus the `*`/`#` word-under-cursor searches. Each moves the caret without
//! mutating the buffer (unless paired with an operator, which lives elsewhere).

use super::*;

#[test]
fn hjkl_moves_without_typing() {
    let (mut app, path) = vim_app("abc\ndef");
    keys(&mut app, "ll");
    assert_eq!(head(&app), 2);
    keys(&mut app, "j");
    // column preserved on the second line: 'f' is at offset 4+2 = 6
    assert_eq!(head(&app), 6);
    keys(&mut app, "h");
    assert_eq!(head(&app), 5);
    // Buffer is untouched — Normal mode doesn't insert.
    assert_eq!(text(&app), "abc\ndef");
    std::fs::remove_file(&path).ok();
}

#[test]
fn gg_and_g_navigate() {
    let (mut app, path) = vim_app("l1\nl2\nl3\nl4");
    keys(&mut app, "G"); // last line
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app)),
        3
    );
    keys(&mut app, "gg"); // first line
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app)),
        0
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn goto_line_via_count_g() {
    let (mut app, path) = vim_app("l1\nl2\nl3\nl4\nl5");
    keys(&mut app, "3G");
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app)),
        2
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn word_motions_big_and_end() {
    let (mut app, path) = vim_app("foo.bar baz.qux");
    keys(&mut app, "W"); // WORD forward: past 'foo.bar' -> 'baz.qux'
    assert_eq!(head(&app), 8);
    keys(&mut app, "B"); // WORD back
    assert_eq!(head(&app), 0);
    keys(&mut app, "e"); // end of 'foo'
    assert_eq!(head(&app), 2);
    keys(&mut app, "E"); // end of WORD 'foo.bar'
    assert_eq!(head(&app), 6);
    std::fs::remove_file(&path).ok();
}

#[test]
fn zero_caret_and_dollar_motions() {
    let (mut app, path) = vim_app("  indented text");
    keys(&mut app, "$"); // end of line, on last char
    assert_eq!(head(&app), 14);
    keys(&mut app, "0"); // absolute line start
    assert_eq!(head(&app), 0);
    keys(&mut app, "^"); // first non-blank
    assert_eq!(head(&app), 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn column_bar_motion() {
    let (mut app, path) = vim_app("abcdefgh");
    keys(&mut app, "5|"); // column 5 (1-based) -> offset 4
    assert_eq!(head(&app), 4);
    std::fs::remove_file(&path).ok();
}

#[test]
fn paragraph_and_percent_motions() {
    let (mut app, path) = vim_app("a\nb\n\nc\nd");
    keys(&mut app, "}"); // to blank line
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app)),
        2
    );
    keys(&mut app, "{"); // back
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app)),
        0
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn percent_jumps_to_bracket() {
    let (mut app, path) = vim_app("x(abc)y");
    keys(&mut app, "%"); // cursor at 'x', scans to '(' then partner ')'
    assert_eq!(head(&app), 5);
    std::fs::remove_file(&path).ok();
}

#[test]
fn screen_motions_hml_no_panic() {
    let (mut app, path) = vim_app("l0\nl1\nl2\nl3\nl4\nl5");
    keys(&mut app, "L"); // bottom of screen
    keys(&mut app, "H"); // top
    keys(&mut app, "M"); // middle
                         // Just assert the caret stayed within the document and nothing panicked.
    assert!(head(&app) <= app.editor.active_document().unwrap().len_chars());
    std::fs::remove_file(&path).ok();
}

#[test]
fn ge_moves_to_previous_word_end() {
    let (mut app, path) = vim_app("foo bar");
    keys(&mut app, "$"); // on 'r'
    keys(&mut app, "ge"); // end of previous word 'foo'
    assert_eq!(head(&app), 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn find_backward_and_repeat() {
    let (mut app, path) = vim_app("a.b.c.d");
    keys(&mut app, "$"); // on last 'd'
    keys(&mut app, "F."); // back onto a '.'
    assert_eq!(head(&app), 5);
    keys(&mut app, ";"); // repeat backward
    assert_eq!(head(&app), 3);
    keys(&mut app, ","); // reverse -> forward
    assert_eq!(head(&app), 5);
    std::fs::remove_file(&path).ok();
}

#[test]
fn till_backward_motion() {
    let (mut app, path) = vim_app("a.b.c");
    keys(&mut app, "$"); // on 'c'
    keys(&mut app, "T."); // just after previous '.'
    assert_eq!(head(&app), 4);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ctrl_scroll_moves_caret() {
    let (mut app, path) = vim_app("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7");
    ctrl(&mut app, 'd'); // half page down
    assert!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app))
            > 0
    );
    ctrl(&mut app, 'u'); // half page up
    ctrl(&mut app, 'f'); // page forward
    ctrl(&mut app, 'b'); // page back
    std::fs::remove_file(&path).ok();
}

#[test]
fn z_recenter_commands_no_panic() {
    let (mut app, path) = vim_app("l0\nl1\nl2\nl3\nl4");
    keys(&mut app, "jjzz"); // center
    keys(&mut app, "zt"); // top
    keys(&mut app, "zb"); // bottom
    assert_eq!(mode(&app), Mode::Normal);
    std::fs::remove_file(&path).ok();
}

#[test]
fn star_searches_word_under_cursor() {
    let (mut app, path) = vim_app("cat dog cat");
    keys(&mut app, "*"); // search 'cat' forward -> next occurrence
    assert_eq!(head(&app), 8);
    std::fs::remove_file(&path).ok();
}

#[test]
fn hash_searches_word_backward() {
    let (mut app, path) = vim_app("cat dog cat");
    keys(&mut app, "$"); // on last 't' of second 'cat'
    keys(&mut app, "b"); // start of second 'cat'
    keys(&mut app, "#"); // search backward -> first 'cat'
    assert_eq!(head(&app), 0);
    std::fs::remove_file(&path).ok();
}
