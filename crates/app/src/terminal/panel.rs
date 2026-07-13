//! The terminal dock header layout shared by the renderer and the mouse router.
//!
//! The dock lifecycle now lives in the `terminal` builtin plugin (`TerminalView`); the app keeps
//! only this hit type, which names the clickable header regions the mouse router resolves.

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
