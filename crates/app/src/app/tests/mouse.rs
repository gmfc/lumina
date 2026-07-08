use super::*;

#[test]
fn click_places_cursor() {
    let path = temp_file("hello\nworld");
    let mut app = app_with(&path);
    app.regions.editor = Rect::new(0, 0, 80, 24); // gutter is 4 for a 2-line doc
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 6, 0));
    // col 6 = text col 2 on line 0 -> char offset 2.
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head,
        2
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn double_click_selects_word() {
    let path = temp_file("foo bar baz");
    let mut app = app_with(&path);
    app.regions.editor = Rect::new(0, 0, 80, 24);
    // Two clicks at the same position within the double-click window.
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 8, 0)); // inside "bar"
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 8, 0));
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_eq!((sel.from(), sel.to()), (4, 7)); // "bar"
    std::fs::remove_file(&path).ok();
}

#[test]
fn wheel_scrolls_viewport() {
    let body: String = (0..100).map(|i| format!("l{i}\n")).collect();
    let path = temp_file(&body);
    let mut app = app_with(&path);
    app.regions.editor = Rect::new(0, 0, 80, 24);
    app.on_mouse(mouse(MouseEventKind::ScrollDown, 10, 10));
    assert_eq!(app.editor.active_document().unwrap().view.scroll_line, 3);
    app.on_mouse(mouse(MouseEventKind::ScrollUp, 10, 10));
    assert_eq!(app.editor.active_document().unwrap().view.scroll_line, 0);
    std::fs::remove_file(&path).ok();
}

// ---- rendering -----------------------------------------------------------

#[test]
fn middle_click_closes_a_tab() {
    let p1 = temp_file("one");
    let p2 = temp_file("two");
    let mut app = app_with(&p1);
    app.open_path(&p2);
    assert_eq!(app.editor.workspace.tabs.len(), 2);
    app.regions.tabs = Rect::new(0, 0, 80, 1);
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Middle), 2, 0));
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    std::fs::remove_file(&p1).ok();
    std::fs::remove_file(&p2).ok();
}

#[test]
fn left_drag_extends_selection() {
    let path = temp_file("hello world");
    let mut app = app_with(&path);
    app.regions.editor = Rect::new(0, 0, 80, 24);
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 0)); // set drag anchor
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 12, 0)); // extend
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_ne!(
        sel.from(),
        sel.to(),
        "drag should build a non-empty selection"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn tab_click_then_drag_reorders() {
    let p1 = temp_file("one");
    let p2 = temp_file("two");
    let mut app = app_with(&p1);
    app.open_path(&p2);
    app.regions.tabs = Rect::new(0, 0, 80, 1);
    // Press a tab (arms the drag), then drag along the bar (reorders if over a new tab).
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 0));
    app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 40, 0));
    assert_eq!(app.editor.workspace.tabs.len(), 2); // reorder never drops a tab
    std::fs::remove_file(&p1).ok();
    std::fs::remove_file(&p2).ok();
}

#[test]
fn mouse_up_clears_drag_state() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.regions.editor = Rect::new(0, 0, 80, 24);
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 0));
    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4, 0));
    assert!(app.drag_anchor.is_none());
    assert!(app.tab_drag.is_none());
    std::fs::remove_file(&path).ok();
}
