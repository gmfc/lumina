use super::*;

/// Number of `find.match` decoration spans the `find` plugin has published for the active doc.
fn find_span_count(app: &App) -> usize {
    let Some(id) = app.editor.workspace.active_doc() else {
        return 0;
    };
    app.editor
        .decorations
        .get(&id)
        .and_then(|layers| layers.get("find.match"))
        .map(|set| set.spans.len())
        .unwrap_or(0)
}

#[test]
fn find_highlights_and_cycles() {
    // Drives the real registry + prompt-key path: exec_id opens the `find` plugin's widget, keys
    // route through on_key -> the plugin's on_prompt_key, and results are read off the published
    // decoration layer + the selection (the current match is the primary selection).
    let path = temp_file("foo bar foo baz foo");
    let mut app = app_with(&path);
    app.exec_id("search.find");
    for c in "foo".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    assert_eq!(find_span_count(&app), 3, "three matches highlighted");
    // Cursor started at 0 -> current match is the first, and it's the primary selection.
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_eq!((sel.from(), sel.to()), (0, 3));
    app.on_key(KeyEvent::from(KeyCode::Enter)); // next match
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_eq!((sel.from(), sel.to()), (8, 11));
    std::fs::remove_file(&path).ok();
}

#[test]
fn replace_all_is_one_undo() {
    let path = temp_file("cat cat cat");
    let mut app = app_with(&path);
    app.exec_id("search.replace");
    for c in "cat".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.on_key(KeyEvent::from(KeyCode::Tab)); // focus replace field
    for c in "dog".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.exec_id("search.replaceAll");
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "dog dog dog"
    );
    // One undo reverts the whole replace-all.
    app.on_key(KeyEvent::from(KeyCode::Esc)); // close find
    app.dispatch(Command::Undo);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "cat cat cat"
    );
    std::fs::remove_file(&path).ok();
}

/// Flatten the published "search.results" panel into one string (query header + hit rows).
fn search_panel_text(app: &App) -> String {
    app.editor
        .panels
        .get("search.results")
        .map(|p| {
            p.lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[test]
fn project_search_finds_and_opens() {
    // The `project-search` plugin owns the state; drive it through exec_id + the prompt path and
    // read results off the published panel.
    let dir = temp_dir_with_files();
    std::fs::write(dir.join("a.txt"), "find_me on this line\nother").unwrap();
    let mut app = app_with(&dir);
    app.exec_id("search.project");
    for c in "find_me".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.on_key(KeyEvent::from(KeyCode::Enter)); // run
                                                // Drain the worker channel until the job completes (panel stops showing "searching…").
    for _ in 0..200 {
        app.drain_workers();
        if !search_panel_text(&app).contains("searching") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        search_panel_text(&app).contains("find_me on this line"),
        "search results should include the matching line"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ctrl_d_selects_word_then_adds_next_match() {
    let path = temp_file("foo bar foo baz foo");
    let mut app = app_with(&path);
    app.exec_id("cursor.addNextMatch"); // select "foo" under cursor
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 1);
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_eq!((sel.from(), sel.to()), (0, 3));
    app.exec_id("cursor.addNextMatch"); // add next "foo"
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 2);
    app.exec_id("cursor.addNextMatch"); // third "foo"
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 3);
    std::fs::remove_file(&path).ok();
}

#[test]
fn add_cursor_below_creates_two_carets() {
    let path = temp_file("aaa\nbbb\nccc");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(1); // col 1 line 0
    app.exec_id("cursor.addBelow");
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.selections.len(), 2);
    // Second caret is on line 1 at the same column.
    assert!(doc
        .selections
        .ranges()
        .iter()
        .any(|s| doc.char_to_line(s.head) == 1));
    std::fs::remove_file(&path).ok();
}

#[test]
fn alt_click_adds_cursor() {
    use crossterm::event::KeyModifiers;
    let path = temp_file("hello\nworld");
    let mut app = app_with(&path);
    app.regions.editor = Rect::new(0, 0, 80, 24);
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 6,
        row: 1,
        modifiers: KeyModifiers::ALT,
    });
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 2);
    std::fs::remove_file(&path).ok();
}
