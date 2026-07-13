//! LSP code lens, implemented **as a plugin** (invariant #3).
//!
//! Actionable annotations (§6.4) — rendered as inline **virtual text** at the lens position. The
//! app requests lenses per document and resolves their commands (transport stays app-side), then
//! broadcasts the resolved set as [`Event::LspCodeLenses`]; this plugin publishes each `title` as
//! the `"lsp.lens"` virtual-text layer. A true above-the-line placement needs virtual rows the
//! TUI renderer doesn't have yet, so lenses render as an inline prefix on their code line — a
//! common TUI adaptation. Display-only for now (activation is a follow-up). Cleared when empty.

use editor_core::DocId;
use editor_plugin::{DecorationSet, Event, Host, LspCodeLens, Plugin, VirtualText};

const LAYER: &str = "lsp.lens";

#[derive(Default)]
pub struct CodeLensPlugin;

impl CodeLensPlugin {
    fn publish(host: &mut dyn Host, doc: DocId, lenses: &[LspCodeLens]) {
        if lenses.is_empty() {
            host.clear_decorations(doc, LAYER);
            return;
        }
        let virtual_text = lenses
            .iter()
            .filter(|l| !l.title.is_empty())
            .map(|l| {
                let offset = host.lsp_pos_to_offset(doc, l.line, l.char16);
                // A trailing space separates the lens from the code it prefixes.
                let mut vt = VirtualText::new(offset, l.title.clone(), LAYER);
                vt.pad_right = true;
                vt
            })
            .collect();
        host.set_decorations(doc, LAYER, DecorationSet::virtual_text(virtual_text));
    }
}

impl Plugin for CodeLensPlugin {
    fn id(&self) -> &str {
        "code-lens"
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if let Event::LspCodeLenses {
            doc: Some(doc),
            lenses,
        } = event
        {
            Self::publish(host, *doc, lenses);
        }
    }
}
