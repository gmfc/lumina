//! Coverage for the VS Code-parity commands: line ops, multi-cursor helpers, matching-bracket
//! motion, and the save-all / close-all / reopen-closed tab lifecycle.

use super::*;
use crossterm::event::KeyModifiers;

/// Feed a single key chord through the real keymap path.
fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
}

#[test]
fn delete_line_via_dispatch() {
    let path = temp_file("a\nb\nc");
    let mut app = app_with(&path);
    app.editor
        .active_document_mut()
        .unwrap()
        .set_caret("a\n".len());
    app.dispatch(Command::DeleteLine);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "a\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn delete_line_chord_ctrl_k_ctrl_k() {
    let path = temp_file("one\ntwo\nthree");
    let mut app = app_with(&path);
    // Ctrl+K arms the chord; Ctrl+K completes Delete Line.
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "two\nthree"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn insert_line_below_and_above_from_keys() {
    let path = temp_file("    x");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(5); // end of "    x"
    key(&mut app, KeyCode::Enter, KeyModifiers::CONTROL); // insert below, copies indent
    key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "    x\n    y"
    );
    key(
        &mut app,
        KeyCode::Enter,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ); // insert above
    key(&mut app, KeyCode::Char('z'), KeyModifiers::NONE);
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "    x\n    z\n    y"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn copy_line_up_from_keys() {
    let path = temp_file("a\nb");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(0); // line "a"
    key(
        &mut app,
        KeyCode::Up,
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    );
    assert_eq!(app.editor.active_document().unwrap().to_string(), "a\na\nb");
    std::fs::remove_file(&path).ok();
}

#[test]
fn select_all_matches_then_edit_rewrites_all() {
    let path = temp_file("foo foo bar foo");
    let mut app = app_with(&path);
    // Bare caret inside the first "foo" selects the word, then all occurrences.
    app.editor.active_document_mut().unwrap().set_caret(1);
    app.exec_id("cursor.selectAllMatches");
    assert_eq!(app.editor.active_document().unwrap().selections.len(), 3);
    app.dispatch(Command::InsertText("X".into()));
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "X X bar X"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn registry_contributed_chord_reaches_the_plugin() {
    // Regression guard for the keymap-wiring fix: `ctrl+d` is contributed by the `multicursor`
    // plugin (not the defaults table), so this only resolves if build_keymap folds in
    // registry.keybindings(). Pressing it must reach the plugin via the real chord→id→registry
    // path and select the word under the caret.
    let path = temp_file("foo foo");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(1); // inside the first "foo"
    key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
    let sel = app.editor.active_document().unwrap().selections.primary();
    assert_eq!(
        (sel.from(), sel.to()),
        (0, 3),
        "ctrl+d should resolve through the plugin-contributed keybinding to cursor.addNextMatch"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn cursors_to_line_ends_from_keys() {
    let path = temp_file("aa\nbbb");
    let mut app = app_with(&path);
    app.dispatch(Command::SelectAll);
    key(
        &mut app,
        KeyCode::Char('i'),
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    );
    let doc = app.editor.active_document().unwrap();
    assert_eq!(doc.selections.len(), 2);
    assert!(doc.selections.ranges().iter().all(|s| s.is_empty()));
    // Typing appends at each line end.
    app.dispatch(Command::InsertText("!".into()));
    assert_eq!(
        app.editor.active_document().unwrap().to_string(),
        "aa!\nbbb!"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn jump_to_matching_bracket_motion() {
    let path = temp_file("x(abc)y");
    let mut app = app_with(&path);
    app.editor.active_document_mut().unwrap().set_caret(1); // on '('
    key(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL);
    // Caret jumps to the matching ')' at offset 5.
    assert_eq!(
        app.editor
            .active_document()
            .unwrap()
            .selections
            .primary()
            .head,
        5
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn trim_trailing_whitespace_command() {
    let path = temp_file("a   \nb\t\nc");
    let mut app = app_with(&path);
    app.dispatch(Command::TrimTrailingWhitespace);
    assert_eq!(app.editor.active_document().unwrap().to_string(), "a\nb\nc");
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_all_writes_every_dirty_tab() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b);
    // Dirty both buffers.
    app.editor.workspace.focus_tab(0);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('1'));
    app.editor.workspace.focus_tab(1);
    app.dispatch(Command::Move(Motion::DocEnd));
    app.dispatch(Command::InsertChar('2'));
    let active_before = app.editor.workspace.active_tab;

    app.dispatch(Command::SaveAll);

    assert_eq!(app.editor.workspace.active_tab, active_before);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "aaa1");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "bbb2");
    assert!(app.editor.workspace.documents.values().all(|d| !d.dirty));
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}

#[test]
fn reopen_closed_tab_restores_last_closed() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b); // b is active (tab 1)
    assert_eq!(app.editor.workspace.tabs.len(), 2);

    // Close the active (clean) tab b, then reopen it.
    app.dispatch(Command::CloseTab);
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    app.dispatch(Command::ReopenClosedTab);
    assert_eq!(app.editor.workspace.tabs.len(), 2);
    assert_eq!(
        app.editor.active_document().unwrap().path.as_deref(),
        Some(b.as_path())
    );
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}

#[test]
fn close_all_closes_clean_tabs() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b);
    app.dispatch(Command::CloseAllTabs);
    assert_eq!(app.editor.workspace.tabs.len(), 0);
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}

#[test]
fn close_all_stops_at_dirty_tab_with_prompt() {
    let a = temp_file("aaa");
    let b = temp_file("bbb");
    let mut app = app_with(&a);
    app.open_path(&b);
    // Make the first tab dirty; the last (clean) closes, the dirty one prompts.
    app.editor.workspace.focus_tab(0);
    app.dispatch(Command::InsertChar('!'));
    app.dispatch(Command::CloseAllTabs);
    assert_eq!(app.editor.workspace.tabs.len(), 1);
    assert!(matches!(
        app.editor.overlay,
        Some(crate::editor::Overlay::ConfirmClose { .. })
    ));
    std::fs::remove_file(&a).ok();
    std::fs::remove_file(&b).ok();
}

// ---- keymap reachability -------------------------------------------------

/// Regression guard for the Shift-folding collision.
///
/// `Chord::parse` used to drop Shift for character keys, so `ctrl+shift+p` and `ctrl+p` were
/// literally the same chord — and `Keymap::bind` is last-writer-wins. Four shipped commands were
/// left with no key that could reach them (the command palette, in-file Find, Reopen Closed
/// Editor and Signature Help), while four in-app messages went on telling the user to press
/// `Ctrl+Shift+P`. Nothing caught it because every test drove `exec_id` rather than a chord.
#[test]
fn every_bound_command_keeps_a_reachable_chord() {
    let path = temp_file("x");
    let app = app_with(&path);

    let mut wanted: Vec<String> = crate::commands::default_bindings()
        .iter()
        .map(|(_, id)| (*id).to_string())
        .collect();
    wanted.extend(
        app.registry
            .keybindings()
            .iter()
            .map(|kb| kb.command.clone()),
    );
    wanted.sort();
    wanted.dedup();

    let unreachable: Vec<&str> = wanted
        .iter()
        .filter(|id| app.keymap.binding_label(id).is_none())
        .map(String::as_str)
        .collect();
    assert!(
        unreachable.is_empty(),
        "these commands are bound to a chord that no key can reach: {unreachable:?}\n\
         overwritten bindings: {:?}",
        app.keymap.conflicts()
    );

    std::fs::remove_file(&path).ok();
}

/// The four commands the collision had silently disarmed, pinned to the chords the README
/// advertises for them.
#[test]
fn the_advertised_chords_reach_their_commands() {
    let path = temp_file("x");
    let app = app_with(&path);
    for (id, chord) in [
        ("view.commandPalette", "Ctrl+Shift+P"),
        ("search.find", "Ctrl+F"),
        ("tab.reopenClosed", "Ctrl+Shift+T"),
        ("lsp.signatureHelp", "Ctrl+Shift+Space"),
        ("lsp.documentSymbols", "Ctrl+Shift+O"),
        ("search.project", "Ctrl+Shift+F"),
        ("view.quickOpen", "Ctrl+P"),
        ("lsp.workspaceSymbols", "Ctrl+T"),
    ] {
        assert_eq!(
            app.keymap.binding_label(id).as_deref(),
            Some(chord),
            "{id} lost its documented chord"
        );
    }
    std::fs::remove_file(&path).ok();
}

/// Shipping with an overwritten binding means a command lost its key. Later tiers overriding
/// earlier ones is the point of the stack, but the *defaults plus builtins* must not fight.
#[test]
fn the_shipped_keymap_has_no_overwritten_bindings() {
    let path = temp_file("x");
    let app = app_with(&path);
    assert!(
        app.keymap.conflicts().is_empty(),
        "shipped bindings clobber each other: {:?}",
        app.keymap.conflicts()
    );
    std::fs::remove_file(&path).ok();
}

/// A terminal without the kitty keyboard protocol cannot report Shift on a Ctrl chord — it sends
/// the same bytes for `Ctrl+O` and `Ctrl+Shift+O`. The unshifted chord is free, so the shifted
/// binding still resolves rather than being dead weight on those terminals.
#[test]
fn a_shiftless_terminal_still_reaches_a_shifted_binding() {
    use crate::keymap::{Chord, Keymap, Resolve};
    let km = Keymap::from_pairs([("ctrl+shift+o", "lsp.documentSymbols"), ("ctrl+t", "taken")]);

    let plain_ctrl_o = Chord::from_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert_eq!(
        km.resolve(&[plain_ctrl_o]),
        Resolve::Command("lsp.documentSymbols".into()),
        "Ctrl+O should fall through to the Ctrl+Shift+O binding when Ctrl+O is unbound"
    );

    // But an exact binding always wins: Ctrl+T is claimed, so a physical Ctrl+Shift+T that
    // arrives shiftless must not steal it.
    let plain_ctrl_t = Chord::from_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(
        km.resolve(&[plain_ctrl_t]),
        Resolve::Command("taken".into())
    );
}
