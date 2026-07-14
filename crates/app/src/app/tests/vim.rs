//! Integration coverage for the Vim modal layer, driven through the real `on_key`
//! path (the same seam the terminal loop uses).

use super::*;
use editor_plugin::VimMode as Mode;

/// An app with the Vim layer enabled on a scratch file.
fn vim_app(contents: &str) -> (App, PathBuf) {
    let path = temp_file(contents);
    let mut app = app_with(&path);
    app.exec_id("vim.enable");
    app.editor.active_document_mut().unwrap().set_caret(0);
    (app, path)
}

/// Feed a run of plain character keys (Normal/Insert mode text).
fn keys(app: &mut App, s: &str) {
    for c in s.chars() {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }
}

fn esc(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

fn ctrl(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

fn text(app: &App) -> String {
    app.editor.active_document().unwrap().to_string()
}

fn head(app: &App) -> usize {
    app.editor
        .active_document()
        .unwrap()
        .selections
        .primary()
        .head
}

fn mode(app: &App) -> Mode {
    app.editor.vim_view.as_ref().unwrap().mode
}

#[test]
fn starts_in_normal_mode() {
    let (app, path) = vim_app("hello");
    assert_eq!(mode(&app), Mode::Normal);
    std::fs::remove_file(&path).ok();
}

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
fn i_inserts_and_esc_returns_to_normal() {
    let (mut app, path) = vim_app("world");
    keys(&mut app, "i");
    assert_eq!(mode(&app), Mode::Insert);
    keys(&mut app, "hello ");
    esc(&mut app);
    assert_eq!(mode(&app), Mode::Normal);
    assert_eq!(text(&app), "hello world");
    std::fs::remove_file(&path).ok();
}

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
fn yank_and_paste() {
    let (mut app, path) = vim_app("abc");
    keys(&mut app, "yyp"); // yank line, paste below
    assert_eq!(text(&app), "abc\nabc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn char_yank_and_paste_after() {
    let (mut app, path) = vim_app("abc");
    keys(&mut app, "ylp"); // yank one char 'a', paste after cursor
                           // p pastes after the caret; caret was left at 'a', paste 'a' after -> "aabc"
    assert_eq!(text(&app), "aabc");
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
fn named_register_roundtrip() {
    let (mut app, path) = vim_app("keep\nthrow");
    keys(&mut app, "\"ayy"); // yank line into register a
    keys(&mut app, "j\"ap"); // paste register a below line 2
    assert_eq!(text(&app), "keep\nthrow\nkeep");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_goto_line() {
    let (mut app, path) = vim_app("a\nb\nc\nd");
    keys(&mut app, ":3");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
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
fn ex_substitute_global() {
    let (mut app, path) = vim_app("a a a\nb a");
    keys(&mut app, ":%s/a/X/g");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(text(&app), "X X X\nb X");
    std::fs::remove_file(&path).ok();
}

#[test]
fn search_moves_to_match() {
    let (mut app, path) = vim_app("one two three two");
    keys(&mut app, "/two");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(head(&app), 4); // first 'two'
    keys(&mut app, "n"); // next match
    assert_eq!(head(&app), 14); // second 'two'
    std::fs::remove_file(&path).ok();
}

#[test]
fn global_ctrl_s_still_saves_in_normal_mode() {
    let (mut app, path) = vim_app("data");
    keys(&mut app, "x"); // dirty the buffer
    assert!(app.editor.active_document().unwrap().dirty);
    ctrl(&mut app, 's'); // Ctrl+S falls through to the global keymap
    assert!(!app.editor.active_document().unwrap().dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ata");
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
fn disabling_vim_restores_plain_typing() {
    let (mut app, path) = vim_app("x");
    app.exec_id("vim.disable");
    assert!(app.editor.vim_view.is_none());
    // With Vim off, a plain char inserts again.
    keys(&mut app, "y");
    assert!(text(&app).contains('y'));
    std::fs::remove_file(&path).ok();
}

#[test]
fn toggle_commands_enable_and_disable() {
    let path = temp_file("hello");
    let mut app = app_with(&path); // Vim off by default
    assert!(app.editor.vim_view.is_none());
    app.exec_id("vim.enable");
    assert!(app.editor.vim_view.is_some());
    app.exec_id("vim.toggle"); // toggles off
    assert!(app.editor.vim_view.is_none());
    app.exec_id("vim.toggle"); // toggles on
    assert!(app.editor.vim_view.is_some());
    app.exec_id("vim.disable");
    assert!(app.editor.vim_view.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn status_line_shows_pending_operator_and_count() {
    let (mut app, path) = vim_app("hello world");
    keys(&mut app, "2d"); // pending: count 2 + operator d
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("2d"),
        "expected pending hint 2d in {screen:?}"
    );
    esc(&mut app);
    keys(&mut app, ":wq"); // command line hint
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains(":wq"),
        "expected command hint in {screen:?}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn status_line_shows_mode_badge() {
    let (mut app, path) = vim_app("hello world");
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("NORMAL"),
        "expected NORMAL badge in {screen:?}"
    );
    keys(&mut app, "v");
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("VISUAL"),
        "expected VISUAL badge in {screen:?}"
    );
    keys(&mut app, "i"); // enters insert? no — in visual, 'i' starts a text object prefix
    esc(&mut app);
    keys(&mut app, "i"); // now Normal -> Insert
    let screen = render_to_string(&mut app, 60, 8);
    assert!(
        screen.contains("INSERT"),
        "expected INSERT badge in {screen:?}"
    );
    std::fs::remove_file(&path).ok();
}

// --- extended motion coverage ---------------------------------------------

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

// --- operator + g-prefix coverage -----------------------------------------

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

// --- insert-entering command coverage -------------------------------------

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

// --- paste / register coverage --------------------------------------------

#[test]
fn paste_before_linewise_and_char() {
    let (mut app, path) = vim_app("a\nb");
    keys(&mut app, "yyjP"); // yank line 'a', down to 'b', paste above
    assert_eq!(text(&app), "a\na\nb");
    std::fs::remove_file(&path).ok();
}

#[test]
fn paste_char_before() {
    let (mut app, path) = vim_app("bc");
    keys(&mut app, "ylP"); // yank 'b', paste before caret
    assert_eq!(text(&app), "bbc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn yank_register_survives_delete() {
    let (mut app, path) = vim_app("foo\nbar");
    keys(&mut app, "yy"); // yank "foo" into unnamed + "0
    keys(&mut app, "jdd"); // delete "bar" (clobbers unnamed, not "0)
    keys(&mut app, "\"0p"); // paste "0 -> "foo"
    assert_eq!(text(&app), "foo\nfoo");
    std::fs::remove_file(&path).ok();
}

#[test]
fn uppercase_register_appends() {
    let (mut app, path) = vim_app("one\ntwo\nx");
    keys(&mut app, "\"ayy"); // yank "one" into a
    keys(&mut app, "j\"Ayy"); // append "two" to a
    keys(&mut app, "j\"ap"); // paste a below 'x'
    assert_eq!(text(&app), "one\ntwo\nx\none\ntwo");
    std::fs::remove_file(&path).ok();
}

#[test]
fn blackhole_register_preserves_yank() {
    let (mut app, path) = vim_app("keep me");
    keys(&mut app, "yiw"); // yank "keep"
    keys(&mut app, "w\"_diw"); // delete "me" into the black hole
    keys(&mut app, "p"); // paste the still-held "keep"
    assert!(text(&app).contains("keep"));
    std::fs::remove_file(&path).ok();
}

// --- scroll / z / dot / undo coverage -------------------------------------

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

// --- visual mode coverage -------------------------------------------------

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

// --- ex command coverage --------------------------------------------------

#[test]
fn ex_write_saves() {
    let (mut app, path) = vim_app("data");
    keys(&mut app, "x"); // dirty
    keys(&mut app, ":w");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.editor.active_document().unwrap().dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ata");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_quit_closes_tab() {
    let (mut app, path) = vim_app("bye");
    keys(&mut app, ":q");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.editor.workspace.tabs.len(), 0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_force_quit_discards() {
    let (mut app, path) = vim_app("bye");
    keys(&mut app, "x"); // dirty it
    keys(&mut app, ":q!");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.editor.workspace.tabs.len(), 0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_quit_all_sets_quit() {
    let (mut app, path) = vim_app("x");
    keys(&mut app, ":qa");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.quit);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_unknown_command_reports() {
    let (mut app, path) = vim_app("x");
    keys(&mut app, ":frobnicate");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app
        .editor
        .status_message
        .as_deref()
        .unwrap_or_default()
        .contains("Not an editor command"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_substitute_current_line() {
    let (mut app, path) = vim_app("a a a\nb b");
    keys(&mut app, ":s/a/Z/g"); // current line only
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(text(&app), "Z Z Z\nb b");
    std::fs::remove_file(&path).ok();
}

#[test]
fn ex_command_backspace_closes_when_empty() {
    let (mut app, path) = vim_app("x");
    keys(&mut app, ":");
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    // Command line closed; a subsequent motion works in Normal mode again.
    keys(&mut app, "l");
    assert_eq!(mode(&app), Mode::Normal);
    std::fs::remove_file(&path).ok();
}

// --- search coverage ------------------------------------------------------

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

#[test]
fn search_backward_and_prev() {
    let (mut app, path) = vim_app("x y x y x");
    keys(&mut app, "$"); // on last 'x'
    keys(&mut app, "?x"); // search backward for 'x'
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(head(&app), 4); // middle 'x'
    keys(&mut app, "N"); // reverse -> forward -> last 'x'
    assert_eq!(head(&app), 8);
    std::fs::remove_file(&path).ok();
}

#[test]
fn renders_without_panic_in_all_modes() {
    let (mut app, path) = vim_app("alpha beta\ngamma delta");
    keys(&mut app, "v$"); // charwise visual to end of line
    let _ = render_to_string(&mut app, 40, 6);
    esc(&mut app);
    keys(&mut app, "Vj"); // linewise visual across two lines
    let _ = render_to_string(&mut app, 40, 6);
    esc(&mut app);
    keys(&mut app, ":"); // command line open
    let _ = render_to_string(&mut app, 40, 6);
    esc(&mut app);
    keys(&mut app, "/beta"); // search line open
    let _ = render_to_string(&mut app, 40, 6);
    std::fs::remove_file(&path).ok();
}
