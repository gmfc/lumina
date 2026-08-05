use super::*;

#[test]
fn external_plugin_registers_and_edits_through_host() {
    let manifest = "id = \"shout\"\ncapabilities = [\"edit\"]\n\
                    [[commands]]\nid = \"shout.line\"\ntitle = \"Shout\"\n";
    let script = "fn on_command(id, ctx) { \
                  [ #{ action: \"replace_line\", text: ctx.line_text.to_upper() } ] }";
    let (dir, file) = temp_project_with_plugin("shout", manifest, script, "hello world");
    let mut app = app_with(&file);
    // The plugin registered its command through the same registry as built-ins.
    assert!(app.registry.command_ids().any(|c| c == "shout.line"));
    // Running it edits the buffer via a transaction (undoable).
    app.exec_id("shout.line");
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "HELLO WORLD"
    );
    app.dispatch(Command::Undo);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "hello world"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn capability_gating_blocks_ungranted_edit() {
    // Same plugin, but WITHOUT the "edit" capability → the edit action is dropped.
    let manifest = "id = \"shout\"\ncapabilities = []\n\
                    [[commands]]\nid = \"shout.line\"\ntitle = \"Shout\"\n";
    let script = "fn on_command(id, ctx) { \
                  [ #{ action: \"replace_line\", text: ctx.line_text.to_upper() } ] }";
    let (dir, file) = temp_project_with_plugin("shout", manifest, script, "hello world");
    let mut app = app_with(&file);
    app.exec_id("shout.line");
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "hello world",
        "plugin without the edit capability must not modify the buffer"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn external_plugin_draws_a_panel() {
    let manifest = "id = \"insp\"\ncapabilities = [\"ui\"]\n\
                    [[panels]]\nid = \"insp.panel\"\ntitle = \"Inspector\"\nlocation = \"sidebar\"\n";
    let script = "fn render_panel(id, ctx) { [ \"cursor line: \" + ctx.cursor_line ] }";
    let (dir, file) = temp_project_with_plugin("insp", manifest, script, "a\nb\nc");
    let mut app = app_with(&file);
    assert!(app.registry.panel_ids().any(|p| p == "insp.panel"));
    app.registry.render_panel("insp.panel", &mut app.editor);
    let panel = app.editor.panels.get("insp.panel").expect("panel not set");
    assert!(panel.lines[0].spans[0].text.contains("cursor line:"));
    std::fs::remove_dir_all(&dir).ok();
}

/// Build a project dir holding an external plugin plus a file with a chosen name/contents.
fn project_with_viewer(
    id: &str,
    manifest: &str,
    script: &str,
    file_name: &str,
    contents: &str,
) -> (PathBuf, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumina_viewer_{}_{}", std::process::id(), n));
    let pdir = dir.join(".lumina").join("plugins").join(id);
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(pdir.join("plugin.toml"), manifest).unwrap();
    std::fs::write(pdir.join("main.rhai"), script).unwrap();
    let file = dir.join(file_name);
    std::fs::write(&file, contents).unwrap();
    (dir, file)
}

/// The rendered rows of the active viewer tab.
fn viewer_rows(app: &App) -> Vec<String> {
    match app.editor.active_tab_view() {
        Some(crate::editor::TabView::Viewer(v)) => v
            .content
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect())
            .collect(),
        other => panic!("expected a viewer tab, got {other:?}"),
    }
}

#[test]
fn external_plugin_contributes_a_file_viewer() {
    // The external tier reaches the viewer seam through the same contribution a built-in uses.
    let manifest = "id = \"upper\"\ncapabilities = [\"ui\", \"fs:read\"]\n\
                    [[viewers]]\nid = \"upper.view\"\ntitle = \"Upper\"\nextensions = [\"widget\"]\n";
    let script = "fn render_viewer(id, ctx) { [ ctx.text.to_upper(), ctx.path ] }";
    let (dir, file) = project_with_viewer("upper", manifest, script, "a.widget", "hello");
    let app = app_with(&file);

    assert!(app.registry.viewer_ids().any(|v| v == "upper.view"));
    let rows = viewer_rows(&app);
    assert_eq!(rows[0], "HELLO", "the script saw the file's bytes");
    assert!(rows[1].ends_with("a.widget"), "and its path: {}", rows[1]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_viewer_without_the_ui_capability_publishes_nothing() {
    let manifest = "id = \"upper\"\ncapabilities = [\"fs:read\"]\n\
                    [[viewers]]\nid = \"upper.view\"\ntitle = \"Upper\"\nextensions = [\"widget\"]\n";
    let script = "fn render_viewer(id, ctx) { [ \"should not appear\" ] }";
    let (dir, file) = project_with_viewer("upper", manifest, script, "a.widget", "hello");
    let app = app_with(&file);
    assert!(
        viewer_rows(&app).is_empty(),
        "publishing a tab's content requires `ui`, like every other UI action"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_viewer_without_fs_read_never_sees_the_file_contents() {
    // Rhai has no filesystem of its own, so this really is the only channel — deny it and the
    // guest can render only from the path.
    let manifest = "id = \"peek\"\ncapabilities = [\"ui\"]\n\
                    [[viewers]]\nid = \"peek.view\"\ntitle = \"Peek\"\nextensions = [\"widget\"]\n";
    let script =
        "fn render_viewer(id, ctx) { [ if \"text\" in ctx { \"LEAKED\" } else { \"no text\" } ] }";
    let (dir, file) = project_with_viewer("peek", manifest, script, "a.widget", "secret");
    let app = app_with(&file);
    assert_eq!(viewer_rows(&app), vec!["no text".to_string()]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_bundled_csv_viewer_example_aligns_its_columns() {
    // Guards the shipped `plugins/csvview` example against Rhai/API drift — a broken example is
    // worse than no example, since it is what plugin authors copy.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/csvview");
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lumina_csv_{}_{}", std::process::id(), n));
    let pdir = dir.join(".lumina").join("plugins").join("csvview");
    std::fs::create_dir_all(&pdir).unwrap();
    for name in ["plugin.toml", "main.rhai"] {
        std::fs::copy(src.join(name), pdir.join(name)).unwrap();
    }
    let file = dir.join("data.csv");
    std::fs::write(&file, "name,qty\nwidget,3\nx,10\n").unwrap();

    let app = app_with(&file);
    let rows = viewer_rows(&app);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(rows.len(), 3, "one row per record: {rows:?}");
    assert!(rows[0].starts_with("name"), "{rows:?}");
    // Every row's second column starts at the same screen column — that's the whole point.
    let second = |r: &str| r.len() - r.trim_start_matches(|c: char| c != ' ').trim_start().len();
    assert_eq!(
        second(&rows[1]),
        second(&rows[2]),
        "columns aligned: {rows:?}"
    );
}

#[test]
fn modal_keys_route_to_active_modal() {
    let path = temp_file("hello world");
    let mut app = app_with(&path);
    // find (the `find` plugin's prompt is the active modal)
    app.exec_id("search.find");
    assert!(app.editor.prompt.is_some());
    app.on_key(KeyEvent::from(KeyCode::Char('h')));
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(app.editor.prompt.is_none(), "Esc closes the find prompt");
    // picker (the `palette` plugin opens the generic picker)
    app.exec_id("view.commandPalette");
    assert!(app.editor.picker.is_some());
    app.on_key(KeyEvent::from(KeyCode::Esc));
    // search (the `project-search` plugin's query box is a Panel-placement prompt)
    app.exec_id("search.project");
    assert!(app.editor.prompt.is_some());
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(app.editor.prompt.is_none(), "Esc closes the search prompt");
    // overlay (confirm-close prompt on a dirty tab)
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('!'));
    app.dispatch(Command::CloseTab);
    assert!(app.editor.overlay.is_some());
    app.on_key(KeyEvent::from(KeyCode::Esc));
    std::fs::remove_file(&path).ok();
}

#[test]
fn lsp_commands_request_at_cursor() {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_lsp_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn main() {}\n").unwrap();
    let mut app = app_with(&path);
    // The `lsp` plugin owns these ids; force the enabled mirror so it queues requests, then
    // exec_id drains them through the app (a .rs doc resolves lsp_position; no server is
    // configured, so the manager no-ops gracefully rather than spawning one).
    app.editor.lsp_enabled = true;
    app.exec_id("lsp.hover");
    app.exec_id("lsp.gotoDefinition");
    app.exec_id("lsp.completion");
    std::fs::remove_file(&path).ok();
}

#[test]
fn plugin_actions_dispatch_all_kinds() {
    // A Rhai plugin that returns one of every action kind, with the capabilities to
    // exercise each arm of the runtime's action dispatcher.
    let manifest = "id = \"multi\"\ncapabilities = [\"edit\", \"ui\", \"fs:read\"]\n\
                    [[commands]]\nid = \"multi.go\"\ntitle = \"Multi\"\n";
    let script = "fn on_command(id, ctx) { [ \
                  #{ action: \"insert\", text: \"I\" }, \
                  #{ action: \"replace_selection\", text: \"R\" }, \
                  #{ action: \"replace_line\", text: \"L\" }, \
                  #{ action: \"notify\", message: \"hi\" }, \
                  #{ action: \"run\", command: \"view.toggleTheme\" }, \
                  #{ action: \"set_panel\", panel: \"multi.panel\", lines: [\"x\", \"y\"] } \
                  ] }";
    let (dir, file) = temp_project_with_plugin("multi", manifest, script, "hello world");
    let mut app = app_with(&file);
    assert!(app.registry.command_ids().any(|c| c == "multi.go"));
    app.exec_id("multi.go");
    // The set_panel action ran (its panel is now populated); the others ran without error.
    assert!(app.editor.panels.contains_key("multi.panel"));
    std::fs::remove_dir_all(&dir).ok();
}
