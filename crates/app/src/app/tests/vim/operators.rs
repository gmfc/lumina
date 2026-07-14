//! Operator & editing-command coverage: d/c operators, text objects, counts,
//! dot-repeat + undo/redo, indent/outdent, the g-prefixed case operators, and
//! the assorted insert-entering / substitute / replace commands.

use super::*;

#[test]
fn append_and_open_line() {
    let (mut app, path) = vim_app("ab");
    keys(&mut app, "A!"); // append at end of line
    esc(&mut app);
    assert_eq!(text(&app), "ab!");
    keys(&mut app, "ox"); // open line below
    esc(&mut app);
    assert_eq!(text(&app), "ab!\nx");
    std::fs::remove_file(&path).ok();
}

#[test]
fn dw_deletes_word() {
    let (mut app, path) = vim_app("foo bar baz");
    keys(&mut app, "dw");
    assert_eq!(text(&app), "bar baz");
    std::fs::remove_file(&path).ok();
}

#[test]
fn de_deletes_through_word_end_inclusive() {
    let (mut app, path) = vim_app("foo bar");
    keys(&mut app, "de");
    assert_eq!(text(&app), " bar"); // inclusive: through the last char of 'foo'
    std::fs::remove_file(&path).ok();
}

#[test]
fn dd_deletes_line() {
    let (mut app, path) = vim_app("a\nb\nc");
    keys(&mut app, "jdd"); // move to 'b', delete it
    assert_eq!(text(&app), "a\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn count_with_operator_multiplies() {
    let (mut app, path) = vim_app("one two three four");
    keys(&mut app, "d3w");
    assert_eq!(text(&app), "four");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ciw_changes_inner_word() {
    let (mut app, path) = vim_app("foo bar baz");
    keys(&mut app, "w"); // onto 'bar'
    keys(&mut app, "ciwX");
    esc(&mut app);
    assert_eq!(text(&app), "foo X baz");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ci_paren_changes_inside_brackets() {
    let (mut app, path) = vim_app("call(a, b)");
    keys(&mut app, "f("); // land on the '(' so the pair encloses the cursor
    keys(&mut app, "ci(");
    keys(&mut app, "z");
    esc(&mut app);
    assert_eq!(text(&app), "call(z)");
    std::fs::remove_file(&path).ok();
}

#[test]
fn di_quote_deletes_inside_string() {
    let (mut app, path) = vim_app("x = \"hello\"");
    keys(&mut app, "di\"");
    assert_eq!(text(&app), "x = \"\"");
    std::fs::remove_file(&path).ok();
}

#[test]
fn dtx_deletes_until_char() {
    let (mut app, path) = vim_app("abcXdef");
    keys(&mut app, "dtX");
    assert_eq!(text(&app), "Xdef");
    std::fs::remove_file(&path).ok();
}

#[test]
fn dfx_deletes_through_char() {
    let (mut app, path) = vim_app("abcXdef");
    keys(&mut app, "dfX");
    assert_eq!(text(&app), "def");
    std::fs::remove_file(&path).ok();
}

#[test]
fn x_deletes_char_and_dollar_end() {
    let (mut app, path) = vim_app("hello");
    keys(&mut app, "x");
    assert_eq!(text(&app), "ello");
    keys(&mut app, "$x"); // to end of line, delete last char
    assert_eq!(text(&app), "ell");
    std::fs::remove_file(&path).ok();
}

#[test]
fn undo_and_redo() {
    let (mut app, path) = vim_app("hello");
    keys(&mut app, "x"); // -> "ello"
    assert_eq!(text(&app), "ello");
    keys(&mut app, "u"); // undo
    assert_eq!(text(&app), "hello");
    ctrl(&mut app, 'r'); // redo
    assert_eq!(text(&app), "ello");
    std::fs::remove_file(&path).ok();
}

#[test]
fn dot_repeats_last_change() {
    let (mut app, path) = vim_app("aaaa");
    keys(&mut app, "x"); // delete first 'a'
    assert_eq!(text(&app), "aaa");
    keys(&mut app, "."); // repeat
    keys(&mut app, "."); // repeat
    assert_eq!(text(&app), "a");
    std::fs::remove_file(&path).ok();
}

#[test]
fn dot_repeats_change_with_inserted_text() {
    let (mut app, path) = vim_app("foo foo");
    keys(&mut app, "ciwbar");
    esc(&mut app);
    assert_eq!(text(&app), "bar foo");
    keys(&mut app, "w"); // onto second 'foo'
    keys(&mut app, "."); // repeat ciw+bar
    assert_eq!(text(&app), "bar bar");
    std::fs::remove_file(&path).ok();
}

#[test]
fn r_replaces_char() {
    let (mut app, path) = vim_app("cat");
    keys(&mut app, "rb"); // replace 'c' with 'b'
    assert_eq!(text(&app), "bat");
    std::fs::remove_file(&path).ok();
}

#[test]
fn tilde_toggles_case() {
    let (mut app, path) = vim_app("abc");
    keys(&mut app, "~~"); // toggle 'a','b'
    assert_eq!(text(&app), "ABc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn join_lines() {
    let (mut app, path) = vim_app("foo\nbar");
    keys(&mut app, "J");
    assert_eq!(text(&app), "foo bar");
    std::fs::remove_file(&path).ok();
}

#[test]
fn indent_operator_on_line() {
    let (mut app, path) = vim_app("code");
    keys(&mut app, ">>");
    assert_eq!(text(&app), "    code");
    std::fs::remove_file(&path).ok();
}

#[test]
fn outdent_operator_removes_leading_indent() {
    // `<<` outdents: up to a tab-width of leading spaces, or one leading tab.
    let (mut app, path) = vim_app("        code"); // 8 spaces (two tab stops)
    keys(&mut app, "<<");
    assert_eq!(text(&app), "    code"); // removed one tab-width (4 spaces)
    std::fs::remove_file(&path).ok();

    let (mut app2, path2) = vim_app("\tcode"); // a leading tab
    keys(&mut app2, "<<");
    assert_eq!(text(&app2), "code"); // removed the tab
    std::fs::remove_file(&path2).ok();

    // Outdenting a line with no indent is a no-op.
    let (mut app3, path3) = vim_app("code");
    keys(&mut app3, "<<");
    assert_eq!(text(&app3), "code");
    std::fs::remove_file(&path3).ok();
}

#[test]
fn lowercase_uppercase_operators() {
    let (mut app, path) = vim_app("Hello World");
    keys(&mut app, "guiw"); // lowercase inner word
    assert_eq!(text(&app), "hello World");
    keys(&mut app, "wgUiw"); // uppercase the next word
    assert_eq!(text(&app), "hello WORLD");
    std::fs::remove_file(&path).ok();
}

#[test]
fn toggle_case_operator_doubled() {
    let (mut app, path) = vim_app("aBcD");
    keys(&mut app, "g~~"); // toggle case of the whole line
    assert_eq!(text(&app), "AbCd");
    std::fs::remove_file(&path).ok();
}

#[test]
fn uppercase_line_doubled() {
    let (mut app, path) = vim_app("abc");
    keys(&mut app, "gUU");
    assert_eq!(text(&app), "ABC");
    std::fs::remove_file(&path).ok();
}

#[test]
fn delete_to_start_of_file_dgg() {
    let (mut app, path) = vim_app("l0\nl1\nl2");
    keys(&mut app, "Gdgg"); // to last line, delete through first line (linewise)
    assert_eq!(text(&app), "");
    std::fs::remove_file(&path).ok();
}

#[test]
fn indent_paragraph_operator() {
    let (mut app, path) = vim_app("one\ntwo\n\nthree");
    keys(&mut app, ">ip"); // indent the first paragraph's lines
    assert_eq!(text(&app), "    one\n    two\n\nthree");
    std::fs::remove_file(&path).ok();
}

#[test]
fn delete_around_paragraph() {
    let (mut app, path) = vim_app("one\ntwo\n\nthree");
    keys(&mut app, "dap"); // delete paragraph + trailing blank
    assert_eq!(text(&app), "three");
    std::fs::remove_file(&path).ok();
}

#[test]
fn change_around_word() {
    let (mut app, path) = vim_app("foo bar");
    keys(&mut app, "daw"); // delete 'foo' and its trailing space
    assert_eq!(text(&app), "bar");
    std::fs::remove_file(&path).ok();
}

#[test]
fn brace_and_bracket_text_objects() {
    let (mut app, path) = vim_app("x{a}[b]<c>");
    keys(&mut app, "f{ci{Z"); // change inside braces
    esc(&mut app);
    assert_eq!(text(&app), "x{Z}[b]<c>");
    keys(&mut app, "f[di["); // delete inside brackets
    assert_eq!(text(&app), "x{Z}[]<c>");
    keys(&mut app, "f<ci<Q"); // change inside angles
    esc(&mut app);
    assert_eq!(text(&app), "x{Z}[]<Q>");
    std::fs::remove_file(&path).ok();
}

#[test]
fn insert_first_non_blank_and_col1() {
    let (mut app, path) = vim_app("  code");
    keys(&mut app, "$Ix"); // I inserts at first non-blank
    esc(&mut app);
    assert_eq!(text(&app), "  xcode");
    keys(&mut app, "gIy"); // gI inserts at column 1
    esc(&mut app);
    assert_eq!(text(&app), "y  xcode");
    std::fs::remove_file(&path).ok();
}

#[test]
fn open_line_above() {
    let (mut app, path) = vim_app("bottom");
    keys(&mut app, "Otop");
    esc(&mut app);
    assert_eq!(text(&app), "top\nbottom");
    std::fs::remove_file(&path).ok();
}

#[test]
fn substitute_char_and_line() {
    let (mut app, path) = vim_app("cat\ndog");
    keys(&mut app, "sX"); // substitute one char
    esc(&mut app);
    assert_eq!(text(&app), "Xat\ndog");
    keys(&mut app, "jShey"); // S changes the whole line
    esc(&mut app);
    assert_eq!(text(&app), "Xat\nhey");
    std::fs::remove_file(&path).ok();
}

#[test]
fn change_and_delete_to_eol() {
    let (mut app, path) = vim_app("hello world");
    keys(&mut app, "wCthere"); // C changes to end of line
    esc(&mut app);
    assert_eq!(text(&app), "hello there");
    keys(&mut app, "0D"); // D deletes to end of line
    assert_eq!(text(&app), "");
    std::fs::remove_file(&path).ok();
}

#[test]
fn delete_char_backward_x() {
    let (mut app, path) = vim_app("abc");
    keys(&mut app, "llX"); // on 'c', delete char before -> removes 'b'
    assert_eq!(text(&app), "ac");
    std::fs::remove_file(&path).ok();
}

#[test]
fn replace_with_newline_splits_line() {
    let (mut app, path) = vim_app("abc");
    keys(&mut app, "l"); // on 'b'
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(text(&app), "a\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn join_with_count() {
    let (mut app, path) = vim_app("a\nb\nc\nd");
    keys(&mut app, "3J"); // join 3 lines
    assert_eq!(text(&app), "a b c\nd");
    std::fs::remove_file(&path).ok();
}

#[test]
fn toggle_case_with_count() {
    let (mut app, path) = vim_app("abcd");
    keys(&mut app, "3~"); // toggle 3 chars
    assert_eq!(text(&app), "ABCd");
    std::fs::remove_file(&path).ok();
}

#[test]
fn dot_repeat_with_count() {
    let (mut app, path) = vim_app("aaaaaa");
    keys(&mut app, "x"); // delete one -> "aaaaa"
    keys(&mut app, "3."); // repeat x three times
    assert_eq!(text(&app), "aa");
    std::fs::remove_file(&path).ok();
}

#[test]
fn undo_with_count() {
    let (mut app, path) = vim_app("abcde");
    keys(&mut app, "xxx"); // three deletes -> "de"
    assert_eq!(text(&app), "de");
    keys(&mut app, "2u"); // undo twice
    assert_eq!(text(&app), "bcde");
    std::fs::remove_file(&path).ok();
}
