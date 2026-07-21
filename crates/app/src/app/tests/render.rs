use super::*;

#[test]
fn published_decoration_layer_paints_and_reverts() {
    use editor_plugin::{Decoration, DecorationSet, Host};
    use ratatui::style::Color;
    use ratatui::{backend::TestBackend, Terminal};

    // A .txt buffer (no syntax highlighting), so the only background tint can come from the
    // decoration layer we publish — an unambiguous signal.
    let path = temp_file("hello world");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let id = app.editor.workspace.active_doc().unwrap();

    let find_bg = Color::Rgb(90, 74, 30);
    let has_find_bg = |app: &mut App| -> bool {
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.symbol() == "h" && c.bg == find_bg)
    };

    // Nothing published yet: no cell carries the find background.
    assert!(
        !has_find_bg(&mut app),
        "no find bg before a layer is published"
    );

    // Publish a find-match highlight over "hello" (chars 0..5) through the Host port.
    app.editor.set_decorations(
        id,
        "find.match",
        DecorationSet::spans(vec![Decoration::new((0, 5), "find.match")]),
    );
    assert!(
        has_find_bg(&mut app),
        "the 'h' cell should carry the find-match background once the layer is published"
    );

    // Clearing the layer reverts the render to no tint (proves clear_decorations wipes it).
    app.editor.clear_decorations(id, "find.match");
    assert!(
        !has_find_bg(&mut app),
        "clearing the layer must remove the decoration from the render"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn rust_file_gets_syntax_highlighting() {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_hl_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn main() {\n    let x = 42;\n}\n").unwrap();
    let mut app = app_with(&path);
    app.page_height = 24;
    app.editor.update_highlights(app.page_height);
    let id = app.editor.workspace.active_doc().unwrap();
    let hl = app.editor.highlighters.get(&id).expect("no highlighter");
    assert!(
        hl.line_spans(0)
            .iter()
            .any(|s| s.capture.starts_with("keyword")),
        "expected a keyword span on the fn line"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn renders_editor_with_all_decorations() {
    // A .rs file (so syntax highlighting runs) with a tab, a wide char, and a
    // repeated word to exercise selection, multi-cursor, find and diagnostics paths.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_render_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn foo() {\n\tlet w = foo; // 世界\n    foo();\n}\n").unwrap();
    let mut app = app_with(&path);
    app.page_height = 12;
    app.editor.update_highlights(app.page_height);

    // Non-empty selection + a secondary caret (exercises selection-bg + secondary-cursor).
    app.editor.active_document_mut().unwrap().set_caret(0);
    app.exec_id("cursor.addBelow");
    app.dispatch(Command::SelectWord);

    // Active find with matches (exercises the match-highlight decoration path).
    app.exec_id("search.find");
    for c in "foo".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }

    // A diagnostic on line 0 → the `diagnostics` plugin publishes the `lsp.diag` decoration layer
    // (gutter marker + underline).
    let id = app.editor.workspace.active_doc().unwrap();
    feed_diagnostics(&mut app, id, vec![diag(0, 3, 0, 6, "")]);

    // Viewport taller than the 4-line doc → past-EOF tildes render too.
    let text = render_to_string(&mut app, 48, 12);
    assert!(text.contains('~'), "expected past-EOF tildes below the doc");
    assert!(text.contains('E'), "the diagnostic gutter marker renders");
    std::fs::remove_file(&path).ok();
}

#[test]
fn soft_wrap_splits_a_long_line_across_rows() {
    // A long line wrapped at the live pane width (~13 cells here) lands its tail on later rows.
    let path = temp_file("hello world foo bar baz qux");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.editor.active_document_mut().unwrap().view.wrap = true;
    // Narrow pane forces the wrap (the renderer uses the live pane text width).
    let rows = screen_rows(&render_to_string(&mut app, 16, 8), 16);
    // Find the editor content (below the tab bar / chrome).
    let i = rows
        .iter()
        .position(|r| r.contains("hello"))
        .expect("first wrapped segment renders");
    // The line wrapped: its tail ("qux") is NOT on the first row, but on a later one.
    assert!(
        !rows[i].contains("qux"),
        "tail should wrap off row 0: {:?}",
        rows[i]
    );
    assert!(
        rows[i + 1..].iter().any(|r| r.contains("qux")),
        "tail 'qux' renders on a continuation row: {:?}",
        &rows[i..]
    );
    // Only the first visual row carries the line number "1"; continuations blank the gutter.
    assert!(
        rows[i].trim_start().starts_with('1'),
        "gutter: {:?}",
        rows[i]
    );
    assert!(
        !rows[i + 1].trim_start().starts_with('1'),
        "continuation gutter should be blank: {:?}",
        rows[i + 1]
    );
    // A row past the wrapped line shows the EOF tilde.
    assert!(
        rows.iter().any(|r| r.contains('~')),
        "past-EOF tilde: {:?}",
        rows
    );
}

#[test]
fn wrap_scrolls_the_viewport_to_follow_the_caret() {
    // Many wrapping lines + a short pane → moving the caret to the end must scroll the visual-row
    // anchor down so the caret stays visible (exercises the wrap branch of ensure_cursor_visible).
    let path = temp_file(&"a fairly long line that wraps a couple of times\n".repeat(30));
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.editor.wrap_enabled = true;
    app.page_height = 6;
    // A first render populates the laid-out regions; refresh_viewport then mirrors wrap + width.
    let _ = render_to_string(&mut app, 24, 8);
    app.refresh_viewport();
    assert!(
        app.editor.active_document().unwrap().view.wrap,
        "wrap is active on the doc"
    );

    // Caret to end-of-document → the anchor scrolls down to keep it visible.
    let end = app.editor.active_document().unwrap().len_chars();
    app.editor.active_document_mut().unwrap().set_caret(end);
    app.ensure_cursor_visible();
    assert!(
        app.editor.active_document().unwrap().view.scroll_line > 0,
        "the viewport scrolled down to follow the caret under wrap"
    );

    // Back to the top → the anchor returns to (0, 0).
    app.editor.active_document_mut().unwrap().set_caret(0);
    app.ensure_cursor_visible();
    let v = &app.editor.active_document().unwrap().view;
    assert_eq!((v.scroll_line, v.scroll_sub), (0, 0), "returns to the top");
    std::fs::remove_file(&path).ok();
}

#[test]
fn toggle_wrap_flips_global_state_and_mirrors_every_doc() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    assert!(!app.editor.wrap_enabled, "off by default");

    app.dispatch(Command::ToggleWrap);
    assert!(app.editor.wrap_enabled, "toggle turns wrap on");
    assert!(
        app.editor.workspace.documents.get(id).unwrap().view.wrap,
        "the global flag is mirrored onto the doc"
    );

    app.dispatch(Command::ToggleWrap);
    assert!(!app.editor.wrap_enabled, "toggle turns it back off");
    assert!(!app.editor.workspace.documents.get(id).unwrap().view.wrap);
    std::fs::remove_file(&path).ok();
}

/// Split the flat cell-symbol string from `render_to_string` into `w`-wide screen rows.
fn screen_rows(flat: &str, w: usize) -> Vec<String> {
    flat.chars()
        .collect::<Vec<_>>()
        .chunks(w)
        .map(|c| c.iter().collect())
        .collect()
}

#[test]
fn soft_wrap_off_keeps_one_row_per_line() {
    // With wrap off, the same long line occupies a single row (the tail is hscroll-clipped).
    let path = temp_file("hello world foo bar baz qux");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let rows = screen_rows(&render_to_string(&mut app, 30, 8), 30);
    let i = rows
        .iter()
        .position(|r| r.contains("hello world"))
        .expect("the line renders");
    // The tail is hscroll-clipped onto the same row — "qux" does NOT get its own row.
    assert!(
        !rows[i].contains("qux"),
        "tail should be clipped, not wrapped: {:?}",
        rows[i]
    );
    // The next row is already past EOF (the doc has one line), so a tilde.
    assert!(
        rows[i + 1].contains('~'),
        "row+1 should be EOF: {:?}",
        rows[i + 1]
    );
}

#[test]
fn diagnostics_publish_as_a_decoration_layer() {
    // Data-level guard for the `diagnostics` plugin's decoration build (the render path has no
    // color assertion): an Error on line 0 cols 3..6 and a Warning on line 1 produce two underline
    // spans + two gutter marks with the right severity keys.
    let path = temp_file("let x = 1;\nlonger line here\n");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    feed_diagnostics(
        &mut app,
        id,
        vec![
            editor_plugin::LspDiagnostic {
                line: 0,
                start_char16: 3,
                end_line: 0,
                end_char16: 6,
                severity: editor_plugin::LspSeverity::Error,
                message: String::new(),
                source: None,
                code: None,
            },
            editor_plugin::LspDiagnostic {
                line: 1,
                start_char16: 0,
                end_line: 1,
                end_char16: 4,
                severity: editor_plugin::LspSeverity::Warning,
                message: String::new(),
                source: None,
                code: None,
            },
        ],
    );
    let layer = app.editor.decorations[&id]
        .get("lsp.diag")
        .expect("lsp.diag layer published");
    // Underline spans: line 0 col 3..6 → char offsets (3,6); line 1 col 0..4 → line start (11) + 0..4.
    assert!(layer
        .spans
        .iter()
        .any(|d| d.range == (3, 6) && d.style == "lsp.diag.error"));
    assert!(layer.spans.iter().any(|d| d.style == "lsp.diag.warning"));
    // One gutter mark per affected line, carrying the severity glyph + mark style.
    assert!(layer
        .gutter
        .iter()
        .any(|m| m.line == 0 && m.glyph == 'E' && m.style == "lsp.diag.mark.error"));
    assert!(layer
        .gutter
        .iter()
        .any(|m| m.line == 1 && m.glyph == 'W' && m.style == "lsp.diag.mark.warning"));

    // An empty diagnostics push drops the layer.
    feed_diagnostics(&mut app, id, vec![]);
    assert!(app
        .editor
        .decorations
        .get(&id)
        .map(|l| !l.contains_key("lsp.diag"))
        .unwrap_or(true));
    std::fs::remove_file(&path).ok();
}

#[test]
fn long_line_scrolls_horizontally_to_follow_caret() {
    // A line far wider than the viewport, with distinct markers at each end.
    let path = temp_file("STARThere 1111111111 2222222222 3333333333 4444444444 ENDhere\nsecond\n");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false; // give the editor the full width
    app.page_height = 10;

    // A first render populates the laid-out regions that ensure_cursor_visible reads.
    let text = render_to_string(&mut app, 40, 12);
    assert!(
        text.contains("STARThere"),
        "caret at start: the line's head is visible before scrolling"
    );

    // Move the caret to end-of-line; the viewport should scroll right to follow it.
    let end = app.editor.active_document().unwrap().line_len_chars(0);
    app.editor.active_document_mut().unwrap().set_caret(end);
    app.ensure_cursor_visible();
    assert!(
        app.editor.active_document().unwrap().view.scroll_col > 0,
        "a long line should scroll horizontally to keep the caret visible"
    );

    let text = render_to_string(&mut app, 40, 12);
    assert!(
        text.contains("ENDhere"),
        "the caret end of the long line is now visible"
    );
    assert!(
        !text.contains("STARThere"),
        "the head of the long line is scrolled off the left edge"
    );

    // Moving back to the line start scrolls the view all the way back to column 0.
    app.editor.active_document_mut().unwrap().set_caret(0);
    app.ensure_cursor_visible();
    assert_eq!(
        app.editor.active_document().unwrap().view.scroll_col,
        0,
        "returning to the line start resets the horizontal scroll"
    );
    let text = render_to_string(&mut app, 40, 12);
    assert!(text.contains("STARThere"), "the head is visible again");

    std::fs::remove_file(&path).ok();
}

#[test]
fn wheel_scroll_moves_the_view_past_the_caret() {
    // Regression: the per-tick viewport clamp used to snap the scroll back to the caret, so once
    // the caret reached the top/bottom edge you could no longer scroll. `refresh_viewport` now
    // re-clamps only when the caret actually moves, so a wheel scroll moves the view freely.
    let path = temp_file(&"line\n".repeat(200));
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.page_height = 20;
    let _ = render_to_string(&mut app, 40, 22); // populate laid-out regions

    // Baseline: caret at line 0, view at the top.
    app.refresh_viewport();
    assert_eq!(app.editor.active_document().unwrap().view.scroll_line, 0);

    // Scroll the view down without touching the caret — the clamp must not fight it.
    app.scroll_editor(40);
    app.refresh_viewport();
    assert_eq!(
        app.editor.active_document().unwrap().view.scroll_line,
        40,
        "a wheel scroll must move the view past the caret, not snap back to it"
    );

    // Moving the caret (to line 100) re-clamps the view to keep it visible.
    app.editor.active_document_mut().unwrap().set_caret(5 * 100);
    app.refresh_viewport();
    assert!(
        app.editor.active_document().unwrap().view.scroll_line > 40,
        "moving the caret should scroll the view to follow it"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn renders_welcome_when_no_document_is_open() {
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.dispatch(Command::CloseTab); // close the only (clean) tab
    assert!(app.editor.active_document().is_none());
    let text = render_to_string(&mut app, 40, 10);
    assert!(text.contains("lumina"), "welcome screen shows the app name");
    std::fs::remove_file(&path).ok();
}

#[test]
fn welcome_screen_lists_main_commands_and_banner() {
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.dispatch(Command::CloseTab);
    // Roomy pane (past the 30-col sidebar): the block banner, the two-column command grid,
    // and the palette footer all render.
    let text = render_to_string(&mut app, 120, 30);
    assert!(text.contains("█"), "block-letter LUMINA banner is drawn");
    assert!(
        text.contains("Command Palette"),
        "surfaces the palette hint"
    );
    assert!(
        text.contains("Go to Definition"),
        "surfaces a navigation hint"
    );
    // Newer features are surfaced too: the Settings tab and LSP symbol rename.
    assert!(text.contains("Settings"), "surfaces the Settings tab");
    assert!(text.contains("Rename Symbol"), "surfaces LSP rename");
    // The footer points palette-only functionality (Vim, themes) at the command palette.
    assert!(
        text.contains("Vim mode") && text.contains("Ctrl+Shift+P"),
        "footer points Vim/theme functionality at the palette"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn welcome_screen_reflects_remapped_binding() {
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.dispatch(Command::CloseTab);
    // Simulate a user config that rebinds "Go to Line" to a distinctive chord.
    app.keymap = crate::keymap::Keymap::from_pairs([("alt+shift+g", "view.gotoLine")]);
    let text = render_to_string(&mut app, 90, 24);
    assert!(
        text.contains("Alt+G"),
        "welcome screen shows the remapped key, not the default Ctrl+G"
    );
    std::fs::remove_file(&path).ok();
}

// ---- mouse routing -------------------------------------------------------

#[test]
fn diagnostics_publish_the_active_doc_error_warning_count() {
    // The `diagnostics` plugin publishes the active doc's counts as "<errors> <warnings>" under
    // `lsp.diag.count`, for the footer LSP badge.
    let path = temp_file("line one\nline two\n");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    let warn = editor_plugin::LspDiagnostic {
        severity: editor_plugin::LspSeverity::Warning,
        ..diag(1, 0, 1, 4, "w")
    };
    // An info diagnostic must not count toward the error/warning badge.
    let info = editor_plugin::LspDiagnostic {
        severity: editor_plugin::LspSeverity::Info,
        ..diag(0, 9, 0, 10, "i")
    };
    feed_diagnostics(
        &mut app,
        id,
        vec![diag(0, 0, 0, 4, "e1"), diag(0, 5, 0, 8, "e2"), warn, info],
    );
    assert_eq!(
        app.editor
            .status_items
            .get("lsp.diag.count")
            .map(String::as_str),
        Some("2 1"),
        "publishes '<errors> <warnings>' for the active doc"
    );
    // Clearing diagnostics empties the count (footer badge disappears).
    feed_diagnostics(&mut app, id, vec![]);
    assert_eq!(
        app.editor
            .status_items
            .get("lsp.diag.count")
            .map(String::as_str),
        Some(""),
        "a clean doc publishes an empty count"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn footer_shows_the_lsp_status_indicator() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    // Nothing mirrored yet → no LSP segment in the status bar.
    assert!(
        !render_to_string(&mut app, 100, 6).contains("LSP"),
        "no LSP indicator before health is published"
    );

    // A 'ready' server with 2 errors → the ● LSP indicator + the ✗2 badge render.
    app.editor
        .status_items
        .insert("lsp.health".into(), "ready".into());
    app.editor
        .status_items
        .insert("lsp.diag.count".into(), "2 1".into());
    let bar = render_to_string(&mut app, 100, 6);
    assert!(bar.contains("LSP"), "the ready indicator shows");
    assert!(bar.contains("✗2"), "the error badge shows the error count");

    // Errors take priority over warnings; a warnings-only doc shows the ⚠ badge.
    app.editor
        .status_items
        .insert("lsp.diag.count".into(), "0 3".into());
    assert!(
        render_to_string(&mut app, 100, 6).contains("⚠3"),
        "a warnings-only doc shows the warning badge"
    );

    // 'starting' health shows the spinner + LSP indicator.
    app.editor
        .status_items
        .insert("lsp.health".into(), "starting".into());
    app.editor.status_items.remove("lsp.diag.count");
    assert!(
        render_to_string(&mut app, 100, 6).contains("LSP"),
        "the starting/spinner indicator shows"
    );

    // 'error' health shows the ✗ LSP indicator even with no diagnostics.
    app.editor
        .status_items
        .insert("lsp.health".into(), "error".into());
    assert!(
        render_to_string(&mut app, 100, 6).contains("LSP"),
        "the error indicator shows"
    );
    std::fs::remove_file(&path).ok();
}
