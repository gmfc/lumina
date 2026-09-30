use super::*;
use crossterm::event::KeyModifiers;

#[test]
fn split_right_creates_two_panes_and_close_restores_one() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.exec_id("view.splitRight");
    assert_eq!(app.editor.splits.as_ref().map(|t| t.leaf_count()), Some(2));
    app.exec_id("view.focusNextSplit");
    app.exec_id("view.closeSplit");
    assert_eq!(app.editor.splits.as_ref().map(|t| t.leaf_count()), Some(1));
    // Closing the last split is a no-op.
    app.exec_id("view.closeSplit");
    assert_eq!(app.editor.splits.as_ref().map(|t| t.leaf_count()), Some(1));
    std::fs::remove_file(&path).ok();
}

#[test]
fn split_down_stacks_panes() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.exec_id("view.splitDown");
    assert_eq!(app.editor.splits.as_ref().map(|t| t.leaf_count()), Some(2));
    std::fs::remove_file(&path).ok();
}

#[test]
fn focus_prev_and_next_cycle_panes() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.exec_id("view.splitRight");
    app.exec_id("view.splitDown");
    assert_eq!(app.editor.splits.as_ref().map(|t| t.leaf_count()), Some(3));
    let start = app.editor.split_focus.clone();
    app.exec_id("view.focusNextSplit");
    assert_ne!(app.editor.split_focus, start);
    app.exec_id("view.focusPrevSplit");
    assert_eq!(app.editor.split_focus, start);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ctrl_k_backslash_chord_splits_right() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::NONE));
    assert_eq!(app.editor.splits.as_ref().map(|t| t.leaf_count()), Some(2));
    std::fs::remove_file(&path).ok();
}

#[test]
fn ctrl_k_ctrl_backslash_chord_splits_down() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.splits.as_ref().map(|t| t.leaf_count()), Some(2));
    std::fs::remove_file(&path).ok();
}

#[test]
fn click_focuses_other_split_pane() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.exec_id("view.splitRight");
    let _ = render_to_string(&mut app, 100, 20);
    assert!(
        app.regions.editor_panes.len() >= 2,
        "render should publish pane rects"
    );
    let (path_a, rect_a) = app.regions.editor_panes[0].clone();
    let (path_b, rect_b) = app.regions.editor_panes[1].clone();
    // Focus A, then click into B.
    app.editor.split_focus = path_a;
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        rect_b.x + 1,
        rect_b.y + 1,
    ));
    assert_eq!(app.editor.split_focus, path_b);
    // Click back into A via its rect.
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        rect_a.x + 1,
        rect_a.y + 1,
    ));
    assert_eq!(app.editor.split_focus, app.regions.editor_panes[0].0);
    std::fs::remove_file(&path).ok();
}
