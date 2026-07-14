//! The bottom dock header layout shared by the renderer and the mouse router.
//!
//! The terminal lifecycle lives in the `terminal` builtin plugin (`TerminalView`); the LSP tab is
//! app-owned. The header is a single strip: a minimize control, the dock tab buttons
//! (`Terminal`/`LSP`), and — when the terminal tab is showing — its per-session tabs + `+`. This
//! hit type names the clickable regions the mouse router resolves.

use crate::editor::DockTab;

/// A clickable region of the dock header, returned by hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderHit {
    /// The minimize / restore control (acts on the visible tab).
    Minimize,
    /// A top-level dock tab button (`Terminal` / `LSP`).
    DockTab(DockTab),
    /// The tab for terminal session at this index.
    Tab(usize),
    /// The "new terminal" (`+`) control.
    New,
}
