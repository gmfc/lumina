use super::*;

#[test]
fn terminal_panel_layout_and_header_render_without_a_shell() {
    // Force the dock open without spawning a shell, exercising the layout split, the header
    // controls, and the empty-content branch — all PTY-free, so it runs everywhere.
    let path = temp_file("x");
    let mut app = app_with(&path);
    assert!(!app.panel.open);

    app.panel.open = true;
    let text = render_to_string(&mut app, 60, 16);
    assert!(text.contains('▾'), "header shows the minimize control");
    assert!(text.contains('+'), "header shows the new-terminal control");

    // Minimized → only the header row is laid out (no content region recorded).
    app.panel.minimized = true;
    let _ = render_to_string(&mut app, 60, 16);
    assert!(app.regions.panel_header.is_some());
    assert!(app.regions.panel_content.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn minimize_and_close_return_focus_to_editor() {
    // State transitions without a live shell: minimize/restore + close bookkeeping.
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.panel.open = true;
    app.editor.focus = Focus::Panel;

    app.minimize_terminal();
    assert!(app.panel.minimized);
    assert_eq!(app.editor.focus, Focus::Editor);

    app.minimize_terminal();
    assert!(!app.panel.minimized);

    // Closing with no terminals collapses the dock and restores editor focus.
    app.close_terminal();
    assert!(!app.panel.open);
    assert_eq!(app.editor.focus, Focus::Editor);
    std::fs::remove_file(&path).ok();
}

/// Drive the terminal panel end-to-end against a real PTY + `/bin/sh`: render, spawn, type,
/// switch tabs, scroll, and close. Scoped to Linux — the only platform where coverage is
/// collected, so there's no reason to take on macOS PTY / Windows ConPTY variance — and
/// guarded so a runner without a usable PTY skips cleanly rather than failing.
#[cfg(target_os = "linux")]
#[test]
fn terminal_end_to_end_drive() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.config.terminal_shell = Some("/bin/sh".to_string());

    // First frame lays out the (closed) panel; toggling then spawns + focuses the shell.
    let _ = render_to_string(&mut app, 120, 40);
    app.dispatch(Command::ToggleTerminal);
    if app.panel.terminals.is_empty() {
        return; // no usable PTY on this runner — skip rather than fail.
    }
    assert_eq!(app.editor.focus, Focus::Panel);
    // Re-lay-out so the PTY is sized to the panel region.
    let _ = render_to_string(&mut app, 120, 40);
    app.sync_terminals();

    // Type a command via the real key path (Focus::Panel → PTY bytes).
    for ch in "echo lumina_smoke".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        pump_until(&mut app, "lumina_smoke"),
        "the shell echo should render in the terminal panel"
    );

    // A bracketed paste while focused goes to the shell, not the document.
    app.on_paste("echo pasted_ok\r".to_string());
    assert!(
        pump_until(&mut app, "pasted_ok"),
        "paste should reach the shell"
    );

    // Emit far more than one screenful, then wheel up hard over the panel: scrolling past a
    // screenful must not panic vt100's `cell()` (regression for the scrollback-clamp fix).
    for ch in "seq 1 300".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    pump_until(&mut app, "300");
    let content = app.regions.panel_content.expect("panel content region");
    for _ in 0..40 {
        app.on_mouse(mouse(
            MouseEventKind::ScrollUp,
            content.x + 2,
            content.y + 1,
        ));
        let _ = render_to_string(&mut app, 120, 40); // must not panic while scrolled back
    }
    // Typing snaps back to the live view.
    app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(app.panel.active_terminal().unwrap().at_live());

    // A second tab, then cycle and switch by clicking the header.
    app.dispatch(Command::NewTerminal);
    assert_eq!(app.panel.terminals.len(), 2);
    app.dispatch(Command::PrevTerminal);
    assert_eq!(app.panel.active, 0);
    app.dispatch(Command::NextTerminal);
    assert_eq!(app.panel.active, 1);
    let header = app.regions.panel_header.expect("header region");
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        header.x + 6,
        header.y,
    )); // click first tab area → focuses panel
    assert_eq!(app.editor.focus, Focus::Panel);

    // Close tabs until the dock collapses and focus returns to the editor.
    app.dispatch(Command::CloseTerminal);
    assert_eq!(app.panel.terminals.len(), 1);
    app.dispatch(Command::CloseTerminal);
    assert!(!app.panel.open);
    assert_eq!(app.editor.focus, Focus::Editor);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn toggle_terminal_close_branch_without_spawn() {
    // Open + expanded → toggle closes and returns focus to the editor (no shell needed).
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.panel.open = true;
    app.editor.focus = Focus::Panel;
    app.dispatch(Command::ToggleTerminal);
    assert!(!app.panel.open);
    assert_eq!(app.editor.focus, Focus::Editor);
    std::fs::remove_file(&path).ok();
}

#[test]
fn terminal_commands_and_routing_are_inert_without_a_panel() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    // next/prev are guarded no-ops while the dock is closed.
    app.dispatch(Command::NextTerminal);
    app.dispatch(Command::PrevTerminal);
    assert!(!app.panel.open && app.panel.active == 0);

    // A wheel scroll not over the panel routes to the editor.
    let body: String = (0..50).map(|i| format!("l{i}\n")).collect();
    let p2 = temp_file(&body);
    let mut app2 = app_with(&p2);
    app2.regions.editor = Rect::new(0, 0, 80, 24);
    app2.on_mouse(mouse(MouseEventKind::ScrollDown, 10, 10));
    assert_eq!(app2.editor.active_document().unwrap().view.scroll_line, 3);

    // A stray Panel focus with no terminal falls back to the editor.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    app.editor.focus = Focus::Panel;
    assert!(!app.handle_terminal_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
    assert_eq!(app.editor.focus, Focus::Editor);

    // A paste while not panel-focused edits the document.
    app.on_paste("Z".to_string());
    assert!(app
        .editor
        .active_document()
        .unwrap()
        .to_string()
        .contains('Z'));
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&p2).ok();
}
