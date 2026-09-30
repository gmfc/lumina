//! Breadcrumb strip: enclosing LSP document symbols for the caret.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use editor_lsp::DocumentSymbol;

use crate::app::App;

/// Symbols whose range encloses `(line, col)`, ordered shallow → deep.
pub(crate) fn enclosing_symbols(
    syms: &[DocumentSymbol],
    line: u32,
    col: u32,
) -> Vec<&DocumentSymbol> {
    let at = (line, col);
    let mut enclosing: Vec<&DocumentSymbol> = syms
        .iter()
        .filter(|s| {
            let start = (s.line, s.character);
            let end = (s.end_line, s.end_character);
            start <= at && at <= end
        })
        .collect();
    if enclosing.is_empty() {
        // Fallback when servers omit end ranges: walk by depth using start positions.
        let before: Vec<&DocumentSymbol> = syms
            .iter()
            .filter(|s| (s.line, s.character) <= at)
            .collect();
        let max_d = before.iter().map(|s| s.depth).max().unwrap_or(0);
        for d in 0..=max_d {
            if let Some(s) = before.iter().rev().find(|s| s.depth == d) {
                enclosing.push(*s);
            }
        }
        return enclosing;
    }
    enclosing.sort_by_key(|s| s.depth);
    // One symbol per depth (deepest wins collisions).
    let mut chain = Vec::new();
    let mut last_d = None;
    for s in enclosing {
        if last_d == Some(s.depth) {
            if let Some(prev) = chain.last_mut() {
                *prev = s;
            }
        } else {
            chain.push(s);
            last_d = Some(s.depth);
        }
    }
    chain
}

/// Draw a 1-row breadcrumb under the tab bar. Returns the area used and records click hits on
/// `app.editor.breadcrumb_hits`.
pub(super) fn render_breadcrumb(f: &mut Frame, app: &mut App, area: Rect) -> bool {
    app.editor.breadcrumb_hits.clear();
    let Some(id) = app.editor.workspace.active_doc() else {
        return false;
    };
    let Some(syms) = app.editor.doc_symbols.get(&id) else {
        return false;
    };
    if syms.is_empty() {
        return false;
    }
    let (line, col) = app
        .editor
        .workspace
        .documents
        .get(id)
        .map(|d| {
            let head = d.selections.primary().head;
            let (l, c) = d.char_to_line_col(head);
            (l as u32, c as u32)
        })
        .unwrap_or((0, 0));
    let chain = enclosing_symbols(syms, line, col);
    if chain.is_empty() {
        return false;
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;
    let sep_style = Style::default().fg(Color::DarkGray);
    let name_style = Style::default()
        .fg(Color::Rgb(180, 190, 210))
        .add_modifier(Modifier::BOLD);
    for (i, sym) in chain.iter().enumerate() {
        if i > 0 {
            let sep = " › ";
            spans.push(Span::styled(sep, sep_style));
            x = x.saturating_add(sep.chars().count() as u16);
        }
        let label = format!(" {} ", sym.name);
        let w = label.chars().count() as u16;
        let hit = Rect {
            x,
            y: area.y,
            width: w.min(area.width.saturating_sub(x.saturating_sub(area.x))),
            height: 1,
        };
        app.editor
            .breadcrumb_hits
            .push((hit, sym.line, sym.character));
        spans.push(Span::styled(label, name_style));
        x = x.saturating_add(w);
        if x >= area.x + area.width {
            break;
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(28, 30, 36))),
        area,
    );
    true
}
