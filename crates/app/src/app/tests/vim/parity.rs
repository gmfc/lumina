//! Vim parity coverage for plan §1: macros, marks, jump list, gj/gk, `=`/`gq`, tag objects.

use super::*;

#[test]
fn marks_set_and_jump_exact_and_line() {
    let (mut app, path) = vim_app("  alpha\n  beta\n  gamma");
    keys(&mut app, "j"); // line 1
    keys(&mut app, "ma"); // mark a at current col
    let marked = head(&app);
    keys(&mut app, "G"); // last line
    assert!(head(&app) > marked);
    keys(&mut app, "`a"); // exact jump
    assert_eq!(head(&app), marked);
    keys(&mut app, "gg");
    keys(&mut app, "'a"); // line jump → first non-blank of mark's line
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.char_to_line(head(&app)), 1);
    assert_eq!(head(&app), doc.line_to_char(1) + 2); // after leading spaces
    std::fs::remove_file(&path).ok();
}

#[test]
fn jump_list_ctrl_o_and_ctrl_i() {
    let (mut app, path) = vim_app("l1\nl2\nl3\nl4");
    keys(&mut app, "gg");
    let start = head(&app);
    keys(&mut app, "G"); // records a jump
    let end = head(&app);
    assert!(end > start);
    ctrl(&mut app, 'o'); // older → back toward start
    assert_eq!(head(&app), start);
    ctrl(&mut app, 'i'); // newer → end again
    assert_eq!(head(&app), end);
    // Extra Ctrl-I at the tip is a no-op (covers jump_newer empty branch).
    ctrl(&mut app, 'i');
    assert_eq!(head(&app), end);
    std::fs::remove_file(&path).ok();
}

#[test]
fn macro_record_and_replay() {
    // Record `A x<Esc>` into register a, undo, then replay with @a / @@.
    let (mut app, path) = vim_app("ab");
    keys(&mut app, "qa"); // start recording into a
    assert_eq!(
        app.editor
            .vim_view
            .as_ref()
            .and_then(|v| v.pending.as_deref()),
        Some("recording @a")
    );
    keys(&mut app, "A x"); // append space+x at EOL
    esc(&mut app);
    keys(&mut app, "q"); // stop
    assert_eq!(text(&app), "ab x");
    keys(&mut app, "u"); // undo the append
    assert_eq!(text(&app), "ab");
    keys(&mut app, "@a"); // replay
    assert_eq!(text(&app), "ab x");
    keys(&mut app, "@@"); // repeat last macro
    assert_eq!(text(&app), "ab x x");
    std::fs::remove_file(&path).ok();
}

#[test]
fn gj_gk_move_by_visual_row_under_wrap() {
    // One long line that wraps; gj should stay on the same logical line while j advances.
    let line = "word ".repeat(20);
    let (mut app, path) = vim_app(&format!("{line}\nsecond\n"));
    app.exec_id("view.toggleWrap");
    app.editor.sidebar_visible = false;
    // Force a narrow wrap width so the first logical line spans multiple visual rows.
    if let Some(doc) = app.editor.active_document_mut() {
        doc.view.wrap = true;
        doc.view.wrap_width = 20;
    }
    let start = head(&app);
    keys(&mut app, "gj");
    let after_gj = head(&app);
    assert!(
        after_gj > start,
        "gj should advance within the wrapped line"
    );
    assert_eq!(
        app.editor.active_document().unwrap().char_to_line(after_gj),
        0,
        "gj stays on logical line 0"
    );
    keys(&mut app, "gk");
    assert!(head(&app) < after_gj, "gk should move back a visual row");
    // Reset and compare with logical j/k.
    app.editor.active_document_mut().unwrap().set_caret(0);
    keys(&mut app, "j");
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .char_to_line(head(&app)),
        1,
        "bare j moves to the next logical line"
    );
    keys(&mut app, "k");
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
fn reindent_operator_copies_previous_indent() {
    let (mut app, path) = vim_app("    parent\nchild\n");
    keys(&mut app, "j");
    keys(&mut app, "==");
    assert_eq!(text(&app), "    parent\n    child\n");
    std::fs::remove_file(&path).ok();
}

#[test]
fn reindent_motion_form_equals_g() {
    // `=G` reindents from the current line through the last line.
    let (mut app, path) = vim_app("    parent\nchild\nnephew\n");
    keys(&mut app, "j"); // on child
    keys(&mut app, "=G");
    let t = text(&app);
    assert!(
        t.contains("    child"),
        "=G should reindent from caret through end: {t:?}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn gq_hard_wraps_lines() {
    let (mut app, path) = vim_app("one two three four five six seven eight nine ten\n");
    if let Some(doc) = app.editor.active_document_mut() {
        doc.view.wrap_width = 20;
    }
    keys(&mut app, "gqq"); // gq operator, then q doubles to current line
    let t = text(&app);
    assert!(
        t.lines().count() > 1,
        "gq should wrap a long line into multiple: {t:?}"
    );
    assert!(t.contains("one") && t.contains("ten"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn tag_object_dit_deletes_inner_tag() {
    let (mut app, path) = vim_app("<div>hello</div>");
    keys(&mut app, "fhdit"); // land on 'h', delete inner tag
    assert_eq!(text(&app), "<div></div>");
    std::fs::remove_file(&path).ok();
}

#[test]
fn tag_object_dat_deletes_around_tag() {
    let (mut app, path) = vim_app("x<div>hello</div>y");
    keys(&mut app, "fh"); // land on 'h'
    keys(&mut app, "dat");
    assert_eq!(text(&app), "xy");
    std::fs::remove_file(&path).ok();
}
