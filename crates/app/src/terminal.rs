//! The integrated terminal panel: a minimizable, tabbed dock below the editor that hosts
//! real shell sessions. Each tab drives a pseudo-terminal (via `portable-pty`); the shell's
//! byte stream is parsed by `vt100` into a screen grid the renderer reads — so the panel stays
//! a pure function of state, exactly like the editor (invariant #8).
//!
//! Threading mirrors the rest of the app (search / git / fs-watch, `worker.rs`): each terminal
//! owns a reader thread that pushes output through the shared `WorkerMsg` channel, so every
//! mutation still lands on the single-threaded main loop. The panel is deliberately small and
//! composable — split panes, a task runner, or other bottom-dock contributions can grow later.
//!
//! The dock **lifecycle** (which terminals exist, the active tab, open/minimized) is owned by the
//! `terminal` builtin plugin, which drives it through the RawPTY Host port; `EditorState` holds the
//! PTY sessions keyed by `TerminalId` and renders the plugin's published `TerminalView`.
//!
//! - [`session`] — one PTY-backed [`session::Terminal`] and its reader thread (app-owned).
//! - [`panel`] — the header layout hit type [`HeaderHit`].
//! - [`keys`] — key/color translation to PTY conventions and the default-shell resolver.

mod keys;
mod panel;
mod session;

pub use keys::{default_shell, key_to_bytes, vt_color};
pub use panel::HeaderHit;
pub use session::Terminal;
