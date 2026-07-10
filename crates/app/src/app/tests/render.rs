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

    // A diagnostic on line 0 (exercises the gutter marker + underline).
    let id = app.editor.workspace.active_doc().unwrap();
    app.editor.diagnostics.insert(
        id,
        vec![editor_lsp::Diagnostic {
            line: 0,
            start_char16: 3,
            end_line: 0,
            end_char16: 6,
            severity: editor_lsp::Severity::Error,
            message: String::new(),
        }],
    );

    // Viewport taller than the 4-line doc → past-EOF tildes render too.
    let text = render_to_string(&mut app, 48, 12);
    assert!(text.contains('~'), "expected past-EOF tildes below the doc");
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
