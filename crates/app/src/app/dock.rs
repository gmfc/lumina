//! The shared bottom dock: a tab strip over the terminal and LSP panels. The terminal tab's
//! lifecycle is owned by the `terminal` plugin (see [`crate::editor::host`]); the LSP tab is
//! app-owned. These helpers arbitrate which tab is shown and drive the toggle/switch/minimize
//! actions. Part of the [`crate::app`] module.

use super::*;
use crate::editor::{DockTab, Focus};

impl App {
    /// The dock tab currently displayed, clamped to an *open* tab (so a stale `dock_active` never
    /// shows an empty region). `None` = the dock is hidden.
    pub(crate) fn dock_active_tab(&self) -> Option<DockTab> {
        let terminal = self.editor.terminal_view.open;
        let lsp = self.editor.lsp_open;
        match self.editor.dock_active {
            DockTab::Terminal if terminal => Some(DockTab::Terminal),
            DockTab::Lsp if lsp => Some(DockTab::Lsp),
            // Active tab isn't open — fall back to whichever tab is.
            _ if terminal => Some(DockTab::Terminal),
            _ if lsp => Some(DockTab::Lsp),
            _ => None,
        }
    }

    /// Whether the visible dock tab is collapsed to its header row.
    pub(crate) fn dock_minimized(&self) -> bool {
        match self.dock_active_tab() {
            Some(DockTab::Terminal) => self.editor.terminal_view.minimized,
            Some(DockTab::Lsp) => self.editor.lsp_panel.minimized,
            None => false,
        }
    }

    /// Toggle the LSP panel (the `lsp.panel.toggle` command / Ctrl+K Ctrl+L / footer click): show +
    /// focus it, or close it when it is already the visible, expanded tab.
    pub(super) fn toggle_lsp_panel(&mut self) {
        let showing =
            self.dock_active_tab() == Some(DockTab::Lsp) && !self.editor.lsp_panel.minimized;
        if showing {
            self.editor.lsp_open = false;
            self.editor.dock_active = DockTab::Terminal;
            self.editor.focus = Focus::Editor;
        } else {
            self.focus_dock_tab(DockTab::Lsp);
        }
    }

    /// Switch the dock to `tab`, opening it if needed and focusing it.
    pub(super) fn focus_dock_tab(&mut self, tab: DockTab) {
        match tab {
            DockTab::Lsp => {
                self.editor.lsp_open = true;
                self.editor.lsp_panel.minimized = false;
                self.editor.dock_active = DockTab::Lsp;
                self.editor.focus = Focus::LspPanel;
            }
            DockTab::Terminal => {
                self.editor.dock_active = DockTab::Terminal;
                if self.editor.terminal_view.open {
                    self.editor.focus = Focus::Panel;
                } else {
                    // No shell yet — let the terminal plugin spawn + open one (it will set focus).
                    self.exec_id("terminal.toggle");
                }
            }
        }
    }

    /// Auto-open the LSP panel — once per language — the first time the active file's language has a
    /// known-but-uninstalled server, surfacing the install command. The panel becomes visible on the
    /// LSP tab, but keyboard focus stays on the editor so it never disrupts typing.
    pub(super) fn maybe_auto_open_lsp(&mut self) {
        let Some(lang) = self
            .editor
            .workspace
            .active_doc()
            .and_then(|id| self.editor.workspace.documents.get(id))
            .and_then(|d| d.language.clone())
        else {
            return;
        };
        if self.lsp_autoopened.contains(&lang) || !self.lsp.server_missing(&lang) {
            return;
        }
        self.lsp_autoopened.insert(lang);
        self.editor.lsp_open = true;
        self.editor.lsp_panel.minimized = false;
        self.editor.dock_active = DockTab::Lsp;
    }

    /// Minimize / restore the visible dock tab (header chevron).
    pub(super) fn dock_minimize_active(&mut self) {
        match self.dock_active_tab() {
            Some(DockTab::Terminal) => self.exec_id("terminal.minimize"),
            Some(DockTab::Lsp) => {
                self.editor.lsp_panel.minimized = !self.editor.lsp_panel.minimized;
                self.editor.focus = if self.editor.lsp_panel.minimized {
                    Focus::Editor
                } else {
                    Focus::LspPanel
                };
            }
            None => {}
        }
    }

    /// Scroll the LSP panel's status list (mouse wheel / keys), clamped to `[0, rows-1]` so
    /// scrolling past the last row can't blank the panel.
    pub(super) fn scroll_lsp_panel(&mut self, delta: isize) {
        let max = self.lsp_status_rows().len().saturating_sub(1) as isize;
        let cur = self.editor.lsp_panel.scroll as isize;
        self.editor.lsp_panel.scroll = cur.saturating_add(delta).clamp(0, max) as u16;
    }

    /// The per-language status rows the LSP panel renders (accessor over the private manager, which
    /// the `crate::ui` renderer cannot reach directly).
    pub(crate) fn lsp_status_rows(&self) -> Vec<crate::lsp::LangStatus> {
        self.lsp.status_rows()
    }

    /// The most recent server log lines for the LSP panel's log tail (accessor over the private
    /// manager).
    pub(crate) fn lsp_recent_logs(&self, limit: usize) -> Vec<String> {
        self.lsp.recent_logs(limit)
    }
}
