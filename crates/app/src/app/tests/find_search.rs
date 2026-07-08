use super::*;

#[test]
fn find_highlights_and_cycles() {
    let path = temp_file("foo bar foo baz foo");
    let mut app = app_with(&path);
    app.dispatch(Command::FindOpen);
    for c in "foo".chars() {
        app.find_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let find = app.editor.find.as_ref().unwrap();
    assert_eq!(find.matches.len(), 3);
    // Cursor started at 0 -> current match is the first.
    assert_eq!(find.current_match(), Some((0, 3)));
    app.find_key(KeyEvent::from(KeyCode::Enter)); // next
    assert_eq!(
        app.editor.find.as_ref().unwrap().current_match(),
        Some((8, 11))
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn replace_all_is_one_undo() {
    let path = temp_file("cat cat cat");
    let mut app = app_with(&path);
    app.dispatch(Command::ReplaceOpen);
    for c in "cat".chars() {
        app.find_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.find_key(KeyEvent::from(KeyCode::Tab)); // focus replace field
    for c in "dog".chars() {
        app.find_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.replace_all();
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "dog dog dog"
    );
    // One undo reverts the whole replace-all.
    app.editor.find = None;
    app.dispatch(Command::Undo);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "cat cat cat"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn project_search_finds_and_opens() {
    let dir = temp_dir_with_files();
    std::fs::write(dir.join("a.txt"), "find_me on this line\nother").unwrap();
    let mut app = app_with(&dir);
    app.open_search();
    for c in "find_me".chars() {
        app.search_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.search_key(KeyEvent::from(KeyCode::Enter)); // run
                                                    // Drain the worker channel until the search completes (bounded, with backoff).
    for _ in 0..200 {
        app.drain_workers();
        if app.search().map(|s| !s.running).unwrap_or(true) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app
        .search()
        .unwrap()
        .results
        .iter()
        .any(|h| h.text.contains("find_me")));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ctrl_d_selects_word_then_adds_next_match() {
    let path = temp_file("foo bar foo baz foo");
    let mut app = app_with(&path);
    app.dispatch(Command::AddCursorAtNextMatch); // select "foo" under cursor
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 1);
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_eq!((sel.from(), sel.to()), (0, 3));
    app.dispatch(Command::AddCursorAtNextMatch); // add next "foo"
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 2);
    app.dispatch(Command::AddCursorAtNextMatch); // third "foo"
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 3);
    std::fs::remove_file(&path).ok();
}

#[test]
fn add_cursor_below_creates_two_carets() {
    let path = temp_file("aaa\nbbb\nccc");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(1); // col 1 line 0
    app.dispatch(Command::AddCursorBelow);
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
