//! The terminal event loop: the `App::run` driver that ticks render → input → drain at ~60Hz.
//!
//! This is imperative I/O glue around a real terminal — it can't be meaningfully unit-tested
//! (it blocks on `event::poll`/`event::read` and drives a live backend), so it is excluded from
//! the coverage ratio (`sonar-project.properties`), the same treatment as `main.rs`. The *logic*
//! it orchestrates — the idle-frame gate's [`App::frame_sig`], [`App::is_animating`],
//! [`App::lsp_restart_pending`], and [`App::drain_workers`] — lives in testable `impl App` blocks
//! and is covered there.

use super::*;

impl App {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Prime the git gutter for any files restored at startup (plan §4.1).
        self.refresh_git_all();
        while !self.quit {
            // Idle-frame gate (v0.5.1): rebuild the frame + re-run the caret/LSP recomputes only when
            // something actually changed since the last frame. `force_redraw` is set by input and by
            // async worker/LSP work; `is_animating` keeps the LSP spinner ticking; the editor-pane
            // `frame_sig` catches direct edits/navigation; and an armed diagnostics-pull debounce is
            // the one time-based LSP path that must keep `update_lsp` running while the buffer is
            // quiet. When none hold, the loop still polls input at ~60Hz but does no render work.
            let sig = self.frame_sig();
            let (redraw, lsp_pending) = self.frame_work(sig);
            if redraw || lsp_pending {
                self.editor.update_highlights(self.page_height);
                self.editor.update_bracket_match();
                self.update_lsp();
            }
            if redraw {
                terminal.draw(|f| ui::draw(f, self))?;
                self.force_redraw = false;
            }
            self.last_frame_sig = Some(sig);
            // Reconcile each PTY's size to the panel region we just laid out.
            self.sync_terminals();

            if self.poll_input()? {
                // Any input event may have changed visible state → repaint next frame.
                self.force_redraw = true;
            }
            // Drain background worker messages (FS/LSP/parse/terminal output). A repaint follows
            // whenever the drain actually processed something.
            if self.drain_workers() {
                self.force_redraw = true;
            }
            self.refresh_viewport();
        }
        // Graceful LSP teardown on quit: shutdown→exit→wait per server, bounded so a hung
        // server can't delay exit beyond the deadline (§3.8).
        self.lsp.stop_all(Duration::from_secs(3));
        self.save_session();
        Ok(())
    }

    /// The idle-frame gate: `(repaint this frame, keep the LSP ticking)`.
    ///
    /// `force_redraw` is set by input and by async worker/LSP work; `is_animating` keeps the LSP
    /// spinner ticking; the editor-pane `sig` catches direct edits/navigation. `lsp_pending`
    /// covers the two wall-clock LSP timers that must keep running while the editor is idle: an
    /// armed diagnostics-pull debounce, and a crashed server (that an open doc uses) waiting out
    /// its restart backoff, whose respawn only fires from `ensure_started` reached via
    /// `update_lsp`. While the LSP panel is open, a pending restart also repaints its server row
    /// through the backoff and the respawn tick (Ok → "starting" / Err → "crashed"); the
    /// subsequent Initializing → Running flip repaints via the `ServerReady` drain event. Both
    /// states terminate, so this can't pin the loop.
    fn frame_work(&self, sig: FrameSig) -> (bool, bool) {
        let restart_pending = self.lsp_restart_pending();
        let redraw = self.force_redraw
            || self.last_frame_sig != Some(sig)
            || self.is_animating()
            || (restart_pending && self.editor.lsp_open);
        let lsp_pending = !self.lsp_pull_deadline.is_empty() || restart_pending;
        (redraw, lsp_pending)
    }

    /// Wait up to one frame for a terminal event and route it. Returns whether one arrived.
    fn poll_input(&mut self) -> Result<bool> {
        if !event::poll(Duration::from_millis(16))? {
            return Ok(false);
        }
        match event::read()? {
            CtEvent::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
            CtEvent::Mouse(m) => self.on_mouse(m),
            CtEvent::Paste(s) => self.on_paste(s),
            // A resize changes the viewport height, so force the caret back into view.
            CtEvent::Resize(..) => self.last_caret = None,
            _ => {}
        }
        Ok(true)
    }
}
