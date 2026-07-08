use super::*;

#[test]
fn config_file_change_triggers_hot_reload() {
    let path = temp_file("");
    let mut app = app_with(&path);
    // Point the watched config path at an arbitrary file and simulate a change event.
    let cfg = std::env::temp_dir().join(format!("lumina_cfg_{}.toml", std::process::id()));
    app.config_path = Some(cfg.clone());
    app.editor.status_message = None;
    app.on_disk_changed(&cfg);
    assert_eq!(
        app.editor.status_message.as_deref(),
        Some("Configuration reloaded"),
        "a change to the config file should hot-reload it"
    );
}

#[test]
fn config_remaps_key() {
    use crossterm::event::KeyModifiers;
    let path = temp_file("");
    let mut app = app_with(&path);
    // Rebind ctrl+s to select-all, then press it.
    app.config
        .keybindings
        .push(("ctrl+s".into(), "edit.selectAll".into()));
    app.keymap = build_keymap(&app.config);
    app.dispatch(Command::InsertText("abc".into()));
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_eq!((sel.from(), sel.to()), (0, 3));
    std::fs::remove_file(&path).ok();
}

#[test]
fn external_clean_reload_follows_cursor() {
    let path = temp_file("a\nb\nTARGET\nd");
    let mut app = app_with(&path);
    // Put the cursor on the TARGET line.
    let off = "a\nb\n".chars().count();
    app.editor.active_document_mut().unwrap().set_caret(off);
    // Another process inserts two lines above.
    std::fs::write(&path, "a\nX\nY\nb\nTARGET\nd").unwrap();
    app.on_disk_changed(&path);
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "a\nX\nY\nb\nTARGET\nd");
    assert!(!doc.dirty);
    assert!(doc.externally_reloaded);
    let line = doc.char_to_line(doc.selections.primary().head);
    assert_eq!(doc.line_text(line).trim_end(), "TARGET");
    std::fs::remove_file(&path).ok();
}

#[test]
fn external_change_on_dirty_flags_conflict() {
    let path = temp_file("original");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('!')); // now dirty
    std::fs::write(&path, "changed by someone else").unwrap();
    app.on_disk_changed(&path);
    let doc = app.editor.active_document().unwrap();
    assert!(doc.external_conflict.is_some());
    assert_eq!(doc.to_string(), "original!"); // NOT clobbered
    std::fs::remove_file(&path).ok();
}

#[test]
fn own_save_echo_is_suppressed() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertText("!".into()));
    app.save_active(); // records pending_self_write
                       // The watcher echoes our own write back:
    app.on_disk_changed(&path);
    let doc = app.editor.active_document().unwrap();
    assert!(
        !doc.externally_reloaded,
        "own save should not trigger a reload"
    );
    assert_eq!(doc.to_string(), "hello!");
    std::fs::remove_file(&path).ok();
}
