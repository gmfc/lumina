//! The right-click context menu: opening it, `when`-filtering, caret placement, and
//! keyboard/mouse navigation.

use super::*;
use crate::editor::Overlay;
use editor_core::Selection;

fn menu_labels(app: &App) -> Vec<String> {
    match &app.editor.overlay {
        Some(Overlay::ContextMenu { items, .. }) => items.iter().map(|i| i.label.clone()).collect(),
        _ => panic!("expected a context menu overlay"),
    }
}

#[test]
fn right_click_opens_menu_with_only_applicable_items() {
    // A `.txt` file (no language server). With a selection, the Edit group's Cut/Copy/Paste show;
    // every LSP item is hidden because no server is running.
    let path = temp_file("hello world");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let _ = render_to_string(&mut app, 60, 12); // lay out regions.editor
    app.editor
        .active_document_mut()
        .unwrap()
        .selections
        .set_single(Selection::new(0, 5)); // select "hello"

    let ed = app.regions.editor;
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        ed.x + 2,
        ed.y,
    ));

    let labels = menu_labels(&app);
    assert!(labels.contains(&"Copy".to_string()));
    assert!(labels.contains(&"Paste".to_string()));
    assert!(
        !labels
            .iter()
            .any(|l| l.contains("Go to Definition") || l.contains("Rename")),
        "LSP items are hidden with no running server: {labels:?}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn menu_without_a_selection_hides_cut_and_copy() {
    // No selection → Cut/Copy (HasSelection) are hidden, but Paste (Always) remains.
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let _ = render_to_string(&mut app, 60, 12);

    let ed = app.regions.editor;
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        ed.x + 1,
        ed.y,
    ));
    let labels = menu_labels(&app);
    assert!(labels.contains(&"Paste".to_string()));
    assert!(
        !labels.contains(&"Copy".to_string()),
        "no selection → no Copy"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn right_click_places_the_caret_at_the_click() {
    let path = temp_file("alpha beta gamma");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let _ = render_to_string(&mut app, 60, 12);
    let ed = app.regions.editor;
    let (cx, cy) = (ed.x + 9, ed.y);
    let expected = app.editor_offset_at(cx, cy);
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), cx, cy));

    if let Some(off) = expected {
        assert_eq!(
            app.editor
                .active_document()
                .unwrap()
                .selections
                .primary()
                .head,
            off,
            "the caret moves to the clicked position"
        );
    }
    assert!(matches!(
        app.editor.overlay,
        Some(Overlay::ContextMenu { .. })
    ));
    std::fs::remove_file(&path).ok();
}

#[test]
fn right_click_inside_a_selection_keeps_it() {
    let path = temp_file("hello world");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let _ = render_to_string(&mut app, 60, 12);
    app.editor
        .active_document_mut()
        .unwrap()
        .selections
        .set_single(Selection::new(0, 5));
    let ed = app.regions.editor;
    // Click within the selected span → the selection is preserved (so Copy still applies).
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        ed.x + 2,
        ed.y,
    ));
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert!(!sel.is_empty(), "the clicked-inside selection is kept");
    std::fs::remove_file(&path).ok();
}

#[test]
fn menu_navigates_with_keys_and_dismisses() {
    let path = temp_file("hi there");
    let mut app = app_with(&path);
    // A selection gives three Edit items (Cut/Copy/Paste), enough to move the selection.
    app.editor
        .active_document_mut()
        .unwrap()
        .selections
        .set_single(Selection::new(0, 2));
    app.open_context_menu(2, 2);
    let selected = |app: &App| match &app.editor.overlay {
        Some(Overlay::ContextMenu { selected, .. }) => *selected,
        _ => usize::MAX,
    };
    assert_eq!(selected(&app), 0);
    app.on_key(KeyEvent::from(KeyCode::Down));
    assert_eq!(selected(&app), 1);
    app.on_key(KeyEvent::from(KeyCode::Up));
    assert_eq!(selected(&app), 0);
    app.on_key(KeyEvent::from(KeyCode::Up)); // wraps to the last item
    assert_eq!(selected(&app), 2);

    // Enter runs the selected command and closes the menu.
    app.on_key(KeyEvent::from(KeyCode::Enter));
    assert!(app.editor.overlay.is_none(), "Enter closes the menu");

    // Esc also dismisses.
    app.open_context_menu(2, 2);
    app.on_key(KeyEvent::from(KeyCode::Esc));
    assert!(app.editor.overlay.is_none(), "Esc dismisses the menu");
    std::fs::remove_file(&path).ok();
}

#[test]
fn menu_item_click_runs_and_outside_click_dismisses() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.editor
        .active_document_mut()
        .unwrap()
        .selections
        .set_single(Selection::new(0, 3));

    // A left click outside the menu dismisses it.
    app.open_context_menu(6, 3);
    let _ = render_to_string(&mut app, 60, 14);
    assert!(!app.regions.context_menu.as_ref().unwrap().is_empty());
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    assert!(app.editor.overlay.is_none(), "a click outside dismisses");

    // A left click on an item rect runs it and closes the menu.
    app.open_context_menu(6, 3);
    let _ = render_to_string(&mut app, 60, 14);
    let item0 = app.regions.context_menu.clone().unwrap()[0];
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        item0.x,
        item0.y,
    ));
    assert!(
        app.editor.overlay.is_none(),
        "clicking an item runs it + closes"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn menu_shows_lsp_items_when_a_server_is_running_and_caret_on_a_word() {
    // A `.rs` file with a running server + the caret on a symbol → the LSP groups appear.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_ctx_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn main() {}\n").unwrap();
    let mut app = app_with(&path);
    app.lsp = crate::lsp::LspManager::new(
        std::path::Path::new("/tmp"),
        std::collections::HashMap::new(),
        "test".into(),
    );
    app.lsp.set_ready_for_test("rust");
    // Caret on 'n' (a word char) so `LspOnWord` holds.
    app.editor.active_document_mut().unwrap().set_caret(1);

    app.open_context_menu(5, 5);
    let labels = menu_labels(&app);
    assert!(
        labels.iter().any(|l| l == "Go to Definition"),
        "navigation shows with a running server: {labels:?}"
    );
    assert!(labels.iter().any(|l| l == "Rename Symbol"));
    assert!(labels.iter().any(|l| l == "Format Document"));
    assert!(labels.iter().any(|l| l == "Code Action / Quick Fix…"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn context_menu_renders_its_item_labels() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.editor
        .active_document_mut()
        .unwrap()
        .selections
        .set_single(Selection::new(0, 3));
    app.open_context_menu(4, 3);
    let out = render_to_string(&mut app, 60, 16);
    assert!(out.contains("Paste"), "the menu draws its item labels");
    assert!(out.contains("Copy"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn tall_menu_clips_items_without_ghost_click_targets() {
    // Many items (LSP running + a selection) in a short terminal → the menu box is clipped. The
    // clipped rows must NOT keep clickable rects (regression: a click below the visible menu ran a
    // hidden command instead of dismissing).
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_ctx_tall_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn main() {}\n").unwrap();
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.lsp = crate::lsp::LspManager::new(
        std::path::Path::new("/tmp"),
        std::collections::HashMap::new(),
        "test".into(),
    );
    app.lsp.set_ready_for_test("rust");
    app.editor
        .active_document_mut()
        .unwrap()
        .selections
        .set_single(Selection::new(0, 2));
    app.open_context_menu(2, 1);
    let _ = render_to_string(&mut app, 60, 8); // short: the menu is taller than the body

    let rects = app.regions.context_menu.clone().unwrap();
    assert!(
        rects.iter().any(|r| r.width == 0),
        "items past the clipped box get no hit rect"
    );
    // Every *clickable* rect stays on-screen (never a ghost below the visible menu).
    assert!(
        rects
            .iter()
            .filter(|r| r.width > 0)
            .all(|r| r.bottom() <= 8),
        "no clickable rect extends past the screen"
    );
    std::fs::remove_file(&path).ok();
}
