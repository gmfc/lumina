//! LSP hover, implemented **as a plugin** (invariant #3).
//!
//! The hover *request* is the `lsp` plugin; this owns the *response*. It is purely reactive: the
//! app renders a hover reply into a primitive [`Event::LspHover`] (already-formatted text), and
//! this plugin shows it in a dismissable info box through [`Host::show_info`]. The LSP transport
//! and the overlay rendering/dismissal stay app-side; the plugin owns only the response→display
//! wiring — a one-liner, but it moves hover onto the same event path as every other feature.

use editor_plugin::{Event, Host, Plugin};

pub(crate) struct HoverPlugin;

impl Plugin for HoverPlugin {
    fn id(&self) -> &str {
        "hover"
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if let Event::LspHover(text) = event {
            host.show_info(text.clone());
        }
    }
}
