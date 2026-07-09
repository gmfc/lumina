//! One shell session: a PTY-backed child whose byte stream is parsed by `vt100` into a
//! screen grid. A dedicated reader thread pumps output onto the shared `WorkerMsg` channel,
//! so every mutation still lands on the single-threaded main loop.

use std::io::{Read, Write};
use std::path::Path;
use crate::worker::WorkerTx;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::worker::WorkerMsg;

/// Scrollback history kept per terminal (lines above the live screen).
const SCROLLBACK: usize = 2000;

/// One shell session, shown as a tab in the terminal panel.
pub struct Terminal {
    /// Stable id, used to route reader-thread output back to the right terminal.
    pub id: u64,
    /// Display name in the tab (the shell's basename, e.g. `bash`).
    pub title: String,
    /// Set once the child process has exited (reported by the reader thread).
    pub exited: bool,
    /// The parsed screen; fed bytes from the reader thread on the main loop.
    parser: vt100::Parser,
    /// The PTY master — kept alive to resize and to write input.
    master: Box<dyn MasterPty + Send>,
    /// Input sink to the shell.
    writer: Box<dyn Write + Send>,
    /// Handle to terminate the child on close / drop.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// The child process, kept so we can `wait()` (reap the zombie) on exit or drop. `None`
    /// once already reaped.
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// Current grid size, so we only touch the PTY when it actually changes.
    rows: u16,
    cols: u16,
    /// Rows scrolled back into history (0 = following live output).
    scrollback: usize,
}

impl Terminal {
    /// Spawn `shell` (a program, optionally with whitespace-separated args) attached to a fresh
    /// PTY sized `rows`×`cols`, rooted at `cwd`. Output is streamed to `tx` tagged with `id`.
    /// Returns `None` if the PTY or child could not be created (the caller surfaces that).
    pub fn new(
        id: u64,
        cwd: &Path,
        shell: &str,
        rows: u16,
        cols: u16,
        tx: WorkerTx,
    ) -> Option<Terminal> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .ok()?;

        let mut parts = shell.split_whitespace();
        let program = parts.next()?;
        let title = Path::new(program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string());
        let mut cmd = CommandBuilder::new(program);
        for arg in parts {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");

        let mut child = pair.slave.spawn_command(cmd).ok()?;
        // Critical: drop the slave so the master read returns EOF once the child exits;
        // otherwise the reader thread blocks forever holding an open slave fd.
        drop(pair.slave);

        // From here the child is live: any setup failure must kill *and reap* it, or we leak a
        // shell process. The reader thread no longer owns the child (it only reports EOF), so
        // reaping is done here on failure and by the Terminal itself on exit / drop.
        let killer = child.clone_killer();
        let Some((reader, writer)) = pair
            .master
            .try_clone_reader()
            .ok()
            .zip(pair.master.take_writer().ok())
        else {
            reap(&mut child);
            return None;
        };
        if std::thread::Builder::new()
            .name(format!("term-{id}"))
            .spawn(move || read_loop(id, reader, tx))
            .is_err()
        {
            reap(&mut child);
            return None;
        }

        Some(Terminal {
            id,
            title,
            exited: false,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            master: pair.master,
            writer,
            killer,
            child: Some(child),
            rows,
            cols,
            scrollback: 0,
        })
    }

    /// The current screen grid (what the renderer reads).
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Whether the shell has requested application-cursor-key mode (affects arrow encoding).
    pub fn application_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    /// Whether the view is following live output rather than scrolled into history. The cursor
    /// is only meaningful (and correctly positioned) at the live view.
    pub fn at_live(&self) -> bool {
        self.scrollback == 0
    }

    /// Feed a chunk of shell output into the parser. `vt100` keeps the viewport anchored to the
    /// same content when we're scrolled up (its offset auto-advances as lines arrive); we only
    /// clamp it back to one screenful so `cell()` can't underflow (see `clamp_scrollback`).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.clamp_scrollback();
    }

    /// Write raw bytes to the shell (no-op once it has exited).
    pub fn send_input(&mut self, bytes: &[u8]) {
        if self.exited || bytes.is_empty() {
            return;
        }
        // Typing jumps back to the live view, like a real terminal.
        if self.scrollback != 0 {
            self.scrollback = 0;
            self.parser.set_scrollback(0);
        }
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Scroll the view by `delta` rows (negative = toward older history).
    pub fn scroll(&mut self, delta: isize) {
        // vt100 0.15's viewport can shift by at most one screenful of scrollback; a larger
        // offset underflows its row math in `cell()`. Clamp the request to the grid height.
        let target = ((self.scrollback as isize - delta).max(0) as usize).min(self.rows as usize);
        self.parser.set_scrollback(target);
        self.scrollback = self.parser.screen().scrollback();
    }

    /// Resize the PTY and grid to `rows`×`cols` (no-op when unchanged).
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || (rows == self.rows && cols == self.cols) {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.parser.set_size(rows, cols);
        // A smaller grid may leave the scroll offset above the new height — clamp it.
        self.clamp_scrollback();
    }

    /// Pin the scrollback offset to at most one screenful and record the effective value.
    /// vt100 0.15's `Screen::cell` computes `grid_rows - scrollback_offset`, which panics
    /// (debug) or wraps (release) once the offset exceeds the grid height, so this must run
    /// after anything that can move the offset: `process`, `set_size`, and scrolling.
    fn clamp_scrollback(&mut self) {
        let max = self.rows as usize;
        if self.parser.screen().scrollback() > max {
            self.parser.set_scrollback(max);
        }
        self.scrollback = self.parser.screen().scrollback();
    }

    /// Mark the shell as exited (its reader thread reached EOF) and reap the zombie.
    ///
    /// Runs on the main loop, so it must not block: the PTY reached EOF, meaning the shell has
    /// almost always already exited and a non-blocking `try_wait` reaps it immediately. In the
    /// rare case of a process that closed the terminal but lingers, we leave it for `Drop`
    /// (which kills first) rather than stalling the UI on a blocking `wait`.
    pub fn mark_exited(&mut self) {
        self.exited = true;
        if let Some(child) = &mut self.child {
            if matches!(child.try_wait(), Ok(Some(_))) {
                self.child = None;
            }
        }
    }
}

/// Kill and reap a child process (best-effort), so it doesn't linger as a zombie.
fn reap(child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    let _ = child.clone_killer().kill();
    let _ = child.wait();
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Best-effort: terminate the shell and reap it so we don't leak child processes.
        let _ = self.killer.kill();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

/// Reader thread body: pump PTY output to the main loop until EOF, then report the exit. The
/// child is reaped by the [`Terminal`] (on the `TerminalExited` it triggers, or on drop), so
/// this thread does not own it.
fn read_loop(id: u64, mut reader: Box<dyn Read + Send>, tx: WorkerTx) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx
                    .send(WorkerMsg::TerminalOutput {
                        id,
                        bytes: buf[..n].to_vec(),
                    })
                    .is_err()
                {
                    return; // main loop gone; nothing left to report to.
                }
            }
        }
    }
    let _ = tx.send(WorkerMsg::TerminalExited { id });
}
