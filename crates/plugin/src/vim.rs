//! Primitive Vim render state — the pty/crossterm-free mirror the `vim` plugin publishes so the
//! pure renderer (invariant #8) can draw the mode badge and shade the visual selection without
//! reaching into the plugin. The plugin owns the whole modal state machine; only what the renderer
//! needs crosses back through [`crate::Host::set_vim_view`].

/// The current Vim mode, mirrored for the status badge + visual-selection shading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl VimMode {
    /// The badge label (`NORMAL`, `INSERT`, `VISUAL`, `V-LINE`).
    pub fn label(self) -> &'static str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "V-LINE",
        }
    }
}

/// What the renderer needs from the Vim plugin: the mode (badge + visual shading) and any pending
/// command hint (the count/operator shown while a multi-key command is mid-entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VimView {
    pub mode: VimMode,
    /// A short hint for the status line while a command is pending (e.g. `"2d"`), if any.
    pub pending: Option<String>,
}
