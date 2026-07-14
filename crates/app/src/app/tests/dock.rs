//! The shared bottom dock: LSP-tab toggle, tab arbitration with the terminal, panel rendering, and
//! the footer-indicator click.

use super::*;
use crate::editor::{DockTab, Focus};

#[test]
fn lsp_panel_toggles_open_and_closed_with_focus() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    assert_eq!(app.dock_active_tab(), None, "dock starts hidden");

    app.toggle_lsp_panel();
    assert_eq!(app.dock_active_tab(), Some(DockTab::Lsp));
    assert_eq!(app.editor.focus, Focus::LspPanel);
    // The dock renders with the Terminal/LSP tab strip.
    let out = render_to_string(&mut app, 80, 24);
    assert!(
        out.contains("LSP") && out.contains("Terminal"),
        "the dock tab strip renders both tabs"
    );

    app.toggle_lsp_panel();
    assert_eq!(
        app.dock_active_tab(),
        None,
        "toggling again closes the dock"
    );
    assert_eq!(app.editor.focus, Focus::Editor);

    // The command id routes through `exec_id` to the same toggle.
    app.exec_id("lsp.panel.toggle");
    assert_eq!(app.dock_active_tab(), Some(DockTab::Lsp));
    std::fs::remove_file(&path).ok();
}

#[test]
fn dock_active_tab_clamps_to_an_open_tab() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    // `dock_active` points at Terminal but nothing is open → the dock is hidden.
    app.editor.dock_active = DockTab::Terminal;
    assert_eq!(app.dock_active_tab(), None);
    // The LSP tab is open while `dock_active` still says Terminal → display clamps to the open tab.
    app.editor.lsp_open = true;
    assert_eq!(app.dock_active_tab(), Some(DockTab::Lsp));
    std::fs::remove_file(&path).ok();
}

#[test]
fn empty_lsp_panel_shows_a_hint() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    app.toggle_lsp_panel();
    let out = render_to_string(&mut app, 90, 24);
    assert!(
        out.contains("No language servers active"),
        "the empty-state hint renders"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn lsp_panel_renders_language_rows_from_the_manager() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    // Inject a manager with an explicit `[lsp]` override → `status_rows` yields a `rust` row.
    let servers = std::collections::HashMap::from([(
        "rust".to_string(),
        vec!["my-rust-analyzer".to_string()],
    )]);
    app.lsp = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), servers, "test".into());
    app.toggle_lsp_panel();
    let out = render_to_string(&mut app, 90, 24);
    assert!(out.contains("rust"), "the language row renders");
    assert!(
        out.contains("my-rust-analyzer"),
        "the resolved command renders on the row"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn terminal_and_lsp_coexist_in_the_dock() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    // Toggling the terminal opens the dock on the Terminal tab (the plugin drives the lifecycle,
    // and the `set_terminal_view` hook makes it the active tab).
    app.exec_id("terminal.toggle");
    assert!(app.editor.terminal_view.open, "the terminal opened");
    assert_eq!(app.dock_active_tab(), Some(DockTab::Terminal));

    // Opening the LSP panel switches the visible tab but leaves the terminal open underneath.
    app.toggle_lsp_panel();
    assert_eq!(app.dock_active_tab(), Some(DockTab::Lsp));
    assert!(
        app.editor.terminal_view.open,
        "the terminal stays open behind the LSP tab"
    );

    // Switching back to the Terminal tab shows it (and refocuses the shell).
    app.focus_dock_tab(DockTab::Terminal);
    assert_eq!(app.dock_active_tab(), Some(DockTab::Terminal));
    assert_eq!(app.editor.focus, Focus::Panel);
    std::fs::remove_file(&path).ok();
}

#[test]
fn footer_lsp_indicator_click_opens_the_panel() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    // Publish a health tag so the footer LSP segment renders and records its click rect.
    app.editor
        .status_items
        .insert("lsp.health".into(), "ready".into());
    let _ = render_to_string(&mut app, 90, 24);
    let rect = app
        .regions
        .lsp_status
        .expect("the footer LSP segment records a click rect");

    // Clicking it opens the LSP panel.
    app.mouse_left_down(rect.x, rect.y, KeyModifiers::NONE);
    assert_eq!(app.dock_active_tab(), Some(DockTab::Lsp));
    std::fs::remove_file(&path).ok();
}

fn three_server_manager() -> crate::lsp::LspManager {
    let servers = std::collections::HashMap::from([
        ("rust".to_string(), vec!["ra".to_string()]),
        ("go".to_string(), vec!["gopls".to_string()]),
        ("python".to_string(), vec!["pyright".to_string()]),
    ]);
    crate::lsp::LspManager::new(std::path::Path::new("/tmp"), servers, "test".into())
}

#[test]
fn lsp_panel_keys_scroll_and_escape() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    app.lsp = three_server_manager(); // three rows, so scrolling advances + clamps
    app.toggle_lsp_panel();
    assert_eq!(app.editor.focus, Focus::LspPanel);

    app.on_key(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.editor.lsp_panel.scroll, 1);
    app.on_key(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(app.editor.lsp_panel.scroll, 2);
    app.on_key(KeyEvent::from(KeyCode::PageDown)); // clamps at rows-1 = 2
    assert_eq!(app.editor.lsp_panel.scroll, 2);
    app.on_key(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(app.editor.lsp_panel.scroll, 1);
    app.on_key(KeyEvent::from(KeyCode::Up));
    assert_eq!(app.editor.lsp_panel.scroll, 0);
    app.on_key(KeyEvent::from(KeyCode::PageUp)); // clamps at 0
    assert_eq!(app.editor.lsp_panel.scroll, 0);
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.editor.focus, Focus::Editor);
    std::fs::remove_file(&path).ok();
}

#[test]
fn dock_minimize_toggles_the_active_tab() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    // No tab open → minimize is a no-op (the None arm).
    app.dock_minimize_active();
    assert_eq!(app.dock_active_tab(), None);

    // LSP tab: minimize collapses it + drops focus; again restores + refocuses.
    app.toggle_lsp_panel();
    app.dock_minimize_active();
    assert!(app.editor.lsp_panel.minimized);
    assert_eq!(app.editor.focus, Focus::Editor);
    app.dock_minimize_active();
    assert!(!app.editor.lsp_panel.minimized);
    assert_eq!(app.editor.focus, Focus::LspPanel);

    // Terminal tab: minimize routes to the plugin.
    app.exec_id("terminal.toggle");
    assert_eq!(app.dock_active_tab(), Some(DockTab::Terminal));
    app.dock_minimize_active();
    assert!(app.editor.terminal_view.minimized);
    std::fs::remove_file(&path).ok();
}

#[test]
fn lsp_panel_mouse_click_focuses_and_wheel_scrolls() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.lsp = three_server_manager();
    app.toggle_lsp_panel();
    let _ = render_to_string(&mut app, 90, 24);
    let content = app
        .regions
        .lsp_content
        .expect("the LSP content region is drawn");

    // A click in the content focuses the LSP panel.
    app.editor.focus = Focus::Editor;
    app.mouse_left_down(content.x, content.y, KeyModifiers::NONE);
    assert_eq!(app.editor.focus, Focus::LspPanel);

    // A wheel scroll over the content scrolls the list (ScrollDown steps by 3, clamped to rows-1=2).
    app.on_mouse(mouse(MouseEventKind::ScrollDown, content.x, content.y));
    assert_eq!(app.editor.lsp_panel.scroll, 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn dock_header_click_switches_tabs_and_minimizes() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    // Open the terminal so the dock shows with both tab buttons.
    app.exec_id("terminal.toggle");
    let _ = render_to_string(&mut app, 90, 24);
    let header = app.regions.panel_header.expect("the dock header is drawn");

    // Segments (x=0): " ▾ "(0..3) " Terminal "(3..13) " LSP "(13..18) …
    // Click the LSP dock tab → switch to it.
    app.mouse_left_down(header.x + 14, header.y, KeyModifiers::NONE);
    assert_eq!(app.dock_active_tab(), Some(DockTab::Lsp));
    // Click the minimize chevron → minimize the now-active (LSP) tab.
    app.mouse_left_down(header.x + 1, header.y, KeyModifiers::NONE);
    assert!(app.editor.lsp_panel.minimized);
    std::fs::remove_file(&path).ok();
}

#[test]
fn focus_dock_tab_terminal_opens_one_when_none_exists() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    assert!(!app.editor.terminal_view.open);
    app.focus_dock_tab(DockTab::Terminal); // no terminal → the plugin spawns one
    assert!(
        app.editor.terminal_view.open,
        "switching to the Terminal tab opens a shell"
    );
    assert_eq!(app.dock_active_tab(), Some(DockTab::Terminal));
    std::fs::remove_file(&path).ok();
}

#[test]
fn missing_server_auto_opens_the_lsp_panel_once() {
    // A `.zig` file whose zig server (zls) is probed as not installed.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_dock_{}_{}.zig", std::process::id(), n));
    std::fs::write(&path, "const x = 1;\n").unwrap();
    let mut app = app_with(&path);
    app.lsp = crate::lsp::LspManager::new(
        std::path::Path::new("/tmp"),
        std::collections::HashMap::new(),
        "test".into(),
    );
    app.lsp.set_resolved_for_test("zig", None); // known server, not installed

    assert_eq!(app.dock_active_tab(), None);
    app.maybe_auto_open_lsp();
    assert_eq!(
        app.dock_active_tab(),
        Some(DockTab::Lsp),
        "auto-opens the panel on a missing server"
    );
    assert_eq!(
        app.editor.focus,
        Focus::Editor,
        "but keeps editor focus so it never disrupts typing"
    );

    // Close it; the once-per-language guard must not re-open it.
    app.editor.lsp_open = false;
    app.editor.dock_active = DockTab::Terminal;
    app.maybe_auto_open_lsp();
    assert_eq!(app.dock_active_tab(), None, "does not nag a second time");
    std::fs::remove_file(&path).ok();
}

#[test]
fn lsp_panel_renders_the_server_log_tail() {
    let path = temp_file("hi");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let servers = std::collections::HashMap::from([("rust".to_string(), vec!["ra".to_string()])]);
    app.lsp = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), servers, "test".into());
    app.lsp
        .push_log_for_test("rust", "indexing 342 of 1200 crates");
    app.toggle_lsp_panel();
    let out = render_to_string(&mut app, 100, 24);
    assert!(out.contains("server log"), "the log separator renders");
    assert!(out.contains("indexing 342"), "the log line renders");
    std::fs::remove_file(&path).ok();
}
