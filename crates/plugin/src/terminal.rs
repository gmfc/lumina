//! The terminal dock **RawPTY port** — the primitive, pty-free surface a plugin uses to own the
//! dock/tab lifecycle. The app owns the concrete PTY spawn, the vt100 parse, the byte budgeting,
//! the grid render, and key forwarding (all keyed by [`TerminalId`]); the plugin owns which
//! terminals exist, the active tab, and the open/minimized state, publishing a [`TerminalView`]
//! the app renders (invariant #8). See [`crate::Host::terminal_open`] / `set_terminal_view`.

/// An opaque handle to an app-owned PTY terminal, allocated by [`crate::Host::terminal_open`]. The
/// plugin tracks these in its tab order; the app maps each to a concrete `vt100`/pty session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalId(pub u64);

/// The dock state a plugin publishes for the app to render (a pure function of state). The app
/// draws the header from `order` (looking each id up for its title / exited flag) and the active
/// terminal's grid; the plugin never produces cells, only this lifecycle description.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalView {
    /// Whether the dock occupies layout space at all.
    pub open: bool,
    /// When open, whether it is collapsed to just its header row.
    pub minimized: bool,
    /// Index into `order` of the focused tab.
    pub active: usize,
    /// The tab order, left to right.
    pub order: Vec<TerminalId>,
}
