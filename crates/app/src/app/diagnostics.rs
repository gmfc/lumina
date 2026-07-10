//! Diagnostic navigation and the caret-diagnostic lookup.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use editor_plugin::{Decoration, DecorationSet, GutterMark};

use super::*;

/// Severity → the semantic decoration-style suffix / gutter glyph / precedence rank the theme and
/// gutter renderer key on (matching the former `ui::util` severity helpers).
fn sev_suffix(s: editor_lsp::Severity) -> &'static str {
    use editor_lsp::Severity::*;
    match s {
        Error => "error",
        Warning => "warning",
        Info => "info",
        Hint => "hint",
    }
}
fn sev_glyph(s: editor_lsp::Severity) -> char {
    use editor_lsp::Severity::*;
    match s {
        Error => 'E',
        Warning => 'W',
        Info => 'i',
        Hint => 'h',
    }
}
fn sev_rank(s: editor_lsp::Severity) -> u8 {
    use editor_lsp::Severity::*;
    match s {
        Error => 0,
        Warning => 1,
        Info => 2,
        Hint => 3,
    }
}

/// Build the `"lsp.diag"` decoration layer for `diags` against `doc`: an underline span per
/// diagnostic (char range, `lsp.diag.<sev>`) plus one gutter mark per line carrying that line's
/// highest-severity glyph (`lsp.diag.mark.<sev>`). Pure over `&Document`, so it's unit-testable.
pub(super) fn build_diag_decorations(
    doc: &Document,
    diags: &[editor_lsp::Diagnostic],
) -> DecorationSet {
    let mut spans = Vec::with_capacity(diags.len());
    let mut per_line: std::collections::BTreeMap<usize, editor_lsp::Severity> =
        std::collections::BTreeMap::new();
    for d in diags {
        let start = lsp_pos_to_char(doc, d.line, d.start_char16);
        let end = if d.end_line == d.line {
            lsp_pos_to_char(doc, d.line, d.end_char16)
        } else {
            let l = (d.line as usize).min(doc.len_lines().saturating_sub(1));
            doc.line_to_char(l) + doc.line_len_chars(l)
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
    DecorationSet { spans, gutter }
}

impl App {
    /// Republish the active document's diagnostics as the `"lsp.diag"` decoration layer, so the
    /// pure renderer draws them generically. Run each frame before draw (the diagnostics model +
    /// caret-message + navigation still live app-side; only the *rendering* is a decoration).
    pub(super) fn update_diagnostic_decorations(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let set = self.editor.workspace.documents.get(id).map(|doc| {
            let diags = self
                .editor
                .diagnostics
                .get(&id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            build_diag_decorations(doc, diags)
        });
        if let Some(set) = set {
            self.editor.set_decorations(id, "lsp.diag", set); // set_decorations clears on empty
        }
    }

    /// Jump the caret to the next (`dir > 0`) or previous diagnostic in the active document,
    /// wrapping around the ends (plan §2.2).
    pub(super) fn goto_diagnostic(&mut self, dir: isize) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(doc) = self.editor.workspace.documents.get(id) else {
            return;
        };
        let Some(diags) = self.editor.diagnostics.get(&id) else {
            return;
        };
        if diags.is_empty() {
            return;
        }
        let mut offs: Vec<usize> = diags
            .iter()
            .map(|d| lsp_pos_to_char(doc, d.line, d.start_char16))
            .collect();
        offs.sort_unstable();
        offs.dedup();
        let head = doc.selections.primary().head;
        let target = if dir > 0 {
            offs.iter().copied().find(|&o| o > head).unwrap_or(offs[0])
        } else {
            offs.iter()
                .rev()
                .copied()
                .find(|&o| o < head)
                .unwrap_or_else(|| *offs.last().unwrap())
        };
        if let Some(d) = self.editor.workspace.documents.get_mut(id) {
            d.set_caret(target);
        }
        self.ensure_cursor_visible();
        self.editor.update_bracket_match();
    }

    /// The diagnostic whose range covers the primary caret, for the status-line message
    /// (plan §2.2). Borrows `self`, so the pure renderer can display it directly.
    pub(crate) fn diagnostic_at_caret(&self) -> Option<(editor_lsp::Severity, &str)> {
        let id = self.editor.workspace.active_doc()?;
        let doc = self.editor.workspace.documents.get(id)?;
        let diags = self.editor.diagnostics.get(&id)?;
        let head = doc.selections.primary().head;
        diags.iter().find_map(|d| {
            let start = lsp_pos_to_char(doc, d.line, d.start_char16);
            let end = lsp_pos_to_char(doc, d.end_line, d.end_char16).max(start);
            (head >= start && head <= end).then_some((d.severity, d.message.as_str()))
        })
    }
}
