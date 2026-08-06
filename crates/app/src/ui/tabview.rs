//! Renders the two kinds of non-text tab: the refusal **notice** and a plugin **viewer**.
//!
//! Both are pure functions of state (invariant #8) — a viewer publishes its rows through
//! `Host::set_viewer_content` and this only reads them, exactly as the sidebar reads a panel's.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::tabview::{NOTICE_ACTIONS, VIEWER_ACTIONS};
use crate::app::App;
use crate::editor::TabView;
use crate::files;

use super::sidebar::style_for;
use super::util::{display_len, CLR_ACCENT};

/// Rows a viewer spends on chrome before its body: the title and the status/separator row.
const VIEWER_HEADER_ROWS: u16 = 2;

/// How many body rows a viewer tab actually draws in a pane `height` rows tall — **0** for a
/// pane too short for a body. Shared with the input side so scroll clamping matches what is
/// drawn; a `.max(1)` here would let paging walk `scroll` through rows the user can never see.
pub(crate) fn viewer_body_rows(height: u16) -> usize {
    height.saturating_sub(VIEWER_HEADER_ROWS) as usize
}

pub(super) fn render_tab_view(f: &mut Frame, app: &App, area: Rect, view: &TabView) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match view {
        TabView::Notice { path, refusal } => render_notice(f, app, area, path, *refusal),
        TabView::Viewer(v) => render_body(
            f,
            area,
            &v.title,
            Some(&v.path),
            &v.content,
            v.scroll,
            &hints(app, VIEWER_ACTIONS),
        ),
        // An app-generated reference tab: the same header + scrolling body as a viewer, with no
        // file behind it and so no file-level escape hatches to offer.
        TabView::Text(t) => render_body(f, area, &t.title, None, &t.content, t.scroll, &[]),
    }
}

/// The chords for a tab's escape-hatch actions, looked up live so a remap or a disabled plugin is
/// reflected rather than advertised wrongly.
fn hints(app: &App, actions: &[(&str, &str)]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|(id, label)| Some(format!("{}  {label}", app.keymap.binding_label(id)?)))
        .collect()
}

/// The "lumina can't show this as text" screen: what the file is, how big, why it was refused,
/// and what the user can do about it.
fn render_notice(
    f: &mut Frame,
    app: &App,
    area: Rect,
    path: &std::path::Path,
    refusal: files::Refusal,
) {
    let accent = Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let mut rows: Vec<Line> = vec![
        Line::from(TSpan::styled(refusal.label().to_string(), accent)),
        Line::from(""),
        Line::from(TSpan::styled(
            format!("{name} · {}", files::human_bytes(refusal.len())),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(TSpan::styled(reason(&refusal), dim)),
        Line::from(""),
    ];

    // Offer only actions that exist: `view.openAsHex` disappears with its plugin, and
    // `file.openAnyway` is meaningless for a binary that can't round-trip through the rope.
    let actions: Vec<(String, &str)> = NOTICE_ACTIONS
        .iter()
        .filter(|(id, _)| *id != "file.openAnyway" || refusal.is_overridable())
        .filter_map(|(id, label)| Some((app.keymap.binding_label(id)?, *label)))
        .collect();
    if !actions.is_empty() {
        let key_col = actions
            .iter()
            .map(|(k, _)| display_len(k))
            .max()
            .unwrap_or(0);
        for (keys, label) in &actions {
            let pad = " ".repeat(key_col - display_len(keys));
            rows.push(Line::from(vec![
                TSpan::styled(keys.clone(), Style::default().fg(CLR_ACCENT)),
                TSpan::raw(format!("{pad}  ")),
                TSpan::styled((*label).to_string(), Style::default().fg(Color::Gray)),
            ]));
        }
    }

    // Vertically centered, like the welcome screen.
    let top = area.height.saturating_sub(rows.len() as u16) / 2;
    let body = Rect::new(
        area.x,
        area.y + top,
        area.width,
        area.height.saturating_sub(top),
    );
    f.render_widget(
        Paragraph::new(rows).alignment(ratatui::layout::Alignment::Center),
        body,
    );
}

/// One sentence explaining the refusal, in the user's terms.
fn reason(refusal: &files::Refusal) -> String {
    match refusal {
        files::Refusal::Binary { .. } => {
            "lumina doesn't display this file as text — its bytes aren't text, and editing them \
             here would corrupt the file."
                .into()
        }
        files::Refusal::TooLarge { limit, .. } => format!(
            "Larger than the {} open limit (settings: max_file_size_mb).",
            files::human_bytes(*limit)
        ),
    }
}

/// A scrolling published-rows tab: a title row, an optional status row, then the rows themselves.
/// Shared by plugin viewers and the app's own reference tabs — they differ only in whether there
/// is a file behind the title and what escape hatches apply.
///
/// Drawn with `Paragraph` rather than direct cell writes. That is not a style preference: the
/// cell writer advances one buffer cell per `char` while any width-aware caller advances by
/// *display* columns, and the two disagree on every CJK or emoji character — which a PDF or a
/// CSV will contain. `Paragraph` owns the width accounting, which also removes the manual `u16`
/// column arithmetic over plugin-supplied text (a span wide enough to overflow `u16` would
/// otherwise panic the editor in a debug build).
fn render_body(
    f: &mut Frame,
    area: Rect,
    title: &str,
    path: Option<&std::path::Path>,
    content: &editor_plugin::ViewerContent,
    scroll: usize,
    hints: &[String],
) {
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rows = viewer_body_rows(area.height);
    let total = content.lines.len();
    // Clamp against the *last page*, not the last row, and do it here as well as on the input
    // side: growing the pane (a resize, closing the dock) leaves a stale `scroll` that would
    // otherwise render a mostly blank tab until the next keystroke.
    let scroll = scroll.min(total.saturating_sub(rows));

    // Header: "<Title> — <file name>", the escape-hatch actions, and a right-aligned scroll
    // position when the body overflows. Showing `file.openAsText` here is what keeps a viewer
    // from taking a text extension hostage — the user can always get to the buffer.
    let mut left = if name.is_empty() {
        title.to_string()
    } else {
        format!("{title} — {name}")
    };
    if !hints.is_empty() {
        left.push_str(&format!("   ·   {}", hints.join("   ·   ")));
    }
    let position = (rows > 0 && total > rows)
        .then(|| format!("{}–{} of {total}", scroll + 1, (scroll + rows).min(total)));
    let mut header = vec![TSpan::styled(
        left.clone(),
        Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
    )];
    if let Some(position) = position {
        let used = display_len(&left) + display_len(&position);
        let gap = (area.width as usize).saturating_sub(used).max(1);
        header.push(TSpan::raw(" ".repeat(gap)));
        header.push(TSpan::styled(
            position,
            Style::default().fg(Color::DarkGray),
        ));
    }

    let mut lines = vec![Line::from(header)];
    // The status row only exists when the pane has a second row to put it in.
    if area.height > 1 {
        lines.push(Line::from(TSpan::styled(
            content.status.clone().unwrap_or_default(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    for line in content.lines.iter().skip(scroll).take(rows) {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        // Indent depth comes from a plugin; cap it so a nonsense value costs a wide indent
        // rather than an overflowed column.
        let indent = line.depth.min(64) * 2;
        if indent > 0 {
            spans.push(TSpan::raw(" ".repeat(indent)));
        }
        for span in &line.spans {
            spans.push(TSpan::styled(span.text.clone(), style_for(&span.style)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}
