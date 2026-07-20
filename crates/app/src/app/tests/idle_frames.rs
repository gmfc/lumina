//! Idle-frame gating (v0.5.1): the run loop repaints + re-runs the caret/LSP recomputes only when
//! something changed. These tests exercise the gate's decision inputs directly — `frame_sig`,
//! `is_animating`, and `drain_workers`' did-work return — since the loop itself blocks on input.

use super::*;

/// Drain until the worker queues settle (the first drain emits `DidChangeActive`, which plugins
/// react to), so a following drain reflects a genuinely idle tick.
fn settle(app: &mut App) {
    for _ in 0..8 {
        if !app.drain_workers() {
            return;
        }
    }
}

#[test]
fn idle_tick_is_skipped_but_an_edit_repaints() {
    let path = temp_file("hello\nworld\n");
    let mut app = app_with(&path);
    // A first render populates the laid-out regions; then let the worker queues settle.
    let _ = render_to_string(&mut app, 40, 8);
    settle(&mut app);

    // Pretend we just painted a frame: snapshot the signature and clear the force flag.
    let sig0 = app.frame_sig();
    app.last_frame_sig = Some(sig0);
    app.force_redraw = false;

    // ---- Idle tick: no input, drain finds nothing, spinner off → the gate skips the frame. ----
    let worked = app.drain_workers();
    let sig1 = app.frame_sig();
    assert!(!worked, "an idle drain reports no work");
    assert_eq!(sig1, sig0, "nothing moved the editor pane");
    assert!(!app.is_animating(), "no spinner active");
    let redraw = app.force_redraw || sig1 != sig0 || app.is_animating();
    assert!(!redraw, "a fully idle frame must be skipped");

    // ---- Typing advances the editor signature (revision + caret) → the gate repaints. ----
    app.on_key(KeyEvent::from(KeyCode::Char('X')));
    let sig2 = app.frame_sig();
    assert_ne!(sig2, sig0, "an edit advances the frame signature");
    let redraw = app.force_redraw || sig2 != app.last_frame_sig.unwrap() || app.is_animating();
    assert!(
        redraw,
        "an edit must repaint (belt-and-suspenders: the signature catches it)"
    );
}

#[test]
fn is_animating_tracks_the_lsp_spinner() {
    let path = temp_file("x");
    let mut app = app_with(&path);
    assert!(!app.is_animating(), "no spinner by default");

    // A starting server animates the footer `{spinner} LSP` indicator.
    app.editor
        .status_items
        .insert("lsp.health".into(), "starting".into());
    assert!(app.is_animating(), "a starting server animates");

    // A ready server is a static dot — no per-frame repaint needed.
    app.editor
        .status_items
        .insert("lsp.health".into(), "ready".into());
    assert!(!app.is_animating(), "a ready server is static");

    // Work-done progress animates its own spinner prefix.
    app.editor
        .status_items
        .insert("lsp.progress".into(), "indexing 40%".into());
    assert!(app.is_animating(), "work-done progress animates");

    // An empty progress string renders nothing, so it must not force redraws.
    app.editor
        .status_items
        .insert("lsp.progress".into(), String::new());
    assert!(!app.is_animating(), "empty progress does not animate");
}

#[test]
fn frame_sig_reflects_edit_caret_and_scroll() {
    let path = temp_file(&"line\n".repeat(50));
    let mut app = app_with(&path);
    let s0 = app.frame_sig();

    // Moving the caret changes the signature.
    app.editor.active_document_mut().unwrap().set_caret(10);
    let s1 = app.frame_sig();
    assert_ne!(s1, s0, "a caret move changes the signature");

    // Scrolling (without moving the caret) also changes it.
    app.editor.active_document_mut().unwrap().view.scroll_line = 5;
    let s2 = app.frame_sig();
    assert_ne!(s2, s1, "a scroll changes the signature");

    // An edit bumps the revision component.
    app.on_key(KeyEvent::from(KeyCode::Char('z')));
    let s3 = app.frame_sig();
    assert_ne!(s3, s2, "an edit changes the signature (revision + caret)");
}

#[test]
fn frame_sig_handles_no_open_document() {
    // With every tab closed there is no active doc; the signature must still be well-defined
    // (the `None` branch) so the idle gate can compare it without panicking.
    let path = temp_file("x");
    let mut app = app_with(&path);
    app.dispatch(Command::CloseTab); // close the only (clean) tab
    assert!(app.editor.active_document().is_none());
    let sig = app.frame_sig();
    assert_eq!(
        sig,
        (None, 0, 0, 0, 0),
        "no-doc signature is the zero tuple"
    );
    // And it stays stable tick-to-tick (so the welcome screen isn't needlessly repainted).
    assert_eq!(app.frame_sig(), sig);
}

#[test]
fn lsp_restart_pending_requires_an_open_doc_of_the_crashed_lang() {
    // A .rs file → the active doc's language is "rust".
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!("lumina_idle_{}_{}.rs", std::process::id(), n));
    std::fs::write(&path, "fn main() {}\n").unwrap();
    let mut app = app_with(&path);

    // Nothing armed → false (the cheap early-out).
    assert!(!app.lsp_restart_pending(), "no crashed server waiting");

    // A crashed "rust" server whose doc is open → pending (drives ensure_started via update_lsp).
    app.lsp.arm_restart_for_test("rust");
    assert!(
        app.lsp_restart_pending(),
        "a rust doc is open, so the rust restart is drivable"
    );

    // A crashed language with NO open doc must be excluded — nothing would ever clear its backoff,
    // so counting it would pin the idle loop forever (the bug this guards).
    let mut app2 = app_with(&path);
    app2.lsp.arm_restart_for_test("python");
    assert!(
        !app2.lsp_restart_pending(),
        "no python doc is open, so its restart must not keep the loop awake"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn drain_reports_work_when_an_intent_is_queued() {
    let path = temp_file("hello");
    let mut app = app_with(&path);
    settle(&mut app);
    assert!(
        !app.drain_workers(),
        "settled app: an idle drain reports no work"
    );

    // A queued command is pending work → the drain that runs it reports true (→ repaint).
    app.editor.pending_commands.push("view.settings".into());
    assert!(
        app.drain_workers(),
        "a queued intent makes the drain report work"
    );
}
