use super::*;

#[test]
fn palette_lists_builtin_and_plugin_commands() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.exec_id("view.commandPalette");
    let picker = app.editor.picker.as_ref().unwrap();
    // The palette opens in command mode (`>`), so the active source is the command list.
    assert!(picker.command_mode());
    let labels: Vec<&str> = picker
        .active_items()
        .iter()
        .map(|i| i.label.as_str())
        .collect();
    assert!(labels.contains(&"File: Save"));
    // Plugin-contributed command titles are present too (explorer).
    assert!(labels.iter().any(|l| l.starts_with("Explorer:")));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn goto_line_moves_cursor() {
    // Goto-line is the `palette` plugin's centered prompt now: exec_id opens it, digits + Enter
    // route through the real prompt-key path to the plugin.
    let path = temp_file("l0\nl1\nl2\nl3");
    let mut app = app_with(&path);
    app.exec_id("view.gotoLine");
    assert!(app.editor.prompt.is_some());
    app.on_key(KeyEvent::from(KeyCode::Char('3')));
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.char_to_line(doc.selections.primary().head), 2); // line 3 (0-based 2)
    assert!(
        app.editor.prompt.is_none(),
        "Enter closes the goto-line prompt"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn keymap_ctrl_s_saves() {
    use crossterm::event::KeyModifiers;
    let path = temp_file("abc");
    let mut app = app_with(&path);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('!'));
    assert!(app.editor.active_document().unwrap().dirty);
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert!(!app.editor.active_document().unwrap().dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc!");
    std::fs::remove_file(&path).ok();
}

#[test]
fn keymap_typed_char_inserts() {
    let path = temp_file("");
    let mut app = app_with(&path);
    app.on_key(KeyEvent::from(KeyCode::Char('x')));
    app.on_key(KeyEvent::from(KeyCode::Char('y')));
    assert_eq!(app.editor.active_document().unwrap().to_string(), "xy");
    std::fs::remove_file(&path).ok();
}

#[test]
fn clipboard_copy_paste() {
    // Copy/paste are the `clipboard` builtin plugin now: `edit.copy`/`edit.paste` route through the
    // registry (exec_id). Select-all + caret motion stay app-side editing primitives.
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.dispatch(Command::SelectAll);
    app.exec_id("edit.copy");
    app.dispatch(Command::Move(Motion::DocEnd));
    app.exec_id("edit.paste");
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "hellohello"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn theme_toggles() {
    let path = temp_file("x");
    let mut app = app_with(&path);
    let was_dark = app.theme.is_dark();
    app.exec_id("view.toggleTheme");
    assert_ne!(app.theme.is_dark(), was_dark);
    std::fs::remove_file(&path).ok();
}
