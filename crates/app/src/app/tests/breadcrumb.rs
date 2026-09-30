use super::*;

#[test]
fn breadcrumb_strip_renders_when_symbols_cached() {
    let path = temp_file("fn main() {\n    let x = 1;\n}\n");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    app.editor.doc_symbols.insert(
        id,
        vec![
            editor_lsp::DocumentSymbol {
                name: "main".into(),
                kind: 12,
                line: 0,
                character: 0,
                end_line: 2,
                end_character: 1,
                depth: 0,
            },
            editor_lsp::DocumentSymbol {
                name: "x".into(),
                kind: 13,
                line: 1,
                character: 4,
                end_line: 1,
                end_character: 14,
                depth: 1,
            },
        ],
    );
    // Place caret inside the inner symbol.
    app.editor
        .active_document_mut()
        .unwrap()
        .set_caret("fn main() {\n    ".len());
    let text = render_to_string(&mut app, 80, 12);
    assert!(
        text.contains("main"),
        "breadcrumb should show symbol names: {text:?}"
    );
    assert!(
        !app.editor.breadcrumb_hits.is_empty(),
        "render should record clickable breadcrumb hits"
    );
    assert!(
        app.regions.breadcrumb.is_some(),
        "layout should reserve a breadcrumb strip"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn breadcrumb_absent_without_symbols() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    let _ = render_to_string(&mut app, 40, 10);
    assert!(app.editor.breadcrumb_hits.is_empty());
    assert!(app.regions.breadcrumb.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn breadcrumb_click_jumps_to_symbol() {
    let path = temp_file("fn main() {\n    let x = 1;\n}\n");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let id = app.editor.workspace.active_doc().unwrap();
    app.editor.doc_symbols.insert(
        id,
        vec![editor_lsp::DocumentSymbol {
            name: "main".into(),
            kind: 12,
            line: 0,
            character: 0,
            end_line: 2,
            end_character: 1,
            depth: 0,
        }],
    );
    // Caret at end of file so the jump is observable.
    let end = app.editor.active_document().unwrap().len_chars();
    app.editor.active_document_mut().unwrap().set_caret(end);
    let _ = render_to_string(&mut app, 80, 12);
    let (hit, line, ch) = app.editor.breadcrumb_hits[0];
    app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        hit.x + 1,
        hit.y,
    ));
    let head = app
        .editor
        .active_document()
        .unwrap()
        .selections
        .primary()
        .head;
    assert_eq!(
        head,
        app.editor
            .active_document()
            .unwrap()
            .line_to_char(line as usize)
            + ch as usize
    );
    std::fs::remove_file(&path).ok();
}
