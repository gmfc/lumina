use super::*;

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
