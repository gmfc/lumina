//! Terminal dock lifecycle intent — the crossterm/pty-free way a plugin drives the bottom
//! terminal panel. The app owns the PTY spawn, the vt100 parse, the byte budgeting, the grid
//! render, and key forwarding; the plugin only expresses which lifecycle action to take (see
//! [`crate::Host::terminal_op`]).

/// A lifecycle action on the terminal dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalOp {
    /// Open the dock (spawning a first shell if empty) or close it if already focused.
    Toggle,
    /// Open a new shell tab.
    New,
    /// Close the active shell tab (closing the dock when the last one goes).
    Close,
    /// Minimize (header-only) or restore the dock.
    Minimize,
    /// Focus the next shell tab.
    Next,
    /// Focus the previous shell tab.
    Prev,
}
