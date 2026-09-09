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

// ---- file operations -----------------------------------------------------

/// Type into the explorer's file-op prompt and confirm.
fn type_and_enter(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));
}

/// The explorer was strictly read-only — no create, rename or delete at all.
#[test]
fn explorer_creates_a_file_and_opens_it() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.exec_id("explorer.newFile");
    assert!(app.editor.prompt.is_some(), "the name prompt is up");

    type_and_enter(&mut app, "created.txt");
    app.drain_workers();

    // A new entry goes into the selected folder — on a fresh tree that is the first directory.
    let created = dir.join("sub").join("created.txt");
    assert!(
        created.exists(),
        "the file was created in the selected folder"
    );
    assert!(app.editor.prompt.is_none(), "and the prompt closed");
    assert!(
        render_to_string(&mut app, 100, 24).contains("created.txt"),
        "the tree rebuilt to show it"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn explorer_creates_a_folder() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.exec_id("explorer.newFolder");
    type_and_enter(&mut app, "newdir");
    assert!(dir.join("sub").join("newdir").is_dir());
    std::fs::remove_dir_all(&dir).ok();
}

/// Creating over an existing file would be silent data loss — the user is adding, not replacing.
#[test]
fn explorer_refuses_to_create_over_an_existing_file() {
    let dir = temp_dir_with_files();
    std::fs::write(dir.join("sub").join("taken.txt"), "important").unwrap();
    let mut app = app_with(&dir);
    app.exec_id("explorer.newFile");
    type_and_enter(&mut app, "taken.txt");

    assert!(
        app.editor
            .prompt
            .as_ref()
            .and_then(|p| p.error.as_ref())
            .is_some(),
        "the prompt stays up carrying the reason, so the name can be fixed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("sub").join("taken.txt")).unwrap(),
        "important",
        "the existing file is untouched"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A name with a path separator would silently create somewhere other than where the user is
/// looking.
#[test]
fn explorer_rejects_a_name_containing_a_separator() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.exec_id("explorer.newFile");
    type_and_enter(&mut app, "sub/evil.txt");
    assert!(app
        .editor
        .prompt
        .as_ref()
        .and_then(|p| p.error.as_ref())
        .is_some());
    assert!(!dir.join("sub").join("evil.txt").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn explorer_renames_the_selected_entry() {
    let dir = temp_dir_with_files();
    let mut app = app_with(&dir);
    app.exec_id("explorer.down"); // select a real row
    let before = app
        .editor
        .panels
        .get("explorer.tree")
        .map(|p| p.selected)
        .unwrap_or(0);
    let _ = before;
    app.exec_id("explorer.rename");
    // The field starts at the current name, so clear it first.
    for _ in 0..40 {
        app.on_key(KeyEvent::from(KeyCode::Backspace));
    }
    type_and_enter(&mut app, "renamed.txt");
    assert!(dir.join("renamed.txt").exists() || dir.join("sub").join("renamed.txt").exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// Deletion has no undo, so it asks for the name back rather than a bare Enter — the same bar
/// the editor sets elsewhere before discarding work.
#[test]
fn explorer_delete_requires_typing_the_name() {
    let dir = temp_dir_with_files();
    std::fs::write(dir.join("doomed.txt"), "bye").unwrap();
    let mut app = app_with(&dir);
    // Walk to doomed.txt.
    for _ in 0..12 {
        let sel = app
            .editor
            .panels
            .get("explorer.tree")
            .and_then(|p| p.lines.get(p.selected))
            .map(|l| l.payload.clone().unwrap_or_default())
            .unwrap_or_default();
        if sel.ends_with("doomed.txt") {
            break;
        }
        app.exec_id("explorer.down");
    }

    app.exec_id("explorer.delete");
    type_and_enter(&mut app, "wrong-name");
    assert!(
        dir.join("doomed.txt").exists(),
        "a mistyped confirmation must not delete anything"
    );
    assert!(app.editor.prompt.is_some(), "and the prompt stays up");

    for _ in 0..20 {
        app.on_key(KeyEvent::from(KeyCode::Backspace));
    }
    type_and_enter(&mut app, "doomed.txt");
    assert!(
        !dir.join("doomed.txt").exists(),
        "the right name deletes it"
    );
    std::fs::remove_dir_all(&dir).ok();
}
