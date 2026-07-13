//! LSP folding, implemented **as a plugin** (invariant #3).
//!
//! Fetches foldable regions (§7.3) and marks each fold's **start line** in the gutter with a
//! chevron so the user can see where regions begin. The app requests ranges per document
//! (transport stays app-side) and broadcasts them as [`Event::LspFoldingRanges`]; this plugin
//! publishes the markers as the `"lsp.fold"` gutter layer.
//!
//! Interactive collapse (hiding a region's body) is a follow-up: it needs a visible-line
//! coordinate model threaded through the renderer, scroll, cursor, and mouse mapping — a
//! cross-cutting change kept out of this indicator-only pass.

use editor_core::DocId;
use editor_plugin::{DecorationSet, Event, GutterMark, Host, LspFoldingRange, Plugin};

const LAYER: &str = "lsp.fold";
/// The chevron drawn at a fold's start line (a collapsed `▸` is the follow-up).
const GLYPH: char = '⌄';

#[derive(Default)]
pub struct FoldingPlugin;

impl FoldingPlugin {
    fn publish(host: &mut dyn Host, doc: DocId, ranges: &[LspFoldingRange]) {
        if ranges.is_empty() {
            host.clear_decorations(doc, LAYER);
            return;
        }
        let gutter = ranges
            .iter()
            .filter(|r| r.end_line > r.start_line) // a real multi-line region
            .map(|r| GutterMark::new(r.start_line as usize, GLYPH, LAYER))
            .collect();
        host.set_decorations(
            doc,
            LAYER,
            DecorationSet {
                gutter,
                ..Default::default()
            },
        );
    }
}

impl Plugin for FoldingPlugin {
    fn id(&self) -> &str {
        "folding"
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if let Event::LspFoldingRanges {
            doc: Some(doc),
            ranges,
        } = event
        {
            Self::publish(host, *doc, ranges);
        }
    }
}
