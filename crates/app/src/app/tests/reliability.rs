//! Industry-readiness §3: crash drafts, lossy-save guard, durable save, and chaos harnesses.

use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use editor_core::{Document, Encoding, LineEnding};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

fn drafts_temp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "lumina_rel_{tag}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn restore_crash_drafts_applies_path_and_untitled() {
    let path = temp_file("disk\n");
    let drafts = drafts_temp("restore");
    let mut app = app_with(&path);
    let ws = app.editor.workspace.root.clone();

    // Seed a path draft that differs from disk, plus an untitled draft.
    crate::drafts::save_draft(
        &drafts,
        &ws,
        &crate::drafts::Draft {
            path: Some(path.clone()),
            untitled_key: None,
            text: "from-draft\n".into(),
            cursor: 2,
            scroll: 0,
            encoding: "utf8".into(),
            line_ending: "lf".into(),
            mixed_line_endings: false,
            lossy_decode: false,
            revision: 1,
        },
    )
    .unwrap();
    crate::drafts::save_draft(
        &drafts,
        &ws,
        &crate::drafts::Draft {
            path: None,
            untitled_key: Some("untitled-0".into()),
            text: "scratch\n".into(),
            cursor: 0,
            scroll: 0,
            encoding: "utf8".into(),
            line_ending: "lf".into(),
            mixed_line_endings: false,
            lossy_decode: true,
            revision: 1,
        },
    )
    .unwrap();

    let before_tabs = app.editor.workspace.tabs.len();
    app.restore_drafts_from(&drafts);
    let doc = app
        .editor
        .workspace
        .documents
        .values()
        .find(|d| d.path.as_ref() == Some(&path));
    let doc = doc.expect("path doc");
    assert_eq!(doc.to_string(), "from-draft\n");
    assert!(doc.dirty);
    assert!(
        app.editor.workspace.tabs.len() > before_tabs,
        "untitled draft should open a new tab"
    );
    assert!(app
        .editor
        .workspace
        .documents
        .values()
        .any(|d| { d.path.is_none() && d.to_string() == "scratch\n" && d.lossy_decode }));
    std::fs::remove_dir_all(&drafts).ok();
    std::fs::remove_file(&path).ok();
}

#[test]
fn crash_draft_flush_and_restore_roundtrip() {
    let path = temp_file("original\n");
    let drafts = drafts_temp("draft");
    let mut app = app_with(&path);
    app.drafts_root_override = Some(drafts.clone());

    // Dirty the buffer and flush a draft immediately (bypass the idle timer).
    app.dispatch(Command::InsertChar('X'));
    assert!(app.editor.active_document().unwrap().dirty);
    app.flush_crash_drafts();

    let loaded = crate::drafts::load_drafts(&drafts, &app.editor.workspace.root);
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].text.contains('X'));

    // Simulate a new launch: reopen the file from disk (no X), then restore drafts.
    let mut app2 = app_with(&path);
    app2.drafts_root_override = Some(drafts.clone());
    assert_eq!(
        app2.editor.active_document().unwrap().to_string(),
        "original\n"
    );
    let root = app2.editor.workspace.root.clone();
    let drafts_list = crate::drafts::load_drafts(&drafts, &root);
    assert_eq!(drafts_list.len(), 1);
    let draft = &drafts_list[0];
    let id = app2.editor.workspace.active_doc().unwrap();
    let doc = app2.editor.workspace.documents.get_mut(id).unwrap();
    doc.reload_from_str(&draft.text);
    doc.dirty = true;
    assert!(doc.to_string().contains('X'));
    assert!(doc.dirty);

    // Saving clears the draft via clear_path_draft.
    app2.clear_path_draft(&path);
    assert!(crate::drafts::load_drafts(&drafts, &root).is_empty());

    std::fs::remove_dir_all(&drafts).ok();
    std::fs::remove_file(&path).ok();
}

#[test]
fn draft_tick_writes_after_idle_window() {
    let path = temp_file("tick\n");
    let drafts = drafts_temp("tick");
    let mut app = app_with(&path);
    app.drafts_root_override = Some(drafts.clone());
    app.dispatch(Command::InsertChar('Z'));
    // Arm the window.
    app.draft_tick();
    assert!(app.draft_mark.is_some());
    // Force the deadline into the past and tick again.
    if let Some((fp, _)) = app.draft_mark {
        app.draft_mark = Some((fp, Instant::now() - Duration::from_secs(1)));
    }
    app.draft_tick();
    let loaded = crate::drafts::load_drafts(&drafts, &app.editor.workspace.root);
    assert_eq!(loaded.len(), 1, "draft_tick should have flushed");
    assert!(loaded[0].text.contains('Z'));
    std::fs::remove_dir_all(&drafts).ok();
    std::fs::remove_file(&path).ok();
}

#[test]
fn untitled_draft_roundtrips() {
    let drafts = drafts_temp("untitled");
    let mut app = App::new(None).unwrap();
    app.lsp.disable_discovery();
    app.editor.lsp_enabled = false;
    app.drafts_root_override = Some(drafts.clone());
    app.new_file();
    app.dispatch(Command::InsertChar('U'));
    app.flush_crash_drafts();
    let loaded = crate::drafts::load_drafts(&drafts, &app.editor.workspace.root);
    assert_eq!(loaded.len(), 1);
    assert!(loaded[0].path.is_none());
    assert_eq!(loaded[0].untitled_key.as_deref(), Some("untitled-0"));
    std::fs::remove_dir_all(&drafts).ok();
}

#[test]
fn lossy_save_opens_confirm_and_save_anyway_clears_flag() {
    let path = temp_file("safe\n");
    let mut app = app_with(&path);
    {
        let doc = app.editor.active_document_mut().unwrap();
        doc.lossy_decode = true;
        doc.dirty = true;
    }
    app.save_active();
    assert!(
        matches!(
            app.editor.overlay,
            Some(crate::editor::Overlay::ConfirmLossySave)
        ),
        "lossy decode must warn before save"
    );
    // Cancel leaves the flag set and the file untouched.
    app.overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.editor.active_document().unwrap().lossy_decode);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "safe\n");

    // Confirm save-anyway.
    app.save_active();
    app.overlay_key(key('s'));
    assert!(!app.editor.active_document().unwrap().lossy_decode);
    assert!(!app.editor.active_document().unwrap().dirty);
    std::fs::remove_file(&path).ok();
}

#[test]
fn cr_and_mixed_line_endings_round_trip_through_encode() {
    let cr = Document::from_str("a\rb\r");
    assert_eq!(cr.line_ending, LineEnding::Cr);
    assert_eq!(crate::files::encode(&cr), b"a\rb\r");

    let mixed = Document::from_str("a\r\nb\nc\r");
    assert!(mixed.mixed_line_endings);
    assert_eq!(mixed.line_ending, LineEnding::Crlf);
    // Dominant style wins on save — mixed does not round-trip byte-for-byte.
    assert_eq!(crate::files::encode(&mixed), b"a\r\nb\r\nc\r\n");
}

#[test]
fn unpaired_utf16_sets_lossy_decode_on_document() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend_from_slice(&0xD800u16.to_le_bytes());
    bytes.extend_from_slice(&(b'x' as u16).to_le_bytes());
    let path = std::env::temp_dir().join(format!(
        "lumina_lossy_{}.txt",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, &bytes).unwrap();
    // Force-open loads UTF-16 even with unpaired surrogates — and must flag lossy_decode.
    let doc = crate::files::open_forced(&path, &crate::files::Limits::from_mb(64, 8)).unwrap();
    assert_eq!(doc.encoding, Encoding::Utf16Le);
    assert!(doc.lossy_decode, "unpaired surrogate must set lossy_decode");
    std::fs::remove_file(&path).ok();
}

// ---- Chaos harnesses (industry-readiness §3 P2) ---------------------------------------------

/// Kill mid-save: rename failure must leave no `.lumina.tmp` orphan (process-kill analogue).
#[test]
fn chaos_kill_mid_save_leaves_no_temp_orphan() {
    let dir = drafts_temp("midsave");
    // Saving *onto* a directory makes rename fail after the temp is created.
    let doc = Document::from_str("chaos\n");
    let err = crate::files::save(&doc, &dir);
    assert!(err.is_err());
    let tmp_name = format!("{}.lumina.tmp", dir.file_name().unwrap().to_string_lossy());
    let orphan = dir.parent().unwrap().join(&tmp_name);
    assert!(
        !orphan.exists(),
        "temp orphan must be cleaned up after failed rename: {}",
        orphan.display()
    );
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        entries
            .iter()
            .all(|e| { !e.file_name().to_string_lossy().ends_with(".lumina.tmp") }),
        "no .lumina.tmp inside the target dir"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Kill mid-LSP: a crash storm during an in-flight didChange must trip the breaker without
/// panicking, and leave the editor able to keep editing.
#[test]
fn chaos_kill_mid_lsp_survives_crash_storm() {
    use crate::lsp::{breaker_tripped, CRASH_LIMIT};
    use std::collections::HashMap;

    let now = Instant::now();
    let times: Vec<Instant> = (0..CRASH_LIMIT).map(|_| now).collect();
    assert!(
        breaker_tripped(&times, now),
        "crash storm must trip the breaker"
    );

    let mut mgr =
        crate::lsp::LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
    let p = std::path::Path::new("/tmp/chaos.rs");
    mgr.did_open(p, "rust", "fn main() {}");
    mgr.did_change(p, "rust", "fn main() { let x = 1; }", None);
    // Simulate CRASH_LIMIT process exits with no live child.
    for _ in 0..CRASH_LIMIT {
        let mut out = Vec::new();
        mgr.handle_exit("rust", &mut out);
    }
    assert!(
        !mgr.ensure_started("rust"),
        "after a crash storm ensure_started must refuse"
    );
    // Editor-side: opening a file and typing must still work with a failed language.
    let path = temp_file("fn main() {}\n");
    let mut app = app_with(&path);
    app.dispatch(Command::InsertChar(' '));
    assert!(app.editor.active_document().unwrap().dirty);
    std::fs::remove_file(&path).ok();
}

/// Watcher storm: a flood of FilesChanged must not panic and must leave the buffer consistent.
#[test]
fn chaos_watcher_storm_is_coalesced_safely() {
    let path = temp_file("watch\n");
    let mut app = app_with(&path);
    for i in 0..200 {
        app.editor.emit(editor_plugin::event::Event::FilesChanged);
        if i % 17 == 0 {
            app.editor.notify_info(format!("external touch {i}"));
        }
    }
    app.drain_workers();
    assert_eq!(app.editor.active_document().unwrap().to_string(), "watch\n");
    app.dispatch(Command::InsertChar('!'));
    for _ in 0..50 {
        app.editor.emit(editor_plugin::event::Event::FilesChanged);
    }
    app.drain_workers();
    assert!(app.editor.active_document().unwrap().dirty);
    std::fs::remove_file(&path).ok();
}

#[test]
fn discard_quit_clears_workspace_drafts() {
    let path = temp_file("bye\n");
    let drafts = drafts_temp("discard");
    let mut app = app_with(&path);
    app.drafts_root_override = Some(drafts.clone());
    app.dispatch(Command::InsertChar('Q'));
    app.flush_crash_drafts();
    assert_eq!(
        crate::drafts::load_drafts(&drafts, &app.editor.workspace.root).len(),
        1
    );
    app.request_quit();
    app.overlay_key(key('d'));
    assert!(app.quit);
    assert!(
        crate::drafts::load_drafts(&drafts, &app.editor.workspace.root).is_empty(),
        "discard & quit must clear drafts"
    );
    std::fs::remove_dir_all(&drafts).ok();
    std::fs::remove_file(&path).ok();
}

#[test]
fn renders_lossy_save_overlay() {
    let path = temp_file("x\n");
    let mut app = app_with(&path);
    app.editor.overlay = Some(crate::editor::Overlay::ConfirmLossySave);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    std::fs::remove_file(&path).ok();
}

#[test]
fn successful_save_clears_draft_and_survives_parent_fsync() {
    let path = temp_file("ok\n");
    let drafts = drafts_temp("oksave");
    let mut app = app_with(&path);
    app.drafts_root_override = Some(drafts.clone());
    app.dispatch(Command::InsertChar('Y'));
    app.flush_crash_drafts();
    assert_eq!(
        crate::drafts::load_drafts(&drafts, &app.editor.workspace.root).len(),
        1
    );
    app.save_active();
    assert!(!app.editor.active_document().unwrap().dirty);
    assert!(
        crate::drafts::load_drafts(&drafts, &app.editor.workspace.root).is_empty(),
        "successful save must clear the matching draft"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Yok\n");
    std::fs::remove_dir_all(&drafts).ok();
    std::fs::remove_file(&path).ok();
}
