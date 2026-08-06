//! Large-file and binary-file handling: what kind of tab a path opens, that a non-text tab can
//! never write over the file it displays, and that degraded mode really turns the expensive
//! per-document work off.

use super::*;
use crate::editor::TabView;

/// A temp file with arbitrary bytes and a chosen extension.
fn temp_binary(ext: &str, bytes: &[u8]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut p = std::env::temp_dir();
    p.push(format!("lumina_bin_{}_{}.{ext}", std::process::id(), n));
    std::fs::write(&p, bytes).unwrap();
    p
}

/// A syntactically valid one-page PDF, so the `pdf` viewer has something real to extract.
fn tiny_pdf() -> Vec<u8> {
    let content = b"BT /F1 12 Tf 72 720 Td (Quarterly Report) Tj ET";
    let mut out = Vec::from(&b"%PDF-1.4\n"[..]);
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    out.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n");
    out.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    out.extend_from_slice(content);
    out.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
    out
}

fn active_view(app: &App) -> Option<&TabView> {
    app.editor.active_tab_view()
}

// --- routing ----------------------------------------------------------------------------

#[test]
fn a_binary_file_opens_a_notice_tab_not_a_buffer_of_replacement_characters() {
    // The reported bug, in one test: opening a binary must not produce an editable buffer.
    let path = temp_binary("bin", b"\x00\x01\x02binary\x00content");
    let app = app_with(&path);

    assert!(
        matches!(active_view(&app), Some(TabView::Notice { .. })),
        "a binary file opens a notice tab"
    );
    let id = app.editor.workspace.active_doc().unwrap();
    assert!(app.editor.is_tab_view(id));
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "",
        "the placeholder buffer holds no file content"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn an_oversize_text_file_opens_a_notice_that_open_anyway_replaces() {
    let path = temp_file(&"x".repeat(4096));
    let mut app = app_with(&path);
    // Re-open with a 1-byte ceiling by closing and re-routing through the policy.
    app.config.max_file_size_mb = 1;
    app.close_and_forget(0);
    // 1 MB ceiling, 4 KB file: passes. Shrink the ceiling by making the file bigger instead.
    std::fs::write(&path, vec![b'x'; 2 * 1024 * 1024]).unwrap();
    app.open_path(&path);

    match active_view(&app) {
        Some(TabView::Notice { refusal, .. }) => {
            assert!(refusal.is_overridable(), "a size refusal is overridable")
        }
        other => panic!("expected a notice tab, got {other:?}"),
    }

    app.open_anyway();
    assert!(active_view(&app).is_none(), "now a real text buffer");
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.len_chars(), 2 * 1024 * 1024);
    assert!(
        doc.large,
        "still in degraded mode — forcing open isn't a licence to parse it"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn open_anyway_refuses_a_binary_notice() {
    // Forcing binary bytes through the UTF-8 rope would silently rewrite the file on save.
    let path = temp_binary("bin", b"\x00\x01\x02\x00");
    let mut app = app_with(&path);
    app.open_anyway();
    assert!(
        matches!(active_view(&app), Some(TabView::Notice { .. })),
        "still the notice tab"
    );
    assert!(app
        .editor
        .status_message
        .as_deref()
        .is_some_and(|m| m.contains("not text")));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_pdf_opens_the_plugin_viewer_and_extracts_its_text() {
    let path = temp_binary("pdf", &tiny_pdf());
    let app = app_with(&path);
    match active_view(&app) {
        Some(TabView::Viewer(v)) => {
            assert_eq!(v.viewer_id, "pdf.document");
            let text: String = v
                .content
                .lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.text.as_str())
                .collect();
            assert!(text.contains("Quarterly Report"), "extracted text: {text}");
        }
        other => panic!("expected the pdf viewer, got {other:?}"),
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn disabling_the_pdf_plugin_hands_the_extension_back_to_the_notice() {
    // The claim lives with the plugin, so this is the whole disable story — no special case.
    let path = temp_binary("pdf", &tiny_pdf());
    let mut app = app_with(&path);
    app.close_and_forget(0);
    app.registry = editor_plugin::Registry::with_plugins(
        editor_builtins::all_builtins()
            .into_iter()
            .filter(|p| p.id() != "pdf")
            .collect::<Vec<_>>(),
    );
    app.open_path(&path);
    assert!(
        matches!(active_view(&app), Some(TabView::Notice { .. })),
        "without the pdf plugin, a .pdf is just a binary"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn view_as_hex_replaces_the_notice_tab_in_place() {
    let path = temp_binary("bin", b"Hello\x00\x01");
    let mut app = app_with(&path);
    let tabs_before = app.editor.workspace.tabs.len();

    app.exec_id("view.openAsHex");
    app.drain_workers();

    assert_eq!(
        app.editor.workspace.tabs.len(),
        tabs_before,
        "the notice tab was replaced, not stacked on"
    );
    match active_view(&app) {
        Some(TabView::Viewer(v)) => {
            let text: String = v.content.lines[0]
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect();
            assert!(text.contains("48 65 6c 6c 6f"), "hex of 'Hello': {text}");
        }
        other => panic!("expected the hex viewer, got {other:?}"),
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn reopening_the_same_path_focuses_the_existing_view_tab() {
    let path = temp_binary("bin", b"\x00\x01");
    let mut app = app_with(&path);
    app.open_path(&path);
    assert_eq!(app.editor.workspace.tabs.len(), 1, "no duplicate tab");
    std::fs::remove_file(&path).ok();
}

// --- inertness --------------------------------------------------------------------------

#[test]
fn saving_a_view_tab_leaves_the_file_untouched() {
    // The regression that matters most: the placeholder buffer is empty and carries the file's
    // path, so an unguarded save would truncate the very file being displayed.
    let bytes = b"\x00\x01\x02important binary\x00".to_vec();
    let path = temp_binary("bin", &bytes);
    let mut app = app_with(&path);

    app.save_active();
    assert!(
        app.editor
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("not a text buffer")),
        "and says why: {:?}",
        app.editor.status_message
    );
    app.save_or_save_as();
    app.save_all();
    // `file.saveAs` reaches the Save As overlay directly, so it needs its own guard — without
    // one the placeholder would be repointed at the typed path before the write was refused.
    app.dispatch(Command::SaveAs);
    assert!(
        app.editor.overlay.is_none(),
        "Save As must not even open on a view tab"
    );
    app.save_as_to("/tmp/somewhere-else.txt");

    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "the file's bytes must be byte-for-byte unchanged"
    );
    assert_eq!(
        app.editor.active_tab_view().map(|v| v.path().to_path_buf()),
        Some(crate::files::absolute_path(&path)),
        "and the tab still points at the file it is showing"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn typing_into_a_view_tab_does_nothing() {
    let path = temp_binary("bin", b"\x00\x01");
    let mut app = app_with(&path);
    for c in "hello".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "", "the placeholder stayed empty");
    assert!(!doc.dirty, "and never went dirty");
    std::fs::remove_file(&path).ok();
}

#[test]
fn an_external_change_re_renders_a_view_tab_instead_of_reloading_text_into_it() {
    let path = temp_binary("bin", b"Hello\x00");
    let mut app = app_with(&path);
    app.exec_id("view.openAsHex");
    app.drain_workers();

    std::fs::write(&path, b"Bye!!\x00").unwrap();
    app.on_disk_changed(&path);

    match active_view(&app) {
        Some(TabView::Viewer(v)) => {
            let text: String = v.content.lines[0]
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect();
            assert!(
                text.contains("42 79 65"),
                "re-rendered from new bytes: {text}"
            );
        }
        other => panic!("expected the hex viewer, got {other:?}"),
    }
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "",
        "the placeholder buffer never took the file's bytes"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn closing_a_view_tab_drops_its_state() {
    let path = temp_binary("bin", b"\x00\x01");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    app.close_and_forget(0);
    assert!(!app.editor.is_tab_view(id), "the side table was pruned too");
    std::fs::remove_file(&path).ok();
}

// --- degraded mode ----------------------------------------------------------------------

#[test]
fn a_large_file_opens_without_syntax_highlighting() {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_big_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn main() {}\n".repeat(100)).unwrap();
    let mut app = app_with(&path);

    // Baseline: a small .rs file does get a highlighter.
    app.editor.update_highlights(20);
    let id = app.editor.workspace.active_doc().unwrap();
    assert!(
        app.editor.highlighters.contains_key(&id),
        "a normal .rs file is highlighted"
    );

    // Flag it large the way `files::open` would, and no highlighter is created.
    app.close_and_forget(0);
    app.config.large_file_mb = 1;
    std::fs::write(&path, "fn main() {}\n".repeat(100_000)).unwrap();
    app.open_path(&path);
    app.editor.update_highlights(20);
    let id = app.editor.workspace.active_doc().unwrap();
    assert!(app.editor.active_document().unwrap().large);
    assert!(
        !app.editor.highlighters.contains_key(&id),
        "no tree-sitter parse for a large file"
    );
    assert!(
        app.editor
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("large-file mode")),
        "the mode is announced, so missing colour isn't mistaken for a bug"
    );
    std::fs::remove_file(&path).ok();
}

// --- rendering --------------------------------------------------------------------------

/// Render a frame and flatten it to one string per row.
fn rendered_rows(app: &mut App, w: u16, h: u16) -> Vec<String> {
    use ratatui::{backend::TestBackend, Terminal};
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn the_notice_tab_renders_the_label_size_and_actions() {
    let path = temp_binary("bin", b"\x00\x01\x02\x03");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let screen = rendered_rows(&mut app, 70, 18).join("\n");

    assert!(screen.contains("Binary file"), "the label: {screen}");
    assert!(screen.contains("4 B"), "the size: {screen}");
    assert!(
        screen.contains("View as hex"),
        "the hex action, from the live keymap: {screen}"
    );
    assert!(
        !screen.contains("Open as text anyway"),
        "a binary offers no override: {screen}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn the_viewer_tab_renders_its_published_lines_and_scrolls() {
    let path = temp_binary("bin", &(0u8..=255).collect::<Vec<u8>>());
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.exec_id("view.openAsHex");
    app.drain_workers();

    let top = rendered_rows(&mut app, 80, 10).join("\n");
    assert!(top.contains("Hex View"), "the header: {top}");
    assert!(top.contains("00000000"), "the first row: {top}");

    // Scroll down; the first row must leave the screen.
    app.scroll_tab_view(4);
    let scrolled = rendered_rows(&mut app, 80, 10).join("\n");
    assert!(
        !scrolled.contains("00000000"),
        "scrolled past row 0: {scrolled}"
    );
    assert!(scrolled.contains("00000040"), "{scrolled}");

    // Scrolling up past the top clamps rather than wrapping or panicking.
    app.scroll_tab_view(-1000);
    assert!(rendered_rows(&mut app, 80, 10)
        .join("\n")
        .contains("00000000"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_view_tab_is_named_after_its_file_in_the_tab_bar() {
    let path = temp_binary("bin", b"\x00");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(rendered_rows(&mut app, 80, 6)[0].contains(&name));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_paste_cannot_edit_a_view_tabs_placeholder() {
    // Bracketed paste arrives as `CtEvent::Paste`, which never enters `on_key` — so the key-layer
    // guard does not see it. `with_doc` is the chokepoint that must.
    let path = temp_binary("bin", b"\x00\x01");
    let mut app = app_with(&path);
    app.on_paste("pasted text".into());
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "", "the placeholder stayed empty");
    assert!(!doc.dirty);
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_plugin_cannot_edit_a_view_tabs_placeholder() {
    // `edit.paste` is one keystroke away from `Host::apply_transaction` on the active doc.
    use editor_plugin::Host;
    let path = temp_binary("bin", b"\x00\x01");
    let mut app = app_with(&path);
    let id = app.editor.workspace.active_doc().unwrap();
    let txn = {
        let doc = app.editor.workspace.documents.get(id).unwrap();
        editor_core::Transaction::insert(doc, 0, "injected")
    };
    app.editor.apply_transaction(id, txn);
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "", "the Host port refused the edit");
    assert!(!doc.dirty);
    std::fs::remove_file(&path).ok();
}

#[test]
fn right_click_opens_no_editing_menu_on_a_view_tab() {
    let path = temp_binary("bin", b"\x00\x01");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    let _ = rendered_rows(&mut app, 60, 10); // lay out `regions.editor`
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 5, 3));
    assert!(
        app.editor.overlay.is_none(),
        "Cut/Copy/Paste would act on the placeholder buffer"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_viewer_can_be_escaped_with_open_as_text() {
    // A plugin claiming a *text* extension must not take those files hostage.
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lumina_hostage_{}_{}", std::process::id(), n));
    let pdir = dir.join(".lumina").join("plugins").join("claimer");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("plugin.toml"),
        "id = \"claimer\"\ncapabilities = [\"ui\"]\n\
         [[viewers]]\nid = \"claimer.v\"\ntitle = \"Claimed\"\nextensions = [\"txt\"]\n",
    )
    .unwrap();
    std::fs::write(
        pdir.join("main.rhai"),
        "fn render_viewer(id, ctx) { [\"claimed\"] }",
    )
    .unwrap();
    let file = dir.join("notes.txt");
    std::fs::write(&file, "real contents\n").unwrap();

    let mut app = app_with(&file);
    assert!(
        matches!(active_view(&app), Some(TabView::Viewer(_))),
        "the plugin claimed .txt"
    );
    app.open_as_text();
    assert!(active_view(&app).is_none(), "escaped to a text buffer");
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "real contents\n"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn opening_a_viewer_for_an_already_open_file_replaces_that_tab() {
    // Two documents claiming one path desynchronize: `find_by_path` answers with the first, so
    // the watcher reloads one and leaves the other stale.
    let path = temp_file("plain text\n");
    let mut app = app_with(&path);
    assert_eq!(app.editor.workspace.tabs.len(), 1);

    app.exec_id("view.openAsHex");
    app.drain_workers();
    assert_eq!(
        app.editor.workspace.tabs.len(),
        1,
        "the text tab was replaced"
    );
    assert!(matches!(active_view(&app), Some(TabView::Viewer(_))));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_dirty_text_tab_is_never_closed_to_open_a_viewer() {
    let path = temp_file("plain text\n");
    let mut app = app_with(&path);
    app.dispatch(Command::InsertChar('x'));
    assert!(app.editor.active_document().unwrap().dirty);

    app.exec_id("view.openAsHex");
    app.drain_workers();
    assert!(
        active_view(&app).is_none(),
        "unsaved work outranks a view request"
    );
    assert!(app
        .editor
        .status_message
        .as_deref()
        .is_some_and(|m| m.contains("unsaved changes")));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_file_that_grows_past_the_limit_while_open_is_not_reloaded() {
    // The watcher path is a back door into the same unbounded read `files::open` prevents.
    let path = temp_file("small\n");
    let mut app = app_with(&path);
    app.config.max_file_size_mb = 1;

    std::fs::write(&path, vec![b'a'; 2 * 1024 * 1024]).unwrap();
    app.on_disk_changed(&path);

    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "small\n",
        "the buffer was kept rather than reloaded"
    );
    assert!(app
        .editor
        .status_message
        .as_deref()
        .is_some_and(|m| m.contains("grew past")));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_file_that_becomes_binary_while_open_is_not_reloaded() {
    let path = temp_file("text\n");
    let mut app = app_with(&path);
    std::fs::write(&path, b"%PDF-1.7\nnow a pdf").unwrap();
    app.on_disk_changed(&path);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "text\n");
    assert!(app
        .editor
        .status_message
        .as_deref()
        .is_some_and(|m| m.contains("no longer text")));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_file_that_grows_across_the_degraded_threshold_picks_it_up_on_reload() {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("lumina_grow_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn a() {}\n").unwrap();
    let mut app = app_with(&path);
    app.editor.update_highlights(20);
    let id = app.editor.workspace.active_doc().unwrap();
    assert!(app.editor.highlighters.contains_key(&id));

    app.config.large_file_mb = 1;
    std::fs::write(&path, "fn a() {}\n".repeat(200_000)).unwrap();
    app.on_disk_changed(&path);

    assert!(
        app.editor.active_document().unwrap().large,
        "flag recomputed"
    );
    assert!(
        !app.editor.highlighters.contains_key(&id),
        "and the stale highlighter dropped"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_chord_still_resolves_while_a_view_tab_is_focused() {
    // `tab_view_key` swallows plain characters so the placeholder can't be typed into — but a
    // chord's continuation key is a plain character too, which made every `ctrl+k <key>` chord
    // dead on exactly the tabs where the hex-view chord lives.
    let path = temp_binary("bin", b"\x00\x01");
    let mut app = app_with(&path);
    app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    assert!(
        app.editor
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("Saved")),
        "ctrl+k s (Save All) must still resolve: {:?}",
        app.editor.status_message
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_viewer_renders_wide_characters_without_dropping_them() {
    // A cell-per-char writer and a width-aware caller disagree on every CJK character, which
    // silently ate the character after each wide one. PDFs and CSVs contain them.
    use editor_plugin::{Host, PanelLine, Span, ViewerContent};
    let path = temp_binary("bin", b"\x00");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.exec_id("view.openAsHex");
    app.drain_workers();
    let id = app.editor.workspace.active_doc().unwrap();
    app.editor.set_viewer_content(
        id,
        ViewerContent {
            status: None,
            lines: vec![PanelLine::new(vec![
                Span::plain("世界"),
                Span::plain("END"),
            ])],
        },
    );
    let screen = rendered_rows(&mut app, 40, 8).join("\n");
    assert!(screen.contains('世') && screen.contains('界'), "{screen}");
    assert!(
        screen.contains("END"),
        "the span after a wide one survived: {screen}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn growing_the_pane_re_clamps_a_viewer_scrolled_to_the_end() {
    // Nothing re-clamps `scroll` when the viewport grows (resize, closing the dock), so the
    // renderer must — otherwise the tab goes mostly blank until the next keystroke.
    let path = temp_binary("bin", &(0u8..=255).collect::<Vec<u8>>());
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.exec_id("view.openAsHex");
    app.drain_workers();

    let _ = rendered_rows(&mut app, 80, 8); // a short pane …
    app.scroll_tab_view(isize::MAX / 2); // … scrolled to its end
                                         // Now render tall enough to show every row: the last row must be on screen, not blank.
    let screen = rendered_rows(&mut app, 80, 24).join("\n");
    assert!(screen.contains("000000f0"), "final row visible: {screen}");
    assert!(
        screen.contains("00000000"),
        "and the first, since all 16 rows fit: {screen}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_viewer_survives_a_pane_too_short_for_a_body() {
    let path = temp_binary("bin", b"Hello\x00");
    let mut app = app_with(&path);
    app.editor.sidebar_visible = false;
    app.exec_id("view.openAsHex");
    app.drain_workers();
    // 1- and 2-row editor panes: no body fits, and paging must not walk `scroll` through rows
    // that can never be drawn.
    for h in [3u16, 4] {
        let _ = rendered_rows(&mut app, 40, h);
        app.scroll_tab_view(100);
        match app.editor.active_tab_view() {
            Some(TabView::Viewer(v)) => assert_eq!(v.scroll, 0, "nothing to scroll at height {h}"),
            other => panic!("expected a viewer, got {other:?}"),
        }
    }
    std::fs::remove_file(&path).ok();
}
