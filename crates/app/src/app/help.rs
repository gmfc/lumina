//! In-app reference tabs: the keyboard-shortcut sheet and the notification scrollback.
//!
//! Both are generated from live state rather than written down, which is the point. The shortcut
//! sheet reads the same [`crate::keymap::Keymap`] the keys themselves resolve through, so it picks
//! up plugin-contributed chords and user `[keys]` overrides for free and can never drift from what
//! pressing a key actually does. The notification log reads
//! [`crate::editor::EditorState::notice_log`], so a message that scrolled out of the one-line
//! status bar is still retrievable instead of being gone for good.
//!
//! Both render through the tab-view mechanism ([`crate::editor::TabView::Text`]), inheriting
//! scrolling, tab management, and every "never write this buffer to disk" guard unchanged.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use editor_plugin::{PanelLine, Span, ViewerContent};

use super::App;
use crate::editor::{NoticeLevel, TextTabKind};

/// Command-id prefixes grouped into sections, in the order they are shown. The catch-all bucket
/// at the end collects anything a plugin contributes under a prefix this list doesn't know, so a
/// new plugin's chords appear in the reference without this table being touched.
const SECTIONS: &[(&str, &[&str])] = &[
    ("Files & tabs", &["file.", "tab."]),
    ("Editing", &["edit."]),
    ("Moving & selecting", &["cursor.", "select."]),
    ("Search", &["search."]),
    ("Language & diagnostics", &["lsp.", "git."]),
    ("View & panels", &["view.", "terminal.", "config."]),
    ("Help", &["help."]),
    ("Application", &["app."]),
];

/// Bindings whose chord deliberately differs from VS Code's, with the reason. The keymap folds
/// Shift into the character for letter keys (see [`crate::keymap`]), so `ctrl+shift+<letter>` is
/// indistinguishable from `ctrl+<letter>` and would silently clobber it. The reasoning is
/// documented at the point of definition in `commands/tables.rs`; without this section the user
/// discovers it by pressing the VS Code chord and watching the wrong thing happen.
const DEVIATIONS: &[(&str, &str, &str)] = &[
    ("file.saveAs", "Ctrl+Shift+S", "Shift folds into the letter"),
    (
        "edit.deleteLines",
        "Ctrl+Shift+K",
        "Shift folds into the letter",
    ),
    (
        "lsp.panel.toggle",
        "Ctrl+Shift+L",
        "Shift folds into the letter",
    ),
    (
        "cursor.selectAllMatches",
        "Ctrl+Shift+L",
        "Shift folds into the letter",
    ),
];

impl App {
    /// `help.keybindings`: open (or refocus) the keyboard-shortcut reference.
    pub(super) fn open_keybindings_help(&mut self) {
        let content = self.keybindings_content();
        self.editor
            .open_text_view_tab(TextTabKind::Keybindings, "Keyboard Shortcuts", content);
    }

    /// `view.notifications`: open (or refocus) the notice scrollback.
    pub(super) fn open_notification_log(&mut self) {
        let content = self.notifications_content();
        self.editor
            .open_text_view_tab(TextTabKind::Notifications, "Notifications", content);
    }

    /// Keep an open notification tab current as new notices arrive, so it is a live log rather
    /// than a snapshot of whenever it happened to be opened. No-op when the tab isn't open.
    pub(super) fn refresh_notification_log(&mut self) {
        let open = self
            .editor
            .tab_views
            .values()
            .any(|v| matches!(v, crate::editor::TabView::Text(t) if t.kind == TextTabKind::Notifications));
        if !open {
            return;
        }
        let stale = self.editor.tab_views.values().any(|v| match v {
            crate::editor::TabView::Text(t) if t.kind == TextTabKind::Notifications => {
                t.content.lines.len() != self.notification_row_count()
            }
            _ => false,
        });
        if !stale {
            return;
        }
        let content = self.notifications_content();
        for view in self.editor.tab_views.values_mut() {
            if let crate::editor::TabView::Text(t) = view {
                if t.kind == TextTabKind::Notifications {
                    t.content = content.clone();
                }
            }
        }
    }

    /// How many body rows the notification tab would have right now — the cheap staleness check
    /// that keeps the refresh from rebuilding the whole log every tick.
    fn notification_row_count(&self) -> usize {
        if self.editor.notice_log.is_empty() {
            2
        } else {
            self.editor.notice_log.len() + 1
        }
    }

    /// The title registered for a command id, for labelling a binding. Falls back to the id: a
    /// chord bound to something with no palette entry is still worth showing, and showing the raw
    /// id is more useful than hiding the row.
    fn command_title(&self, id: &str) -> String {
        self.editor
            .command_catalog
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// Build the shortcut sheet from the live keymap.
    fn keybindings_content(&self) -> ViewerContent {
        let mut rows: Vec<(String, String, String)> = self
            .keymap
            .entries()
            .map(|(chord, id)| (chord, id.to_string(), self.command_title(id)))
            .collect();
        // Stable, readable order inside each section: by the command's shown title.
        rows.sort_by_key(|r| r.2.to_lowercase());
        let width = rows
            .iter()
            .map(|(c, _, _)| c.chars().count())
            .max()
            .unwrap_or(0);

        let mut lines: Vec<PanelLine> = Vec::new();

        // The surprises go first: these are the rows a VS Code user will otherwise discover by
        // pressing the chord they know and watching something else happen.
        let deviations: Vec<&(&str, &str, &str)> = DEVIATIONS
            .iter()
            .filter(|(id, _, _)| self.keymap.binding_label(id).is_some())
            .collect();
        if !deviations.is_empty() {
            push_heading(&mut lines, "Differs from VS Code");
            for (id, vscode, why) in deviations {
                let here = self.keymap.binding_label(id).unwrap_or_default();
                lines.push(PanelLine::new(vec![
                    Span::new(format!("  {here:width$}  "), "match"),
                    Span::new(self.command_title(id), "file"),
                    Span::new(format!("  (VS Code: {vscode} — {why})"), "dim"),
                ]));
            }
        }

        let mut used = vec![false; rows.len()];
        for (heading, prefixes) in SECTIONS {
            let section: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter(|(i, (_, id, _))| !used[*i] && prefixes.iter().any(|p| id.starts_with(p)))
                .map(|(i, _)| i)
                .collect();
            if section.is_empty() {
                continue;
            }
            for i in &section {
                used[*i] = true;
            }
            push_heading(&mut lines, heading);
            for i in section {
                push_binding(&mut lines, &rows[i].0, &rows[i].2, width);
            }
        }
        // Anything a plugin contributed under an unknown prefix still gets shown.
        let rest: Vec<usize> = (0..rows.len()).filter(|i| !used[*i]).collect();
        if !rest.is_empty() {
            push_heading(&mut lines, "Other");
            for i in rest {
                push_binding(&mut lines, &rows[i].0, &rows[i].2, width);
            }
        }

        lines.push(PanelLine::new(vec![Span::plain("")]));
        lines.push(PanelLine::new(vec![Span::new(
            format!(
                "  Remap anything under [keys] in your config.toml. \
                 Commands without a chord live in the command palette ({}).",
                self.chord_for("view.commandPalette", "Ctrl+Shift+P")
            ),
            "dim",
        )]));

        ViewerContent {
            status: Some(format!(
                "{} shortcuts, read from the keymap in use — including your overrides.",
                rows.len()
            )),
            lines,
        }
    }

    /// Build the notification scrollback, newest first so the most recent message needs no
    /// scrolling to reach.
    fn notifications_content(&self) -> ViewerContent {
        let mut lines: Vec<PanelLine> = Vec::new();
        if self.editor.notice_log.is_empty() {
            lines.push(PanelLine::new(vec![Span::plain("")]));
            lines.push(PanelLine::new(vec![Span::new(
                "  Nothing to show yet — messages the editor sends you are kept here.",
                "dim",
            )]));
        } else {
            lines.push(PanelLine::new(vec![Span::plain("")]));
            for notice in self.editor.notice_log.iter().rev() {
                let (glyph, style) = match notice.level {
                    NoticeLevel::Info => ("·", "dim"),
                    NoticeLevel::Warn => ("⚠", "match"),
                    NoticeLevel::Error => ("✗", "match"),
                };
                lines.push(PanelLine::new(vec![
                    Span::new(format!("  {glyph} "), style),
                    Span::new(notice.text.clone(), "file"),
                ]));
            }
        }
        ViewerContent {
            status: Some(format!(
                "The last {} messages, newest first (up to {} are kept).",
                self.editor.notice_log.len(),
                crate::editor::NOTICE_LOG_CAP
            )),
            lines,
        }
    }
}

/// A blank spacer plus a section heading.
fn push_heading(lines: &mut Vec<PanelLine>, heading: &str) {
    lines.push(PanelLine::new(vec![Span::plain("")]));
    lines.push(PanelLine::new(vec![Span::new(
        format!("  {heading}"),
        "dir",
    )]));
}

/// One `chord  Command Title` row, with the chord column padded to a shared width.
fn push_binding(lines: &mut Vec<PanelLine>, chord: &str, title: &str, width: usize) {
    let pad = width.saturating_sub(chord.chars().count());
    lines.push(PanelLine::new(vec![
        Span::new(format!("  {chord}{} ", " ".repeat(pad)), "match"),
        Span::new(format!(" {title}"), "file"),
    ]));
}
