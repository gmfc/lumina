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
    // A .rs doc resolves lsp_position, so each command reaches its request arm.
    app.dispatch(Command::Hover);
    app.dispatch(Command::GotoDefinition);
    app.dispatch(Command::Completion);
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
