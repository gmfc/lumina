//! LSP signature help, implemented **as a plugin** (invariant #3).
//!
//! Parameter hints while typing a call. The plugin fires `textDocument/signatureHelp` through
//! [`Host::lsp_request`] on the trigger characters `(` and `,` (and re-fires on any edit while a
//! hint is showing, so the active parameter tracks the cursor), and renders the app-formatted
//! signature line into a statusline item. The LSP transport + the signature formatting stay
//! app-side; this owns the trigger policy and the display. Closing on `)` / cursor leaving the
//! call (the server answers `None`) / switching documents.

use editor_plugin::{Contributions, Event, Host, LspRequestKind, Plugin};

#[derive(Default)]
pub(crate) struct SignatureHelpPlugin {
    /// Whether a hint is currently showing (drives retrigger-on-edit and clean close).
    open: bool,
}

impl SignatureHelpPlugin {
    const ID: &'static str = "signature-help";
    const STATUS: &'static str = "lsp.signature";

    /// The character immediately before the primary cursor, if any.
    fn char_before_cursor(host: &dyn Host) -> Option<char> {
        let id = host.active_doc()?;
        let doc = host.workspace().documents.get(id)?;
        let head = doc.selections.primary().head;
        (head > 0).then(|| doc.rope().char(head - 1))
    }

    fn close(&mut self, host: &mut dyn Host) {
        self.open = false;
        host.set_status(Self::STATUS, String::new());
    }

    /// Decide whether an edit/cursor move should (re)request signature help.
    fn on_change(&mut self, host: &mut dyn Host) {
        if !host.lsp_enabled() {
            return;
        }
        match Self::char_before_cursor(host) {
            Some(')') => self.close(host),
            // Trigger characters, or keep the active parameter fresh while a hint is up.
            Some('(') | Some(',') => host.lsp_request(LspRequestKind::SignatureHelp),
            _ if self.open => host.lsp_request(LspRequestKind::SignatureHelp),
            _ => {}
        }
    }
}

impl Plugin for SignatureHelpPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("lsp.signatureHelp", "Go: Signature Help")
            .keybinding("ctrl+shift+space", "lsp.signatureHelp")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if command_id == "lsp.signatureHelp" {
            if host.lsp_enabled() {
                host.lsp_request(LspRequestKind::SignatureHelp);
            }
            return true;
        }
        false
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        match event {
            Event::LspSignatureHelp(Some(text)) => {
                self.open = true;
                host.set_status(Self::STATUS, text.clone());
            }
            Event::LspSignatureHelp(None) => self.close(host),
            Event::DidChange(id) | Event::DidChangeCursor(id) if host.active_doc() == Some(*id) => {
                self.on_change(host)
            }
            Event::DidChangeActive(_) | Event::ExternalReload(_) if self.open => self.close(host),
            _ => {}
        }
    }
}
