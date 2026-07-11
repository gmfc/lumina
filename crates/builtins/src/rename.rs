//! LSP rename application, implemented **as a plugin** (invariant #3).
//!
//! The rename *request* is the `lsp` plugin; this owns the *response*. It is purely reactive: the
//! app translates the server's `WorkspaceEdit` into a primitive [`Event::LspWorkspaceEdit`] (URIs
//! resolved to paths) and this plugin forwards it through [`Host::apply_workspace_edit`]. The app
//! owns the file IO + UTF-16↔char mapping + the multi-file transactions (they can't be
//! primitive-typed on the Host surface), so this stays a thin forwarder — like the theme toggle,
//! it moves the feature onto the shared event path without pulling `editor-lsp` into builtins.

use editor_plugin::{Event, Host, Plugin};

pub struct RenamePlugin;

impl Plugin for RenamePlugin {
    fn id(&self) -> &str {
        "rename"
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if let Event::LspWorkspaceEdit(edit) = event {
            host.apply_workspace_edit(edit.clone());
        }
    }
}
