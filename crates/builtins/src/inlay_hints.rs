//! LSP inlay hints, implemented **as a plugin** (invariant #3).
//!
//! Type/parameter hints (§7.2) rendered as inline **virtual text** — the app requests whole-document
//! hints (transport + UTF-16↔char mapping stay app-side) and broadcasts them as
//! [`Event::LspInlayHints`]; this plugin resolves each position to a char offset and publishes it
//! as the `"lsp.inlay"` virtual-text layer, which the editor renderer draws between the real
//! characters. Cleared when the layer comes back empty.

use editor_core::DocId;
use editor_plugin::{DecorationSet, Event, Host, LspInlayHint, Plugin, VirtualText};

const LAYER: &str = "lsp.inlay";

/// The theme scope for a hint, by `kind` (1 Type, 2 Parameter, else generic type styling).
fn scope_for(kind: u8) -> &'static str {
    match kind {
        2 => "lsp.inlay.param",
        _ => "lsp.inlay.type",
    }
}

#[derive(Default)]
pub(crate) struct InlayHintsPlugin;

impl InlayHintsPlugin {
    fn publish(host: &mut dyn Host, doc: DocId, hints: &[LspInlayHint]) {
        if hints.is_empty() {
            host.clear_decorations(doc, LAYER);
            return;
        }
        let virtual_text = hints
            .iter()
            .filter(|h| !h.label.is_empty())
            .map(|h| {
                let offset = host.lsp_pos_to_offset(doc, h.line, h.char16);
                VirtualText {
                    offset,
                    text: h.label.clone(),
                    style: scope_for(h.kind).to_string(),
                    pad_left: h.pad_left,
                    pad_right: h.pad_right,
                }
            })
            .collect();
        host.set_decorations(doc, LAYER, DecorationSet::virtual_text(virtual_text));
    }
}

impl Plugin for InlayHintsPlugin {
    fn id(&self) -> &str {
        "inlay-hints"
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if let Event::LspInlayHints {
            doc: Some(doc),
            hints,
        } = event
        {
            Self::publish(host, *doc, hints);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_kind_to_scope() {
        assert_eq!(scope_for(1), "lsp.inlay.type");
        assert_eq!(scope_for(2), "lsp.inlay.param");
        assert_eq!(scope_for(0), "lsp.inlay.type");
    }
}
