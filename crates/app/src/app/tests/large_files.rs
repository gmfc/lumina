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
