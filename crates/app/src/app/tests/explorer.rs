use super::*;

#[test]
fn explorer_is_registered_and_lists_files() {
    let dir = temp_dir_with_files();
    let app = app_with(&dir);
    // The explorer plugin populated its sidebar panel at activation.
    let panel = app.editor.panels.get("explorer.tree").expect("no panel");
    let names: Vec<String> = panel
        .lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
        .collect();
    assert!(names.iter().any(|t| t.contains("a.txt")));
    assert!(names.iter().any(|t| t.contains("sub")));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn explorer_opens_a_file_on_activate() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    let payload = dir.join("a.txt").to_string_lossy().into_owned();
    app.registry
        .activate_panel_row("explorer.tree", &payload, &mut app.editor);
    app.drain_workers();
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "alpha");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sidebar_click_focuses_sidebar() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.regions.sidebar = Some(Rect::new(0, 0, 20, 24));
    app.regions.editor = Rect::new(20, 0, 60, 24);
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 1));
    assert_eq!(app.editor.focus, Focus::Sidebar);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sidebar_click_hits_the_row_under_the_cursor() {
    use ratatui::{backend::TestBackend, Terminal};
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.editor.focus = Focus::Sidebar;

    // Render a real frame so `Regions` reflect the laid-out sidebar, including the
    // " EXPLORER " title row that the block reserves above the panel content.
    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

    // Locate the screen row where the `sub` directory is actually drawn.
    let sidebar = app.regions.sidebar.expect("sidebar should be visible");
    let buf = terminal.backend().buffer();
    let mut sub_row = None;
    for y in sidebar.y..(sidebar.y + sidebar.height) {
        let mut line = String::new();
        for x in sidebar.x..(sidebar.x + sidebar.width) {
            line.push_str(buf[(x, y)].symbol());
        }
        if line.contains("sub") {
            sub_row = Some(y);
            break;
        }
    }
    let sub_row = sub_row.expect("`sub` directory should be visible in the sidebar");

    // Click exactly where `sub` is drawn. It must toggle that directory open — not
    // open the file rendered on the line below it.
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        sidebar.x + 2,
        sub_row,
    ));
    app.drain_workers();

    assert_eq!(
        app.editor.workspace.tabs.len(),
        0,
        "clicking `sub` must not open the file on the row below it",
    );
    let panel = app.editor.panels.get("explorer.tree").unwrap();
    let names: Vec<String> = panel
        .lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
        .collect();
    assert!(
        names.iter().any(|t| t.contains("b.txt")),
        "clicking `sub` should expand it to reveal b.txt",
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---- key routing ---------------------------------------------------------

#[test]
fn sidebar_keys_drive_explorer_then_escape() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.editor.focus = Focus::Sidebar;
    // Arrow/enter keys map to explorer commands via the sidebar keymap.
    app.on_key(KeyEvent::from(KeyCode::Down)); // explorer.down
    app.on_key(KeyEvent::from(KeyCode::Up)); // explorer.up
    app.on_key(KeyEvent::from(KeyCode::Right)); // explorer.expand
    app.on_key(KeyEvent::from(KeyCode::Left)); // explorer.collapse
    app.on_key(KeyEvent::from(KeyCode::Enter)); // explorer.activate
                                                // revealActiveFile has no arrow binding; drive it directly through the registry.
    app.registry
        .dispatch_command("explorer.revealActiveFile", &mut app.editor);
    // Esc returns focus to the editor.
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.editor.focus, Focus::Editor);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn explorer_commands_navigate_toggle_and_reveal() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    // Open a file so the active document has a path for reveal-active-file to resolve.
    app.open_path(&dir.join("a.txt"));
    // Drive every explorer command through the registry (its run_command dispatcher).
    for id in [
        "explorer.down",
        "explorer.up",
        "explorer.expand",
        "explorer.collapse",
        "explorer.activate",
        "explorer.revealActiveFile",
    ] {
        app.registry.dispatch_command(id, &mut app.editor);
    }
    std::fs::remove_dir_all(&dir).ok();
}

// ---- terminal panel ------------------------------------------------------

// ---- scrolling -----------------------------------------------------------

/// Build a project with more files than fit in the sidebar.
fn temp_dir_with_many_files(n: usize) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "lumina_bigtree_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        std::fs::write(dir.join(format!("file_{i:03}.txt")), "x").unwrap();
    }
    dir
}

/// The panel had no scroll offset at all: rows past the sidebar height were clipped, so arrowing
/// down a long tree moved a selection that nobody could see.
#[test]
fn the_explorer_scrolls_to_keep_the_selection_visible() {
    let dir = temp_dir_with_many_files(60);
    let mut app = app_with(&dir);

    // Walk the selection well past one screenful.
    for _ in 0..40 {
        app.exec_id("explorer.down");
    }
    let text = render_to_string(&mut app, 100, 24);
    assert!(
        text.contains("file_040"),
        "the selected row is on screen after scrolling: {text}"
    );
    assert!(
        !text.contains("file_000"),
        "and the top of the tree has scrolled away"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A click below the fold used to select the wrong entry, because the hit-test mapped the row
/// against panel row 0 rather than the first *drawn* row.
#[test]
fn a_click_below_the_fold_selects_the_row_under_the_cursor() {
    let dir = temp_dir_with_many_files(60);
    let mut app = app_with(&dir);
    for _ in 0..40 {
        app.exec_id("explorer.down");
    }
    render_to_string(&mut app, 100, 24);

    let inner = app.regions.sidebar_inner.expect("sidebar laid out");
    assert!(
        app.regions.sidebar_first_row > 0,
        "the panel really is scrolled"
    );
    // Click the top drawn row and check the file it opened matches what is drawn there.
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 2,
        inner.y,
    ));
    app.drain_workers(); // `Host::open_path` queues; the app opens on the next drain
    let opened = app
        .editor
        .active_document()
        .and_then(|d| d.path.clone())
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .unwrap_or_default();
    let expected = format!("file_{:03}.txt", app.regions.sidebar_first_row);
    assert_eq!(opened, expected, "the click landed on the row drawn there");
    std::fs::remove_dir_all(&dir).ok();
}

/// The wheel over the sidebar scrolled the editor pane next to it, because the sidebar was never
/// in the wheel routing at all.
#[test]
fn the_wheel_scrolls_the_sidebar_not_the_editor_behind_it() {
    let dir = temp_dir_with_many_files(60);
    let mut app = app_with(&dir);
    render_to_string(&mut app, 100, 24);
    let sidebar = app.regions.sidebar.expect("sidebar laid out");

    app.on_mouse(mouse(
        MouseEventKind::ScrollDown,
        sidebar.x + 1,
        sidebar.y + 3,
    ));
    render_to_string(&mut app, 100, 24);
    assert!(
        app.regions.sidebar_first_row > 0,
        "the wheel moved the tree, not the editor"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A file created outside the editor never appeared: the watcher emitted `DidChangeConfig` for a
/// tree change and nothing consumed it. It now emits `FilesChanged`, which the explorer rebuilds on.
#[test]
fn a_file_created_outside_the_editor_appears_in_the_tree() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    assert!(!render_to_string(&mut app, 100, 24).contains("brand_new"));

    app.registry
        .broadcast(&editor_plugin::event::Event::FilesChanged, &mut app.editor);
    std::fs::write(dir.join("brand_new.txt"), "hi").unwrap();
    app.registry
        .broadcast(&editor_plugin::event::Event::FilesChanged, &mut app.editor);

    assert!(
        render_to_string(&mut app, 100, 24).contains("brand_new"),
        "the explorer rebuilt on the tree-change event"
    );
    std::fs::remove_dir_all(&dir).ok();
}
