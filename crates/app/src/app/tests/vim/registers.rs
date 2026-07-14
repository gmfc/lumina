//! Yank / paste / register coverage: charwise & linewise yank+paste, paste
//! before/after, named registers, the append (`"A`) form, the yank register
//! (`"0`) surviving a delete, and the black-hole (`"_`) register.

use super::*;

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
fn named_register_roundtrip() {
    let (mut app, path) = vim_app("keep\nthrow");
    keys(&mut app, "\"ayy"); // yank line into register a
    keys(&mut app, "j\"ap"); // paste register a below line 2
    assert_eq!(text(&app), "keep\nthrow\nkeep");
    std::fs::remove_file(&path).ok();
}

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
