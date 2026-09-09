//! LSP diagnostics, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the diagnostics model (`DocId → Vec<LspDiagnostic>`), fed the primitive
//! diagnostics the app translates from `editor-lsp` and broadcasts as [`Event::LspDiagnostics`].
//! It reaches the editor only through [`Host`]: it publishes the underline spans + gutter markers
//! as the `"lsp.diag"` decoration layer, the caret-diagnostic message as a status item, and
//! navigates next/previous problem via [`Host::set_selections`]. The UTF-16↔char mapping stays
//! app-side behind [`Host::lsp_pos_to_offset`]; the LSP transport stays app-side entirely.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use editor_core::{DocId, Selection, Selections};
use editor_plugin::{
    Contributions, Decoration, DecorationSet, Event, GutterMark, Host, LspDiagnostic, LspSeverity,
    PanelContent, PanelLine, PanelLocation, Plugin, Span,
};

/// The status item id the caret-diagnostic message is published under.
const STATUS_ID: &str = "lsp.diag";
/// The status item id the active doc's `"<errors> <warnings>"` count is published under, for the
/// footer LSP badge (empty when the active doc is clean).
const COUNT_ID: &str = "lsp.diag.count";
const LAYER: &str = "lsp.diag";
/// The workspace-wide problems list. A `Bottom` panel, so it shares the results dock.
const PANEL: &str = "lsp.problems";
/// Rows kept in the problems panel. A project-wide check on a broken workspace can produce
/// thousands; past a screenful or two the list stops being a list.
const PROBLEM_CAP: usize = 500;

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

/// The caret-diagnostic line: `<glyph> [source: ]message[ [code]]`, e.g.
/// `E rustc: cannot find value `x` in this scope [E0425]`.
fn format_diagnostic(d: &LspDiagnostic) -> String {
    let mut out = String::new();
    out.push(sev_glyph(d.severity));
    out.push(' ');
    if let Some(src) = &d.source {
        out.push_str(src);
        out.push_str(": ");
    }
    out.push_str(&d.message);
    if let Some(code) = &d.code {
        out.push_str(" [");
        out.push_str(code);
        out.push(']');
    }
    out
}

#[derive(Default)]
pub(crate) struct DiagnosticsPlugin {
    diags: HashMap<DocId, Vec<LspDiagnostic>>,
    /// Diagnostics keyed by path, covering files with no open document — which is most of a
    /// workspace during a project-wide check. `diags` stays keyed by `DocId` because the
    /// decoration and caret-status paths need a live document; this is what the problems panel
    /// lists from.
    by_path: BTreeMap<PathBuf, Vec<LspDiagnostic>>,
    /// Whether the problems panel is showing. The dock draws a `Bottom` panel whenever it has
    /// rows, so "closed" means publishing none.
    problems_open: bool,
}

impl DiagnosticsPlugin {
    /// Render the workspace problems list, or clear it when the panel is closed.
    ///
    /// Grouped by file, errors before warnings before hints, each row carrying `path\tline` so a
    /// click opens it. Lists every file the server has reported on, not just the open ones —
    /// which is the point: the diagnostics that matter most after a project-wide check are in
    /// files you have not opened yet.
    fn publish_problems(&self, host: &mut dyn Host) {
        if !self.problems_open {
            host.set_panel(PANEL, PanelContent::default());
            return;
        }
        let root = host.root().to_path_buf();
        let mut lines: Vec<PanelLine> = Vec::new();
        let (mut errors, mut warnings, mut shown) = (0usize, 0usize, 0usize);
        for diags in self.by_path.values() {
            for d in diags {
                match d.severity {
                    LspSeverity::Error => errors += 1,
                    LspSeverity::Warning => warnings += 1,
                    _ => {}
                }
            }
        }
        for (path, diags) in &self.by_path {
            if diags.is_empty() || shown >= PROBLEM_CAP {
                continue;
            }
            let name = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            lines.push(PanelLine::new(vec![Span::new(name, "dir")]));
            let mut sorted: Vec<&LspDiagnostic> = diags.iter().collect();
            sorted.sort_by_key(|d| (sev_rank(d.severity), d.line));
            for d in sorted {
                if shown >= PROBLEM_CAP {
                    break;
                }
                shown += 1;
                lines.push(
                    PanelLine::new(vec![
                        Span::new(
                            format!("  {} {:>5}: ", sev_glyph(d.severity), d.line + 1),
                            sev_suffix(d.severity),
                        ),
                        Span::new(
                            d.message
                                .replace('\n', " ")
                                .chars()
                                .take(160)
                                .collect::<String>(),
                            "text",
                        ),
                    ])
                    .payload(format!(
                        "{}\t{}",
                        path.to_string_lossy(),
                        d.line
                    )),
                );
            }
        }
        if lines.is_empty() {
            lines.push(PanelLine::new(vec![Span::new("No problems", "dim")]));
        } else {
            let capped = if shown >= PROBLEM_CAP {
                format!("  (showing the first {PROBLEM_CAP})")
            } else {
                String::new()
            };
            lines.insert(
                0,
                PanelLine::new(vec![Span::new(
                    format!("{errors} error(s), {warnings} warning(s){capped}"),
                    "title",
                )]),
            );
        }
        host.set_panel(PANEL, PanelContent { lines, selected: 0 });
    }

    /// `lsp.problems`: show or hide the problems list.
    fn toggle_problems(&mut self, host: &mut dyn Host) {
        self.problems_open = !self.problems_open;
        self.publish_problems(host);
    }

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
        host.set_decorations(
            doc,
            LAYER,
            DecorationSet {
                spans,
                gutter,
                ..Default::default()
            },
        );
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
                (head >= start && head <= end).then(|| format_diagnostic(d))
            })
        });
        host.set_status(STATUS_ID, msg.unwrap_or_default());
    }

    /// Publish the *active* doc's error/warning counts as `"<errors> <warnings>"` for the footer
    /// LSP badge, or empty when it is clean. Keyed on the active doc (not an arbitrary one) so the
    /// badge always tracks the focused file, even when diagnostics arrive for a background buffer.
    fn refresh_diag_count(&self, host: &mut dyn Host) {
        let counts = host
            .active_doc()
            .and_then(|doc| self.diags.get(&doc))
            .map(|ds| {
                ds.iter()
                    .fold((0usize, 0usize), |(e, w), d| match d.severity {
                        LspSeverity::Error => (e + 1, w),
                        LspSeverity::Warning => (e, w + 1),
                        _ => (e, w),
                    })
            });
        let text = match counts {
            Some((e, w)) if e > 0 || w > 0 => format!("{e} {w}"),
            _ => String::new(),
        };
        host.set_status(COUNT_ID, text);
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
            .command("lsp.problems", "View: Problems")
            .panel(PANEL, "Problems", PanelLocation::Bottom)
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "lsp.nextDiagnostic" => self.navigate(host, 1),
            "lsp.prevDiagnostic" => self.navigate(host, -1),
            "lsp.problems" => self.toggle_problems(host),
            _ => return false,
        }
        true
    }

    fn on_panel_activate(&mut self, panel_id: &str, payload: &str, host: &mut dyn Host) {
        if panel_id != PANEL {
            return;
        }
        if let Some((path, line)) = payload.rsplit_once('\t') {
            if let Ok(line) = line.parse::<usize>() {
                host.open_path_at(Path::new(path), line);
            }
        }
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        match event {
            Event::LspDiagnostics {
                doc,
                path,
                diagnostics,
            } => {
                if diagnostics.is_empty() {
                    self.by_path.remove(path);
                } else {
                    self.by_path.insert(path.clone(), diagnostics.clone());
                }
                self.publish_problems(host);
                // The rest needs a live document: decorations and the caret status are per-buffer.
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
                self.refresh_diag_count(host);
            }
            // An edit remaps char offsets (the stored line/utf16 positions are re-resolved).
            Event::DidChange(id) => {
                self.publish_decorations(host, *id);
                self.refresh_status(host, *id);
                self.refresh_diag_count(host);
            }
            Event::DidChangeCursor(id) => self.refresh_status(host, *id),
            Event::DidChangeActive(_) => {
                self.prune(host);
                if let Some(id) = host.active_doc() {
                    self.refresh_status(host, id);
                }
                self.refresh_diag_count(host);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(msg: &str, source: Option<&str>, code: Option<&str>) -> LspDiagnostic {
        LspDiagnostic {
            line: 0,
            start_char16: 0,
            end_line: 0,
            end_char16: 1,
            severity: LspSeverity::Error,
            message: msg.into(),
            source: source.map(str::to_string),
            code: code.map(str::to_string),
        }
    }

    #[test]
    fn formats_source_prefix_and_code_suffix() {
        assert_eq!(
            format_diagnostic(&diag("no `x`", Some("rustc"), Some("E0425"))),
            "E rustc: no `x` [E0425]"
        );
        assert_eq!(format_diagnostic(&diag("plain", None, None)), "E plain");
    }
}
