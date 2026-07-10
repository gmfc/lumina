//! Render-path coverage for the floating overlays, popups, and pickers. The base render
//! tests draw a plain editor; these open each transient UI surface and render it, exercising
//! `ui::overlays` and `ui::pickers` (which are otherwise never hit by the harness).

use super::*;

/// The completion popup draws, including an item that carries a `detail` label.
#[test]
fn renders_completion_popup() {
    let path = temp_file("pr\nprintln\n");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.open_completion(vec![
        editor_lsp::CompletionItem {
            label: "print".into(),
            detail: Some("macro".into()),
            insert_text: "print".into(),
            kind: Some(3),
        },
        ci("println", 3),
        ci("procedure", 3),
    ]);
    assert!(app.editor.completion.is_some());
    let out = render_to_string(&mut app, 100, 24);
    assert!(out.contains("print"));
    std::fs::remove_file(&path).ok();
}

/// The command-palette picker draws its query line and ranked list.
#[test]
fn renders_command_palette() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.exec_id("view.commandPalette");
    assert!(app.editor.picker.is_some());
    let out = render_to_string(&mut app, 100, 30);
    assert!(out.contains("File: Save"));
    std::fs::remove_dir_all(&dir).ok();
}

/// The goto-line picker draws with its `:` prompt cursor.
#[test]
fn renders_goto_line_picker() {
    let path = temp_file("a\nb\nc\n");
    let mut app = app_with(&path);
    app.open_goto_line();
    assert!(app.editor.picker.is_some());
    let _ = render_to_string(&mut app, 100, 20);
    std::fs::remove_file(&path).ok();
}

/// The project-search panel draws its grouped results after a real search completes.
#[test]
fn renders_search_panel_with_results() {
    let dir = temp_dir_with_files();
    std::fs::write(dir.join("a.txt"), "alpha match here\nsecond alpha line").unwrap();
    let mut app = app_with(&dir);
    app.open_search();
    for c in "alpha".chars() {
        app.search_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.search_key(KeyEvent::from(KeyCode::Enter)); // run
    for _ in 0..200 {
        app.drain_workers();
        if app.search().map(|s| !s.running).unwrap_or(true) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let out = render_to_string(&mut app, 120, 40);
    assert!(out.contains("Search:"));
    std::fs::remove_dir_all(&dir).ok();
}

/// The find widget draws in both find and replace modes, and with live match counts.
#[test]
fn renders_find_and_replace_widget() {
    let path = temp_file("abc abc abc\n");
    let mut app = app_with(&path);
    // Plain find with a query that matches → exercises the match-count branch (via the plugin's
    // generic prompt, rendered by render_prompt).
    app.exec_id("search.find");
    for c in "abc".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let out = render_to_string(&mut app, 100, 20);
    assert!(out.contains("Find"));
    // Replace mode draws the extra "Repl" line.
    let mut app2 = app_with(&path);
    app2.exec_id("search.replace");
    let out2 = render_to_string(&mut app2, 100, 20);
    assert!(out2.contains("Repl"));
    std::fs::remove_file(&path).ok();
}

/// Each modal overlay variant renders its box.
#[test]
fn renders_each_overlay_variant() {
    use crate::editor::Overlay;
    let path = temp_file("hello world\n");

    let mut app = app_with(&path);
    app.editor.overlay = Some(Overlay::ConfirmClose { tab: 0 });
    assert!(render_to_string(&mut app, 100, 20).contains("Save & close"));

    let mut app = app_with(&path);
    app.editor.overlay = Some(Overlay::Info("hover line one\nhover line two".into()));
    assert!(render_to_string(&mut app, 100, 20).contains("Hover"));

    let mut app = app_with(&path);
    app.editor.overlay = Some(Overlay::RenameInput {
        path: path.clone(),
        language: "rust".into(),
        line: 0,
        character: 0,
        buffer: "newName".into(),
    });
    assert!(render_to_string(&mut app, 100, 20).contains("Rename"));

    let mut app = app_with(&path);
    app.editor.overlay = Some(Overlay::SaveAsInput {
        buffer: "out.rs".into(),
    });
    assert!(render_to_string(&mut app, 100, 20).contains("Save As"));

    std::fs::remove_file(&path).ok();
}
