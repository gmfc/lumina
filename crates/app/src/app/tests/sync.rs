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
    app.keymap = build_keymap(&app.config, &app.registry);
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
fn external_reload_preserves_utf16_encoding() {
    use editor_core::{Document, Encoding};
    // A UTF-16LE file, externally rewritten (still UTF-16LE). The reload must decode with the
    // encoding-aware path — a bare from_utf8_lossy would turn UTF-16 bytes into mojibake and the
    // stale encoding would re-encode that garbage on the next save.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("lumina_u16_{}_{}.txt", std::process::id(), n));
    let mut d0 = Document::from_str("café");
    d0.encoding = Encoding::Utf16Le;
    std::fs::write(&path, crate::files::encode(&d0)).unwrap();

    let mut app = app_with(&path);
    assert_eq!(
        app.editor.active_document().unwrap().encoding,
        Encoding::Utf16Le
    );

    let mut d1 = Document::from_str("naïve text");
    d1.encoding = Encoding::Utf16Le;
    std::fs::write(&path, crate::files::encode(&d1)).unwrap();
    app.on_disk_changed(&path);

    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "naïve text", "UTF-16 reload decoded wrong");
    assert_eq!(doc.encoding, Encoding::Utf16Le, "encoding not preserved");
    std::fs::remove_file(&path).ok();
}

#[test]
fn external_reload_of_bom_file_does_not_double_bom() {
    use editor_core::{Document, Encoding};
    // A UTF-8 BOM file must not accumulate a second BOM across reload+save cycles: the reload has
    // to strip the BOM (via files::decode) rather than keep a literal U+FEFF in the text.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("lumina_bom_{}_{}.txt", std::process::id(), n));
    let mut d0 = Document::from_str("hi");
    d0.encoding = Encoding::Utf8Bom;
    std::fs::write(&path, crate::files::encode(&d0)).unwrap();

    let mut app = app_with(&path);
    let mut d1 = Document::from_str("bye");
    d1.encoding = Encoding::Utf8Bom;
    std::fs::write(&path, crate::files::encode(&d1)).unwrap();
    app.on_disk_changed(&path);

    let doc = app.editor.active_document().unwrap();
    assert_eq!(
        doc.to_string(),
        "bye",
        "text must not carry a literal BOM char"
    );
    assert!(!doc.to_string().starts_with('\u{feff}'));
    assert_eq!(doc.encoding, Encoding::Utf8Bom);
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
