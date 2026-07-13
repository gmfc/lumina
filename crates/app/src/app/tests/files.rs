use super::*;

#[test]
fn save_as_writes_new_path_and_updates_document() {
    let src = temp_file("x\n");
    let mut app = app_with(&src);
    let target = std::env::temp_dir().join(format!("lumina_saveas_{}.rs", std::process::id()));
    std::fs::remove_file(&target).ok();
    // New untitled buffer with content, then Save As to the target path.
    app.dispatch(Command::NewFile);
    app.dispatch(Command::InsertText("fn main() {}".into()));
    app.save_as_to(&target.to_string_lossy());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {}");
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.path.as_deref(), Some(target.as_path()));
    assert_eq!(doc.language.as_deref(), Some("rust")); // .rs → language picked up
    assert!(!doc.dirty, "clean after save");
    std::fs::remove_file(&src).ok();
    std::fs::remove_file(&target).ok();
}

#[test]
fn new_file_and_save_as_key_bindings_resolve() {
    use crossterm::event::KeyModifiers;
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    let before = app.editor.workspace.tabs.len();
    // ctrl+n → new untitled buffer.
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.workspace.tabs.len(), before + 1);
    assert!(app.editor.active_document().unwrap().path.is_none());
    // ctrl+k ctrl+s (multi-chord) → Save As overlay, and does NOT clobber plain ctrl+s.
    app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(matches!(
        app.editor.overlay,
        Some(crate::editor::Overlay::SaveAsInput { .. })
    ));
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_untitled_falls_back_to_save_as_prompt() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.dispatch(Command::NewFile); // untitled, no path
    app.dispatch(Command::Save); // should open the Save As overlay, not error
    assert!(matches!(
        app.editor.overlay,
        Some(crate::editor::Overlay::SaveAsInput { .. })
    ));
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_active_on_untitled_reports_no_path() {
    // The dirty-close overlay's "save" path calls save_active() directly (it does not route
    // through the Save As prompt), so on an untitled buffer that branch must surface guidance
    // rather than silently doing nothing.
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.dispatch(Command::NewFile); // untitled, no path
    app.dispatch(Command::InsertText("scratch".into()));
    app.save_active();
    assert_eq!(
        app.editor.status_message.as_deref(),
        Some("No path — use Save As")
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_trims_trailing_whitespace_when_enabled() {
    let path = temp_file("foo   \nbar\t\n");
    let mut app = app_with(&path);
    app.config.trim_trailing_whitespace = true;
    app.dispatch(Command::Save);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo\nbar\n");
    assert!(!app.editor.active_document().unwrap().dirty);
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_hygiene_preserves_crlf_on_disk() {
    let path = temp_file("foo  \r\nbar");
    let mut app = app_with(&path);
    app.config.trim_trailing_whitespace = true;
    app.config.insert_final_newline = true;
    app.dispatch(Command::Save);
    // Trimmed + final newline added, but the CRLF line ending is preserved.
    assert_eq!(std::fs::read(&path).unwrap(), b"foo\r\nbar\r\n");
    std::fs::remove_file(&path).ok();
}

#[test]
fn edit_then_save_persists_atomically() {
    let path = temp_file("abc");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertText("XYZ".into()));
    app.dispatch(Command::Save);
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk, "abcXYZ");
    assert!(!app.editor.active_document().unwrap().dirty);
    std::fs::remove_file(&path).ok();
}

#[test]
fn large_file_scrolls_to_follow_cursor() {
    let body: String = (0..5000).map(|i| format!("line {i}\n")).collect();
    let path = temp_file(&body);
    let mut app = app_with(&path);
    app.page_height = 40;
    // Jump to end of document; the viewport must scroll to keep the cursor visible.
    app.dispatch(Command::Move(Motion::DocEnd));
    app.ensure_cursor_visible();
    let doc = app.editor.active_document().unwrap();
    let head_line = doc.char_to_line(doc.selections.primary().head);
    let top = doc.view.scroll_line;
    assert!(
        head_line >= top && head_line < top + 40,
        "cursor off-screen"
    );
    assert!(top > 0, "did not scroll for a large file");
    std::fs::remove_file(&path).ok();
}

#[test]
fn quit_sets_flag() {
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.dispatch(Command::Quit);
    assert!(app.quit);
    std::fs::remove_file(&path).ok();
}

#[test]
fn dirty_close_prompts_then_discards() {
    let path = temp_file("data");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('!'));
    assert!(app.editor.active_document().unwrap().dirty);
    app.dispatch(Command::CloseTab);
    // A dirty tab prompts instead of closing.
    assert!(app.editor.overlay.is_some());
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    // Discard closes it without saving.
    app.overlay_key(KeyEvent::from(KeyCode::Char('d')));
    assert!(app.editor.overlay.is_none());
    assert_eq!(app.editor.workspace.tabs.len(), 0);
    // The file on disk was not modified.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "data");
    std::fs::remove_file(&path).ok();
}

#[test]
fn session_active_index_accounts_for_dropped_untitled_tabs() {
    // Untitled buffers aren't persisted, so the saved `active` must index the filtered file
    // list, not all tabs — an untitled tab before the active one must not shift focus on restore.
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.editor.workspace.tabs.clear();
    // Tabs: [untitled, a.txt] with the path-backed file active (tab 1).
    app.editor
        .workspace
        .open_document(editor_core::Document::from_str("scratch"));
    let file = dir.join("a.txt");
    let doc = crate::files::load(&file).unwrap();
    app.editor.workspace.open_document(doc);
    assert_eq!(app.editor.workspace.active_tab, 1);

    app.save_session();
    let session = crate::session::load(&app.editor.workspace.root).expect("session saved");
    assert_eq!(session.files.len(), 1, "only the path-backed tab is saved");
    assert_eq!(
        session.active, 0,
        "active must index the filtered file list"
    );
    std::fs::remove_dir_all(&dir).ok();
}
