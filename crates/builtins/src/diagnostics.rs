//! LSP diagnostics, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the diagnostics model (`DocId → Vec<LspDiagnostic>`), fed the primitive
//! diagnostics the app translates from `editor-lsp` and broadcasts as [`Event::LspDiagnostics`].
//! It reaches the editor only through [`Host`]: it publishes the underline spans + gutter markers
//! as the `"lsp.diag"` decoration layer, the caret-diagnostic message as a status item, and
//! navigates next/previous problem via [`Host::set_selections`]. The UTF-16↔char mapping stays
//! app-side behind [`Host::lsp_pos_to_offset`]; the LSP transport stays app-side entirely.

use std::collections::{BTreeMap, HashMap};

use editor_core::{DocId, Selection, Selections};
use editor_plugin::{
    Contributions, Decoration, DecorationSet, Event, GutterMark, Host, LspDiagnostic, LspSeverity,
    Plugin,
};

/// The status item id the caret-diagnostic message is published under.
const STATUS_ID: &str = "lsp.diag";
const LAYER: &str = "lsp.diag";

fn sev_suffix(s: LspSeverity) -> &'static str {
    match s {
        LspSeverity::Error => "error",
        LspSeverity::Warning => "warning",
        LspSeverity::Info => "info",
        LspSeverity::Hint => "hint",
    }
}
fn sev_glyph(s: LspSeverity) -> char {
    match s {
        LspSeverity::Error => 'E',
        LspSeverity::Warning => 'W',
        LspSeverity::Info => 'i',
        LspSeverity::Hint => 'h',
    }
}
fn sev_rank(s: LspSeverity) -> u8 {
    match s {
        LspSeverity::Error => 0,
        LspSeverity::Warning => 1,
        LspSeverity::Info => 2,
        LspSeverity::Hint => 3,
    }
}

#[derive(Default)]
pub struct DiagnosticsPlugin {
    diags: HashMap<DocId, Vec<LspDiagnostic>>,
}

impl DiagnosticsPlugin {
    /// Publish (or clear) the `"lsp.diag"` decoration layer for `doc`: an underline span per
    /// diagnostic + one gutter mark per line carrying its highest-severity glyph. Offsets are
    /// resolved fresh against the current text (via the host), so an edit remaps them.
    fn publish_decorations(&self, host: &mut dyn Host, doc: DocId) {
        let Some(diags) = self.diags.get(&doc).filter(|d| !d.is_empty()) else {
            host.clear_decorations(doc, LAYER);
            return;
        };
        let mut spans = Vec::with_capacity(diags.len());
        let mut per_line: BTreeMap<usize, LspSeverity> = BTreeMap::new();
        for d in diags {
            let start = host.lsp_pos_to_offset(doc, d.line, d.start_char16);
            // For a multi-line diagnostic, underline to the end of the start line (u32::MAX clamps
            // to the line's char count) — matching the former per-line renderer.
            let end = if d.end_line == d.line {
                host.lsp_pos_to_offset(doc, d.line, d.end_char16)
            } else {
                host.lsp_pos_to_offset(doc, d.line, u32::MAX)
            };
            let end = end.max(start + 1);
            spans.push(Decoration::new(
                (start, end),
                format!("lsp.diag.{}", sev_suffix(d.severity)),
            ));
            per_line
                .entry(d.line as usize)
                .and_modify(|cur| {
                    if sev_rank(d.severity) < sev_rank(*cur) {
                        *cur = d.severity;
                    }
                })
                .or_insert(d.severity);
        }
        let gutter = per_line
            .into_iter()
            .map(|(line, s)| {
                GutterMark::new(
                    line,
                    sev_glyph(s),
                    format!("lsp.diag.mark.{}", sev_suffix(s)),
                )
            })
            .collect();
        host.set_decorations(doc, LAYER, DecorationSet { spans, gutter });
    }

    /// Update the status item to the diagnostic under the primary caret, or clear it.
    fn refresh_status(&self, host: &mut dyn Host, doc: DocId) {
        let head = host
            .workspace()
            .documents
            .get(doc)
            .map(|d| d.selections.primary().head);
        let msg = head.and_then(|head| {
            self.diags.get(&doc)?.iter().find_map(|d| {
                let start = host.lsp_pos_to_offset(doc, d.line, d.start_char16);
                let end = host
                    .lsp_pos_to_offset(doc, d.end_line, d.end_char16)
                    .max(start);
                (head >= start && head <= end)
                    .then(|| format!("{} {}", sev_glyph(d.severity), d.message))
            })
        });
        host.set_status(STATUS_ID, msg.unwrap_or_default());
    }

    /// Jump the caret to the next (`dir > 0`) / previous diagnostic, wrapping.
    fn navigate(&self, host: &mut dyn Host, dir: isize) {
        let Some(doc) = host.active_doc() else {
            return;
        };
        let Some(diags) = self.diags.get(&doc).filter(|d| !d.is_empty()) else {
            return;
        };
        let mut offs: Vec<usize> = diags
            .iter()
            .map(|d| host.lsp_pos_to_offset(doc, d.line, d.start_char16))
            .collect();
        offs.sort_unstable();
        offs.dedup();
        let head = host
            .workspace()
            .documents
            .get(doc)
            .map(|d| d.selections.primary().head)
            .unwrap_or(0);
        let target = if dir > 0 {
            offs.iter().copied().find(|&o| o > head).unwrap_or(offs[0])
        } else {
            offs.iter()
                .rev()
                .copied()
                .find(|&o| o < head)
                .unwrap_or_else(|| *offs.last().unwrap())
        };
        host.set_selections(doc, Selections::single(Selection::caret(target)));
    }

    /// Drop diagnostics for documents that are no longer open.
    fn prune(&mut self, host: &dyn Host) {
        self.diags
            .retain(|id, _| host.workspace().documents.get(*id).is_some());
    }
}

impl Plugin for DiagnosticsPlugin {
    fn id(&self) -> &str {
        "diagnostics"
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("lsp.nextDiagnostic", "Go: Next Problem")
            .command("lsp.prevDiagnostic", "Go: Previous Problem")
            .keybinding("f8", "lsp.nextDiagnostic")
            .keybinding("shift+f8", "lsp.prevDiagnostic")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "lsp.nextDiagnostic" => self.navigate(host, 1),
            "lsp.prevDiagnostic" => self.navigate(host, -1),
            _ => return false,
        }
        true
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        match event {
            Event::LspDiagnostics { doc, diagnostics } => {
                let Some(id) = doc else {
                    return;
                };
                if diagnostics.is_empty() {
                    self.diags.remove(id);
                } else {
                    self.diags.insert(*id, diagnostics.clone());
                }
                self.publish_decorations(host, *id);
                self.refresh_status(host, *id);
            }
            // An edit remaps char offsets (the stored line/utf16 positions are re-resolved).
            Event::DidChange(id) => {
                self.publish_decorations(host, *id);
                self.refresh_status(host, *id);
            }
            Event::DidChangeCursor(id) => self.refresh_status(host, *id),
            Event::DidChangeActive(_) => {
                self.prune(host);
                if let Some(id) = host.active_doc() {
                    self.refresh_status(host, id);
                }
            }
            _ => {}
        }
    }
}
