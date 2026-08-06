//! Tests for the guards and affordances the usability review asked for: the quit guard, the
//! Save As overwrite prompt, the external-conflict exits, levelled notices, the persistent status
//! segments, keybindings in the palette, and the in-app reference tabs.

use super::*;
use crate::editor::{NoticeLevel, Overlay, TabView, TextTabKind};

/// Open `path`, then type into it so the buffer is dirty.
fn dirty_app(path: &std::path::Path) -> App {
    let mut app = app_with(path);
    app.dispatch(Command::InsertText("edited".into()));
    assert!(
        app.editor.active_document().unwrap().dirty,
        "precondition: the buffer must be dirty"
    );
    app
}

// --- U5 / U10: quitting past unsaved work ---------------------------------------------------

#[test]
fn quit_with_unsaved_changes_asks_instead_of_discarding() {
    let path = temp_file("hello\n");
    let mut app = dirty_app(&path);

    app.dispatch(Command::Quit);
    assert!(!app.quit, "quit must not go through with unsaved work");
    assert!(
        matches!(app.editor.overlay, Some(Overlay::ConfirmQuit { .. })),
        "it opens the confirmation instead"
    );
    // The box names the file at risk, so the choice can actually be weighed.
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(render_to_string(&mut app, 100, 20).contains(&name));

    // Esc cancels and leaves everything as it was.
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.quit && app.editor.overlay.is_none());
    assert!(app.editor.active_document().unwrap().dirty);

    std::fs::remove_file(&path).ok();
}

#[test]
fn quit_confirmation_can_discard_or_save() {
    let path = temp_file("hello\n");
    let mut app = dirty_app(&path);
    app.dispatch(Command::Quit);
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    assert!(app.quit, "[D] discards and quits");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");

    let mut app = dirty_app(&path);
    app.dispatch(Command::Quit);
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    assert!(app.quit, "[S] saves and quits");
    assert!(
        std::fs::read_to_string(&path).unwrap().contains("edited"),
        "and the work is on disk"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn quit_with_a_clean_buffer_still_quits_immediately() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.dispatch(Command::Quit);
    assert!(app.quit && app.editor.overlay.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn saving_all_before_quit_stops_on_an_unnameable_buffer() {
    // "Save all & quit" on an untitled buffer has nowhere to write: it must not quietly drop the
    // work it just promised to save.
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.dispatch(Command::NewFile);
    app.dispatch(Command::InsertText("scratch".into()));

    app.dispatch(Command::Quit);
    app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    assert!(!app.quit, "the quit is cancelled, not silently completed");
    assert!(
        matches!(app.editor.overlay, Some(Overlay::SaveAsInput { .. })),
        "and Save As opens so the buffer can be named"
    );
    std::fs::remove_file(&path).ok();
}

// --- U11 / U12: Save As --------------------------------------------------------------------

#[test]
fn save_as_asks_before_overwriting_an_existing_file() {
    let victim = temp_file("PRECIOUS\n");
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.dispatch(Command::InsertText("new content".into()));

    app.dispatch(Command::SaveAs);
    type_into_overlay(&mut app, &victim.to_string_lossy());
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        matches!(
            app.editor.overlay,
            Some(Overlay::SaveAsInput {
                overwrite: Some(_),
                ..
            })
        ),
        "Enter on an existing path asks rather than writing"
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "PRECIOUS\n",
        "and nothing has been written yet"
    );
    assert!(render_to_string(&mut app, 100, 20).contains("already exists"));

    // Esc backs out with the file intact.
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "PRECIOUS\n");

    // [O] goes through.
    app.dispatch(Command::SaveAs);
    type_into_overlay(&mut app, &victim.to_string_lossy());
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    assert!(std::fs::read_to_string(&victim)
        .unwrap()
        .contains("new content"));

    std::fs::remove_file(&victim).ok();
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_as_reports_a_missing_directory_in_the_box() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.dispatch(Command::SaveAs);
    type_into_overlay(&mut app, "nope/deeper/file.txt");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match &app.editor.overlay {
        Some(Overlay::SaveAsInput { error: Some(e), .. }) => {
            assert!(e.contains("No such directory"), "{e:?}")
        }
        other => panic!("expected the box to stay open with an error: {other:?}"),
    }
    assert!(
        app.editor
            .active_document()
            .unwrap()
            .path
            .as_deref()
            .is_some_and(|p| p == path),
        "and the buffer must not have been repointed at the bad path"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_as_shows_where_a_relative_path_will_land() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.dispatch(Command::SaveAs);
    type_into_overlay(&mut app, "out.txt");
    let root = app.editor.workspace.root.join("out.txt");
    let full = root.display().to_string();
    // The box shows the tail of a long path, so assert on the part that carries the information:
    // the directory it lands in and the file name.
    let tail: String = full
        .chars()
        .skip(full.chars().count().saturating_sub(24))
        .collect();
    let screen = render_to_string(&mut app, 120, 20);
    assert!(
        screen.contains(&tail),
        "the resolved path should be visible before the save, not after: wanted {tail:?}"
    );
    std::fs::remove_file(&path).ok();
}

/// The full published text of the active app-generated tab, viewport or no viewport.
fn text_view(app: &App) -> String {
    match app.editor.active_tab_view() {
        Some(TabView::Text(t)) => t
            .content
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("expected an app-generated text tab, got {other:?}"),
    }
}

/// Replace the Save As field's contents with `text`.
fn type_into_overlay(app: &mut App, text: &str) {
    for _ in 0..400 {
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in text.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

// --- U3 / U6: external-edit conflicts -------------------------------------------------------

/// Dirty the buffer, then rewrite the file underneath it and let the watcher path run.
fn conflicted_app(path: &std::path::Path) -> App {
    let mut app = dirty_app(path);
    std::fs::write(path, "changed on disk\n").unwrap();
    app.on_disk_changed(path);
    assert!(
        app.editor
            .active_document()
            .unwrap()
            .external_conflict
            .is_some(),
        "precondition: the conflict must be flagged"
    );
    app
}

#[test]
fn an_external_conflict_is_explained_and_shown_persistently() {
    let path = temp_file("hello\n");
    let mut app = conflicted_app(&path);

    let msg = app.editor.status_text().unwrap_or_default().to_string();
    assert!(
        msg.contains("changed on disk") && msg.contains("Revert File"),
        "the conflict names itself and both exits: {msg:?}"
    );
    assert_eq!(app.editor.status_level(), Some(NoticeLevel::Warn));
    assert!(
        render_to_string(&mut app, 120, 20).contains("CONFLICT"),
        "and a persistent state gets a persistent status segment"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn reload_from_disk_resolves_a_conflict_after_confirming() {
    let path = temp_file("hello\n");
    let mut app = conflicted_app(&path);

    app.exec_id("file.reloadFromDisk");
    assert!(
        matches!(app.editor.overlay, Some(Overlay::ConfirmReload)),
        "discarding the buffer is not undoable, so it is confirmed"
    );
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.to_string(), "changed on disk\n");
    assert!(!doc.dirty && doc.external_conflict.is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn keep_mine_clears_a_conflict_and_leaves_the_buffer_alone() {
    let path = temp_file("hello\n");
    let mut app = conflicted_app(&path);
    let before = app.editor.active_document().unwrap().to_string();

    app.exec_id("file.keepMine");
    let doc = app.editor.active_document().unwrap();
    assert!(doc.external_conflict.is_none(), "the conflict is resolved");
    assert!(doc.dirty, "the buffer is still the user's to save");
    assert_eq!(doc.to_string(), before, "and its text is untouched");

    // Saving now writes our version over the disk change, as chosen.
    app.dispatch(Command::Save);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    std::fs::remove_file(&path).ok();
}

#[test]
fn conflict_commands_are_reachable_from_the_palette() {
    let path = temp_file("hello\n");
    let app = app_with(&path);
    for id in ["file.reloadFromDisk", "file.keepMine"] {
        assert!(
            app.editor.command_catalog.iter().any(|c| c.id == id),
            "{id} must be listed in the palette (invariant #4)"
        );
    }
    std::fs::remove_file(&path).ok();
}

// --- U1: levelled, logged notices -----------------------------------------------------------

#[test]
fn confirmations_expire_but_failures_are_held() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);

    app.editor.notify_info("saved something");
    app.dispatch(Command::Move(Motion::Right));
    assert_eq!(app.editor.status_text(), None, "an Info lives one command");

    app.editor.notify_error("Save failed: no permission");
    app.dispatch(Command::Move(Motion::Right));
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        app.editor.status_text(),
        Some("Save failed: no permission"),
        "an Error is not wiped by carrying on typing"
    );

    // Esc is the explicit dismiss.
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.editor.status_text(), None);
    std::fs::remove_file(&path).ok();
}

#[test]
fn every_notice_is_recoverable_from_the_log() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.editor.notice_log.clear();
    app.editor.notify_info("first");
    app.editor.notify_warn("second");
    app.editor.dismiss_status();

    app.exec_id("view.notifications");
    let screen = render_to_string(&mut app, 120, 24);
    assert!(
        screen.contains("first") && screen.contains("second"),
        "a message that scrolled past the status bar is still retrievable"
    );
    assert!(screen.contains("Notifications"), "and the tab names itself");
    std::fs::remove_file(&path).ok();
}

#[test]
fn the_notice_log_is_bounded() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    for i in 0..(crate::editor::NOTICE_LOG_CAP + 50) {
        app.editor.notify_info(format!("message {i}"));
    }
    assert_eq!(app.editor.notice_log.len(), crate::editor::NOTICE_LOG_CAP);
    assert!(app.editor.notice_log.last().unwrap().text.ends_with("149"));
    std::fs::remove_file(&path).ok();
}

// --- U2: large-file mode is visible for as long as it holds ---------------------------------

#[test]
fn large_file_mode_shows_a_persistent_segment() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().large = true;
    // Type past the point where a one-shot message would have gone.
    app.dispatch(Command::Move(Motion::Right));
    assert!(
        render_to_string(&mut app, 120, 20).contains("LARGE"),
        "degraded mode must stay visible, like the LF / UTF-8 segments do"
    );
    std::fs::remove_file(&path).ok();
}

// --- U13 / U14: the palette teaches the keymap ----------------------------------------------

#[test]
fn palette_rows_carry_their_live_keybinding() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    let save = app
        .editor
        .command_catalog
        .iter()
        .find(|c| c.id == "file.save")
        .expect("file.save is in the catalog");
    assert_eq!(save.keys.as_deref(), Some("Ctrl+S"));

    app.exec_id("view.commandPalette");
    let items = &app.editor.picker.as_ref().unwrap().commands;
    let row = items.iter().find(|i| i.id == "file.save").unwrap();
    assert_eq!(row.hint.as_deref(), Some("Ctrl+S"));
    assert!(
        render_to_string(&mut app, 120, 24).contains("Ctrl+S"),
        "and the chord is drawn in the row"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_remapped_chord_shows_through_to_the_palette() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.keymap.bind("ctrl+alt+s", "file.save");
    app.editor.command_catalog = crate::app::command_catalog(&app.registry, &app.keymap);
    let save = app
        .editor
        .command_catalog
        .iter()
        .find(|c| c.id == "file.save")
        .unwrap();
    assert_eq!(
        save.keys.as_deref(),
        Some("Ctrl+S"),
        "the first bound chord wins, and a second binding is additive"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_picker_with_no_matches_says_so() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.exec_id("view.commandPalette");
    for c in "zzzzqqqq".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(app.editor.picker.as_ref().unwrap().filtered.is_empty());
    assert!(
        render_to_string(&mut app, 120, 24).contains("No matching commands"),
        "an empty result must not look like one still loading"
    );
    std::fs::remove_file(&path).ok();
}

// --- U15 / U21: the keymap is learnable from inside the editor ------------------------------

#[test]
fn an_armed_chord_prefix_lists_its_continuations() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    let hint = app.editor.status_text().unwrap_or_default().to_string();
    assert!(hint.starts_with("Ctrl+K …"), "{hint:?}");
    assert!(
        hint.contains("Ctrl+S saveAs"),
        "the prefix should say what may follow it: {hint:?}"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn the_keybinding_reference_is_generated_from_the_live_keymap() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.exec_id("help.keybindings");

    assert!(
        matches!(
            app.editor.active_tab_view(),
            Some(TabView::Text(t)) if t.kind == TextTabKind::Keybindings
        ),
        "it opens as a read-only tab"
    );
    let sheet = text_view(&app);
    assert!(sheet.contains("Ctrl+S"), "with real chords in it");
    assert!(sheet.contains("File: Save"), "labelled by what they do");
    assert!(
        sheet.contains("Differs from VS Code"),
        "and the documented deviations, which the user otherwise finds by surprise"
    );
    assert!(render_to_string(&mut app, 140, 40).contains("Keyboard Shortcuts"));

    // Reopening focuses the one tab rather than stacking duplicates.
    let tabs = app.editor.workspace.tabs.len();
    app.exec_id("help.keybindings");
    assert_eq!(app.editor.workspace.tabs.len(), tabs);
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_remap_shows_through_to_the_keybinding_reference() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.keymap.bind("ctrl+alt+j", "view.toggleSidebar");
    app.exec_id("help.keybindings");
    assert!(text_view(&app).contains("Ctrl+Alt+J"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn a_reference_tab_can_never_be_saved_over_a_file() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.exec_id("help.keybindings");
    // The placeholder buffer has no path at all, so no file can be aimed at.
    let id = app.editor.workspace.active_doc().unwrap();
    assert!(app.editor.is_tab_view(id));
    assert!(app
        .editor
        .workspace
        .documents
        .get(id)
        .unwrap()
        .path
        .is_none());

    app.dispatch(Command::Save);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    std::fs::remove_file(&path).ok();
}

// --- U19 / U20: errors carry their recovery -------------------------------------------------

#[test]
fn a_failed_save_names_the_reason_and_the_way_out() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    // Point the buffer at a path under a directory that does not exist, so the write fails the
    // same way on every platform and for a reason the user can recognise rather than an errno.
    let bad = path
        .parent()
        .unwrap()
        .join("no_such_dir_here")
        .join("child.txt");
    app.editor.active_document_mut().unwrap().path = Some(bad);
    app.dispatch(Command::Save);

    let msg = app.editor.status_text().unwrap_or_default().to_string();
    assert!(msg.starts_with("Save failed:"), "{msg:?}");
    assert!(
        !msg.contains("os error"),
        "the OS's own wording is for a log, not a user: {msg:?}"
    );
    assert!(
        msg.contains("Ctrl+K Ctrl+S"),
        "and a failed save must offer Save As, by live chord: {msg:?}"
    );
    assert_eq!(app.editor.status_level(), Some(NoticeLevel::Error));
    std::fs::remove_file(&path).ok();
}

#[test]
fn an_unknown_command_points_at_the_palette() {
    let path = temp_file("hello\n");
    let mut app = app_with(&path);
    app.exec_id("nope.doesNotExist");
    let msg = app.editor.status_text().unwrap_or_default().to_string();
    assert!(
        msg.contains("no command") && msg.contains("Ctrl+Shift+P"),
        "say what the user can do, not just what the dispatcher couldn't: {msg:?}"
    );
    std::fs::remove_file(&path).ok();
}

// --- U8: overlays dismiss consistently ------------------------------------------------------

#[test]
fn a_hover_box_does_not_swallow_the_key_that_dismisses_it() {
    let path = temp_file("hello world\n");
    let mut app = app_with(&path);
    app.editor.overlay = Some(Overlay::Info("some hover text".into()));
    let before = app
        .editor
        .active_document()
        .unwrap()
        .selections
        .primary()
        .head;

    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(app.editor.overlay.is_none(), "the box closes");
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head,
        before + 1,
        "and the keystroke still does what it was going to do"
    );

    // Esc closes it without doing anything else.
    app.editor.overlay = Some(Overlay::Info("some hover text".into()));
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.editor.overlay.is_none());
    std::fs::remove_file(&path).ok();
}
