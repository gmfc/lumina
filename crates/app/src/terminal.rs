//! The integrated terminal panel: a minimizable, tabbed dock below the editor that hosts
//! real shell sessions. Each tab drives a pseudo-terminal (via `portable-pty`); the shell's
//! byte stream is parsed by `vt100` into a screen grid the renderer reads — so the panel stays
//! a pure function of state, exactly like the editor (invariant #8).
//!
//! Threading mirrors the rest of the app (search / git / fs-watch, `worker.rs`): each terminal
//! owns a reader thread that pushes output through the shared `WorkerMsg` channel, so every
//! mutation still lands on the single-threaded main loop. The panel is deliberately small and
//! composable — split panes, a task runner, or other bottom-dock contributions can grow off the
//! same `TerminalPanel` later.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use ratatui::style::Color;

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
        tx: Sender<WorkerMsg>,
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

        let child = pair.slave.spawn_command(cmd).ok()?;
        // Critical: drop the slave so the master read returns EOF once the child exits;
        // otherwise the reader thread blocks forever holding an open slave fd.
        drop(pair.slave);

        // From here the child is live: any setup failure must kill it, or we leak a shell
        // process (its reader thread — the only other thing that would reap it — never starts).
        let mut killer = child.clone_killer();
        let Some((reader, writer)) = pair
            .master
            .try_clone_reader()
            .ok()
            .zip(pair.master.take_writer().ok())
        else {
            let _ = killer.kill();
            return None;
        };
        if std::thread::Builder::new()
            .name(format!("term-{id}"))
            .spawn(move || read_loop(id, reader, child, tx))
            .is_err()
        {
            let _ = killer.kill();
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

    /// Mark the shell as exited (its reader thread has ended).
    pub fn mark_exited(&mut self) {
        self.exited = true;
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Best-effort: terminate the shell so we don't leak child processes.
        let _ = self.killer.kill();
    }
}

/// Reader thread body: pump PTY output to the main loop until EOF, then report the exit.
fn read_loop(
    id: u64,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    tx: Sender<WorkerMsg>,
) {
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
    let _ = child.wait();
    let _ = tx.send(WorkerMsg::TerminalExited { id });
}

/// A clickable region of the panel header, returned by hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderHit {
    /// The minimize / restore control.
    Minimize,
    /// The tab for terminal at this index.
    Tab(usize),
    /// The "new terminal" (`+`) control.
    New,
}

/// The bottom terminal dock: a set of terminal tabs plus its open / minimized / size state.
pub struct TerminalPanel {
    pub terminals: Vec<Terminal>,
    pub active: usize,
    /// Whether the panel occupies space in the layout at all.
    pub open: bool,
    /// When open, whether it is collapsed to just its header row.
    pub minimized: bool,
    /// Desired content height (rows) when expanded.
    pub height: u16,
    next_id: u64,
}

impl TerminalPanel {
    pub fn new(height: u16) -> TerminalPanel {
        TerminalPanel {
            terminals: Vec::new(),
            active: 0,
            open: false,
            minimized: false,
            height: height.clamp(3, 60),
            next_id: 1,
        }
    }

    pub fn active_terminal(&self) -> Option<&Terminal> {
        self.terminals.get(self.active)
    }

    pub fn active_terminal_mut(&mut self) -> Option<&mut Terminal> {
        self.terminals.get_mut(self.active)
    }

    /// The terminal with `id`, if still present (routes reader-thread messages).
    pub fn terminal_mut(&mut self, id: u64) -> Option<&mut Terminal> {
        self.terminals.iter_mut().find(|t| t.id == id)
    }

    /// Spawn a new terminal tab and make it active. Returns `false` if spawning failed.
    pub fn open_new(
        &mut self,
        cwd: &Path,
        shell: &str,
        rows: u16,
        cols: u16,
        tx: Sender<WorkerMsg>,
    ) -> bool {
        let id = self.next_id;
        match Terminal::new(id, cwd, shell, rows, cols, tx) {
            Some(term) => {
                self.next_id += 1;
                self.terminals.push(term);
                self.active = self.terminals.len() - 1;
                true
            }
            None => false,
        }
    }

    /// Close the active tab (its `Drop` kills the shell). Returns `true` if the panel is now
    /// empty (the caller closes the dock and returns focus to the editor).
    pub fn close_active(&mut self) -> bool {
        if self.terminals.is_empty() {
            return true;
        }
        let removed = self.active;
        self.terminals.remove(removed);
        self.active = index_after_close(self.terminals.len() + 1, self.active, removed);
        self.terminals.is_empty()
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.terminals.len() {
            self.active = idx;
        }
    }

    pub fn next(&mut self) {
        self.active = next_index(self.terminals.len(), self.active);
    }

    pub fn prev(&mut self) {
        self.active = prev_index(self.terminals.len(), self.active);
    }

    pub fn toggle_minimized(&mut self) {
        self.minimized = !self.minimized;
    }

    /// The header laid out left-to-right as `(label, hit)` segments. Labels use only width-1
    /// glyphs, so callers may treat one char as one display column for layout / hit-testing.
    pub fn header_segments(&self) -> Vec<(String, HeaderHit)> {
        let mut segs = Vec::with_capacity(self.terminals.len() + 2);
        let ctrl = if self.minimized { " ▸ " } else { " ▾ " };
        segs.push((ctrl.to_string(), HeaderHit::Minimize));
        for (i, t) in self.terminals.iter().enumerate() {
            let mark = if t.exited { '·' } else { '×' };
            segs.push((
                format!(" {}: {} {mark} ", i + 1, t.title),
                HeaderHit::Tab(i),
            ));
        }
        segs.push((" + ".to_string(), HeaderHit::New));
        segs
    }
}

/// Next tab index (wraps); `active` when the list is empty.
fn next_index(len: usize, active: usize) -> usize {
    if len == 0 {
        0
    } else {
        (active + 1) % len
    }
}

/// Previous tab index (wraps); `active` when the list is empty.
fn prev_index(len: usize, active: usize) -> usize {
    if len == 0 {
        0
    } else {
        (active + len - 1) % len
    }
}

/// New active index after removing `removed` from a list that had `old_len` items.
fn index_after_close(old_len: usize, active: usize, removed: usize) -> usize {
    let new_len = old_len.saturating_sub(1);
    if new_len == 0 {
        0
    } else if removed < active {
        active - 1
    } else {
        active.min(new_len - 1)
    }
}

/// The default shell for a new terminal: the config override, else the platform's usual shell.
pub fn default_shell(config_override: Option<&str>) -> String {
    if let Some(s) = config_override {
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    #[cfg(windows)]
    {
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

/// Map a `vt100` cell color to a ratatui color. `None` means "terminal default" — the renderer
/// leaves that channel unset (`Reset`) so the surrounding theme shows through.
pub fn vt_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Translate a key event into the byte sequence a terminal expects, or `None` when the key has
/// no terminal encoding. `app_cursor` selects the application-cursor-key form for arrows / home
/// / end (shells and full-screen apps toggle this via DECCKM).
pub fn key_to_bytes(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let mut out: Vec<u8> = Vec::new();
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                match ctrl_byte(c) {
                    Some(b) => out.push(b),
                    None => push_char(&mut out, c),
                }
            } else {
                push_char(&mut out, c);
            }
            // Alt/Meta prefixes a printable with ESC (readline word motions, etc.).
            if alt {
                out.insert(0, 0x1b);
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out = csi_arrow(b'D', app_cursor),
        KeyCode::Right => out = csi_arrow(b'C', app_cursor),
        KeyCode::Up => out = csi_arrow(b'A', app_cursor),
        KeyCode::Down => out = csi_arrow(b'B', app_cursor),
        KeyCode::Home => out = csi_arrow(b'H', app_cursor),
        KeyCode::End => out = csi_arrow(b'F', app_cursor),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n) => out = function_key(n)?,
        _ => return None,
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Append `c` as UTF-8.
fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// The control byte for `Ctrl+<c>`, if one exists (C0 range).
fn ctrl_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        'a'..='z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

/// A cursor / navigation escape ending in `final_byte`, in normal (`ESC [`) or application
/// (`ESC O`) form.
fn csi_arrow(final_byte: u8, app_cursor: bool) -> Vec<u8> {
    let intro = if app_cursor { b'O' } else { b'[' };
    vec![0x1b, intro, final_byte]
}

/// The xterm escape for function key `n` (F1–F12), or `None` beyond that.
fn function_key(n: u8) -> Option<Vec<u8>> {
    let seq: &[u8] = match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => return None,
    };
    Some(seq.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_and_control_chars_encode() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::NONE), false),
            Some(vec![b'a'])
        );
        // Ctrl+C → ETX (0x03), the SIGINT byte.
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Some(vec![0x03])
        );
        // Ctrl+A → SOH (0x01).
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::CONTROL), false),
            Some(vec![0x01])
        );
        // Alt+b → ESC b (readline "word back").
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('b'), KeyModifiers::ALT), false),
            Some(vec![0x1b, b'b'])
        );
    }

    #[test]
    fn special_keys_encode() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Enter, KeyModifiers::NONE), false),
            Some(vec![b'\r'])
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Backspace, KeyModifiers::NONE), false),
            Some(vec![0x7f])
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Tab, KeyModifiers::NONE), false),
            Some(vec![b'\t'])
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Delete, KeyModifiers::NONE), false),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::F(1), KeyModifiers::NONE), false),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::F(5), KeyModifiers::NONE), false),
            Some(b"\x1b[15~".to_vec())
        );
    }

    #[test]
    fn arrows_respect_application_cursor_mode() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Home, KeyModifiers::NONE), false),
            Some(b"\x1b[H".to_vec())
        );
    }

    #[test]
    fn color_mapping() {
        assert_eq!(vt_color(vt100::Color::Default), None);
        assert_eq!(vt_color(vt100::Color::Idx(4)), Some(Color::Indexed(4)));
        assert_eq!(
            vt_color(vt100::Color::Rgb(10, 20, 30)),
            Some(Color::Rgb(10, 20, 30))
        );
    }

    #[test]
    fn tab_index_math() {
        assert_eq!(next_index(3, 0), 1);
        assert_eq!(next_index(3, 2), 0);
        assert_eq!(next_index(0, 0), 0);
        assert_eq!(prev_index(3, 0), 2);
        assert_eq!(prev_index(3, 1), 0);

        // Closing before the active tab shifts the active index down.
        assert_eq!(index_after_close(3, 2, 0), 1);
        // Closing the active last tab clamps to the new last.
        assert_eq!(index_after_close(3, 2, 2), 1);
        // Closing after the active tab leaves it put.
        assert_eq!(index_after_close(3, 0, 2), 0);
        // Closing the only tab.
        assert_eq!(index_after_close(1, 0, 0), 0);
    }

    #[test]
    fn panel_flags_and_header() {
        let mut panel = TerminalPanel::new(80);
        // Clamped to the sane range.
        assert_eq!(panel.height, 60);
        assert!(!panel.open && !panel.minimized);
        assert!(panel.active_terminal().is_none());
        // Closing an empty panel reports "now empty".
        assert!(panel.close_active());

        panel.toggle_minimized();
        assert!(panel.minimized);
        // With no terminals the header is just the two controls.
        let segs = panel.header_segments();
        assert_eq!(segs.first().map(|s| s.1), Some(HeaderHit::Minimize));
        assert_eq!(segs.last().map(|s| s.1), Some(HeaderHit::New));
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn default_shell_prefers_override() {
        assert_eq!(default_shell(Some("/bin/zsh")), "/bin/zsh");
        assert_eq!(default_shell(Some("  ")), default_shell(None));
        assert!(!default_shell(None).is_empty());
    }
}
