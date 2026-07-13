//! LSP document highlight, implemented **as a plugin** (invariant #3).
//!
//! Highlights the occurrences of the symbol under the cursor (read vs write distinctly). Purely
//! reactive: on a cursor move onto a word it fires `textDocument/documentHighlight` through
//! [`Host::lsp_request`] (superseded/cancelled by the next move via the app's staleness machinery),
//! and paints the returned ranges as the `"lsp.highlight"` decoration layer. Cleared on edit
//! (positions go stale — re-requested when the cursor next settles) and on switching documents.
//! The LSP transport + UTF-16↔char mapping stay app-side.

use editor_plugin::{Decoration, DecorationSet, Event, Host, LspHighlight, LspRequestKind, Plugin};

const LAYER: &str = "lsp.highlight";

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[derive(Default)]
pub struct DocumentHighlightPlugin;

impl DocumentHighlightPlugin {
    /// Whether the primary cursor is on or just after a word character — i.e. there is a symbol
    /// worth highlighting. Skips requests when idling on whitespace/punctuation.
    fn cursor_on_word(host: &dyn Host) -> bool {
        let Some(doc) = host
            .active_doc()
            .and_then(|id| host.workspace().documents.get(id))
        else {
            return false;
        };
        let head = doc.selections.primary().head;
        let rope = doc.rope();
        let at = (head < rope.len_chars()) && is_word(rope.char(head));
        let before = head > 0 && is_word(rope.char(head - 1));
        at || before
    }

    fn clear(host: &mut dyn Host) {
        if let Some(doc) = host.active_doc() {
            host.clear_decorations(doc, LAYER);
        }
    }

    fn publish(host: &mut dyn Host, hls: &[LspHighlight]) {
        let Some(doc) = host.active_doc() else {
            return;
        };
        if hls.is_empty() {
            host.clear_decorations(doc, LAYER);
            return;
        }
        let spans = hls
            .iter()
            .map(|h| {
                let start = host.lsp_pos_to_offset(doc, h.line, h.start_char16);
                let end = host
                    .lsp_pos_to_offset(doc, h.end_line, h.end_char16)
                    .max(start + 1);
                let scope = match h.kind {
                    2 => "lsp.highlight.read",
                    3 => "lsp.highlight.write",
                    _ => "lsp.highlight.text",
                };
                Decoration::new((start, end), scope.to_string())
            })
            .collect();
        host.set_decorations(
            doc,
            LAYER,
            DecorationSet {
                spans,
                gutter: Vec::new(),
            },
        );
    }
}

impl Plugin for DocumentHighlightPlugin {
    fn id(&self) -> &str {
        "document-highlight"
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        match event {
            Event::LspHighlights(hls) => Self::publish(host, hls),
            Event::DidChangeCursor(id) if host.active_doc() == Some(*id) => {
                if host.lsp_enabled() && Self::cursor_on_word(host) {
                    host.lsp_request(LspRequestKind::DocumentHighlight);
                } else {
                    Self::clear(host);
                }
            }
            Event::DidChange(_) | Event::DidChangeActive(_) | Event::ExternalReload(_) => {
                Self::clear(host)
            }
            _ => {}
        }
    }
}
