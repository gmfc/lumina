//! Integration coverage for the Vim modal layer, driven through the real `on_key`
//! path (the same seam the terminal loop uses).

use super::*;
use crate::vim::Mode;

/// An app with the Vim layer enabled on a scratch file.
fn vim_app(contents: &str) -> (App, PathBuf) {
    let path = temp_file(contents);
    let mut app = app_with(&path);
    app.set_vim(true);
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
    app.editor.vim.as_ref().unwrap().mode
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
fn disabling_vim_restores_plain_typing() {
    let (mut app, path) = vim_app("x");
    app.set_vim(false);
    assert!(app.editor.vim.is_none());
    // With Vim off, a plain char inserts again.
    keys(&mut app, "y");
    assert!(text(&app).contains('y'));
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
