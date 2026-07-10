//! Coverage for the interactive Settings tab: opening, widget interaction (toggle,
//! number stepper, select/dropdown, text field, plugin enable/disable), live-apply,
//! config persistence, rendering, and mouse clicks.

use super::*;
use crate::settings::Entry;

/// An app with a Settings tab open and its config writes redirected to a temp file.
fn settings_app() -> (App, PathBuf, PathBuf) {
    let file = temp_file("hello");
    let mut app = app_with(&file);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut cfg = std::env::temp_dir();
    cfg.push(format!("lumina_settings_{}_{}.toml", std::process::id(), n));
    app.config_path = Some(cfg.clone());
    app.open_settings();
    (app, file, cfg)
}

/// Focus the settings row with the given config key.
fn focus(app: &mut App, key: &str) {
    let view = app.settings.as_mut().expect("settings open");
    let idx = view
        .entries
        .iter()
        .position(|e| matches!(e, Entry::Item(it) if it.key == key))
        .unwrap_or_else(|| panic!("no setting {key}"));
    view.selected = idx;
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn open_creates_and_focuses_settings_tab() {
    let (app, file, cfg) = settings_app();
    assert!(app.settings.is_some());
    assert!(app.settings_active());
    // At least the built-in explorer plugin appears in the Plugins section.
    let plugins = app
        .settings
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::Item(it) if it.key.starts_with("plugin:")))
        .count();
    assert!(plugins >= 1);
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn open_twice_focuses_the_same_tab() {
    let (mut app, file, cfg) = settings_app();
    let tabs = app.editor.workspace.tabs.len();
    app.open_settings();
    assert_eq!(app.editor.workspace.tabs.len(), tabs); // no duplicate tab
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn toggle_bool_applies_and_persists() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "auto_pairs");
    let before = app.config.auto_pairs;
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.config.auto_pairs, !before);
    let written = std::fs::read_to_string(&cfg).unwrap();
    assert!(written.contains("auto_pairs = false"));
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn vim_toggle_enables_modal_layer_live() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "vim");
    press(&mut app, KeyCode::Char(' '));
    assert!(app.config.vim);
    assert!(app.editor.vim.is_some()); // applied live, not just persisted
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn number_stepper_increments_and_clamps() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "sidebar_width");
    let before = app.config.sidebar_width;
    press(&mut app, KeyCode::Right);
    assert_eq!(app.config.sidebar_width, before + 1);
    // Live-applied to the editor state too.
    assert_eq!(app.editor.sidebar_width, before + 1);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.config.sidebar_width, before);
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn select_cycles_tab_width() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "tab_width");
    assert_eq!(app.config.tab_width, 4); // default; options [2,4,8], index 1
    press(&mut app, KeyCode::Right); // -> 8
    assert_eq!(app.config.tab_width, 8);
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn dropdown_open_and_choose() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "tab_width");
    press(&mut app, KeyCode::Enter); // open dropdown
    assert!(app.settings.as_ref().unwrap().dropdown.is_some());
    press(&mut app, KeyCode::Up); // highlight the option above (2)
    press(&mut app, KeyCode::Enter); // choose it
    assert!(app.settings.as_ref().unwrap().dropdown.is_none());
    assert_eq!(app.config.tab_width, 2);
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn text_field_edit_commits() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "terminal_shell");
    press(&mut app, KeyCode::Enter); // start editing
    assert!(app.settings.as_ref().unwrap().editing.is_some());
    for c in "/bin/zsh".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter); // commit
    assert_eq!(app.config.terminal_shell.as_deref(), Some("/bin/zsh"));
    let written = std::fs::read_to_string(&cfg).unwrap();
    assert!(written.contains("/bin/zsh"));
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn text_field_edit_escape_cancels() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "terminal_shell");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Esc);
    assert!(app.settings.as_ref().unwrap().editing.is_none());
    assert_eq!(app.config.terminal_shell, None);
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn plugin_disable_persists() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "plugin:explorer");
    press(&mut app, KeyCode::Char(' ')); // disable
    assert!(app.config.disabled_plugins.iter().any(|d| d == "explorer"));
    let written = std::fs::read_to_string(&cfg).unwrap();
    assert!(written.contains("[plugins]"));
    assert!(written.contains("explorer"));
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn navigation_skips_headers() {
    let (mut app, file, cfg) = settings_app();
    // Selection always lands on an Item, never a Header.
    for _ in 0..40 {
        press(&mut app, KeyCode::Down);
        let view = app.settings.as_ref().unwrap();
        assert!(matches!(
            view.entries.get(view.selected),
            Some(Entry::Item(_))
        ));
    }
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn global_chord_falls_through_in_settings() {
    let (mut app, file, cfg) = settings_app();
    // Ctrl+W closes the settings tab even while it's focused.
    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    app.reconcile_settings();
    assert!(app.settings.is_none());
    assert!(!app.settings_active());
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn renders_form_and_widgets() {
    let (mut app, file, cfg) = settings_app();
    let out = render_to_string(&mut app, 100, 32);
    assert!(out.contains("Settings"));
    assert!(out.contains("Vim mode"));
    assert!(out.contains("Tab width"));
    assert!(out.contains("EDITOR")); // a section header, upper-cased
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn mouse_click_toggles_row() {
    let (mut app, file, cfg) = settings_app();
    // Lay out a frame so `regions.editor` is populated.
    let _ = render_to_string(&mut app, 100, 32);
    let area = app.regions.editor;
    // Find the screen row of the `auto_pairs` toggle.
    let view = app.settings.as_ref().unwrap();
    let idx = view
        .entries
        .iter()
        .position(|e| matches!(e, Entry::Item(it) if it.key == "auto_pairs"))
        .unwrap();
    // Recompute its row via the same layout the renderer uses.
    let row = (area.y + 2..area.y + area.height)
        .find(|&r| crate::ui::settings_entry_at(view, area, r) == Some(idx))
        .expect("auto_pairs row visible");
    let before = app.config.auto_pairs;
    app.on_mouse(mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        area.x + 5,
        row,
    ));
    assert_eq!(app.config.auto_pairs, !before);
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn renders_open_dropdown() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "tab_width");
    press(&mut app, KeyCode::Enter); // open the dropdown
    assert!(app.settings.as_ref().unwrap().dropdown.is_some());
    let out = render_to_string(&mut app, 100, 32);
    // The option values are drawn in the floating list.
    assert!(out.contains('2') && out.contains('8'));
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn renders_while_editing_text_field() {
    let (mut app, file, cfg) = settings_app();
    focus(&mut app, "terminal_shell");
    press(&mut app, KeyCode::Enter);
    for c in "/bin/ba".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let out = render_to_string(&mut app, 100, 32);
    assert!(out.contains("/bin/ba")); // the live edit buffer is shown
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}

#[test]
fn render_tolerates_tiny_pane() {
    let (mut app, file, cfg) = settings_app();
    // A pane too small to draw the form must bail out cleanly, not panic.
    let _ = render_to_string(&mut app, 6, 4);
    std::fs::remove_file(&file).ok();
    std::fs::remove_file(&cfg).ok();
}
