//! Visual & visual-line coverage: charwise (`v`) and linewise (`V`) selection,
//! text objects inside visual mode, and the operators that act on a selection
//! (delete, case, indent, replace, toggle, join, swap-ends).

use super::*;

#[test]
fn visual_select_and_delete() {
    let (mut app, path) = vim_app("hello world");
    keys(&mut app, "vll"); // select 'hel' (inclusive of cursor char)
    keys(&mut app, "d");
    assert_eq!(text(&app), "lo world");
    assert_eq!(mode(&app), Mode::Normal);
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_line_delete() {
    let (mut app, path) = vim_app("a\nb\nc");
    keys(&mut app, "Vd"); // delete the current line, linewise
    assert_eq!(text(&app), "b\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_uppercase() {
    let (mut app, path) = vim_app("hello");
    keys(&mut app, "vllU"); // uppercase 'hel'
    assert_eq!(text(&app), "HELlo");
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_text_object_and_yank() {
    let (mut app, path) = vim_app("foo bar");
    keys(&mut app, "viwd"); // visually select inner word, delete
    assert_eq!(text(&app), " bar");
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_swap_ends_and_delete() {
    let (mut app, path) = vim_app("abcde");
    keys(&mut app, "vllold"); // select, swap ends, move, delete
    assert_eq!(text(&app), "ade");
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_indent_and_lowercase() {
    let (mut app, path) = vim_app("CODE");
    keys(&mut app, "vllu"); // lowercase 'COD'
    assert_eq!(text(&app), "codE");
    keys(&mut app, "V>"); // linewise indent
    assert_eq!(text(&app), "    codE");
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_replace_and_toggle() {
    let (mut app, path) = vim_app("abcde");
    keys(&mut app, "vll"); // select 'abc'
    keys(&mut app, "rx"); // replace each with x
    assert_eq!(text(&app), "xxxde");
    keys(&mut app, "vl~"); // toggle case of 'xx'
    assert_eq!(text(&app), "XXxde");
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_join() {
    let (mut app, path) = vim_app("a\nb\nc");
    keys(&mut app, "VjJ"); // select two lines, join them
    assert_eq!(text(&app), "a b\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn visual_line_down_selection() {
    let (mut app, path) = vim_app("a\nb\nc\nd");
    keys(&mut app, "Vjd"); // linewise select two lines, delete
    assert_eq!(text(&app), "c\nd");
    std::fs::remove_file(&path).ok();
}
