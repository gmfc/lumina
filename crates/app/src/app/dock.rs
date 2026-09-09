//! The shared bottom dock: a tab strip over the terminal and LSP panels. The terminal tab's
//! lifecycle is owned by the `terminal` plugin (see [`crate::editor::host`]); the LSP tab is
//! app-owned. These helpers arbitrate which tab is shown and drive the toggle/switch/minimize
//! actions. Part of the [`crate::app`] module.

use super::*;
use crate::editor::{DockTab, Focus};

impl App {
    /// Every contributed sidebar panel, in contribution order.
    ///
    /// This is what makes `PanelLocation` mean something. The sidebar used to draw the literal id
    /// `"explorer.tree"`, so a plugin could declare a `Sidebar` panel, publish content to it, and
    /// have nothing appear — the seam existed on the producer side and stopped dead on the
    /// consumer side (invariant #3: no privileged back doors).
    pub(crate) fn sidebar_panels(&self) -> Vec<&editor_plugin::PanelSpec> {
        self.registry
            .panels()
            .iter()
            .filter(|p| p.location == editor_plugin::PanelLocation::Sidebar)
            .collect()
    }

    /// The sidebar panel to draw: the chosen one while it is still contributed, else the first.
    pub(crate) fn active_sidebar_panel(&self) -> Option<&editor_plugin::PanelSpec> {
        let panels = self.registry.panels();
        self.editor
            .sidebar_panel
            .as_deref()
            .and_then(|id| {
                panels
                    .iter()
                    .find(|p| p.id == id && p.location == editor_plugin::PanelLocation::Sidebar)
            })
            .or_else(|| self.sidebar_panels().into_iter().next())
    }

    /// The active sidebar panel's id, owned so callers can use it while borrowing `self` mutably
    /// (drawing hit-tests it, the mouse router dispatches to it).
    pub(crate) fn active_sidebar_panel_id(&self) -> Option<String> {
        self.active_sidebar_panel().map(|p| p.id.clone())
    }

    /// `view.nextSidebarPanel`: cycle the sidebar through the contributed sidebar panels, asking
    /// the newly-shown one to render. A no-op with fewer than two — there is nothing to cycle.
    pub(crate) fn cycle_sidebar_panel(&mut self) {
        let ids: Vec<String> = self.sidebar_panels().iter().map(|p| p.id.clone()).collect();
        if ids.len() < 2 {
            return;
        }
        let current = self.active_sidebar_panel_id();
        let next = current
            .and_then(|id| ids.iter().position(|c| *c == id))
            .map(|i| (i + 1) % ids.len())
            .unwrap_or(0);
        self.editor.sidebar_panel = Some(ids[next].clone());
        self.refresh_sidebar_panel();
        self.editor.focus = Focus::Sidebar;
    }

    /// Bring the sidebar's scroll offset in line with the panel it is showing.
    ///
    /// Two rules, and the order matters. The offset follows the *selection* only when the
    /// selection actually moved — otherwise a wheel scroll away from the selected row would be
    /// yanked straight back on the next frame, which is what "scrolling is broken" feels like.
    /// It is then clamped to the end, so a short tail fills the pane instead of leaving blanks.
    pub(crate) fn reconcile_sidebar_scroll(
        &mut self,
        rows: usize,
        selected: usize,
        visible: usize,
    ) {
        if visible == 0 {
            return;
        }
        if selected != self.editor.sidebar_last_selected {
            self.editor.sidebar_last_selected = selected;
            if selected < self.editor.sidebar_scroll {
                self.editor.sidebar_scroll = selected;
            } else if selected >= self.editor.sidebar_scroll + visible {
                self.editor.sidebar_scroll = selected + 1 - visible;
            }
        }
        self.editor.sidebar_scroll = self.editor.sidebar_scroll.min(rows.saturating_sub(visible));
    }

    /// Ask the owning plugin to (re)render the active sidebar panel.
    ///
    /// Most plugins push content when their own state changes, but `Plugin::render_panel` is the
    /// pull half of the same contract and had no production caller at all — so a panel that only
    /// renders on demand could never draw. Called when the sidebar switches panels.
    pub(crate) fn refresh_sidebar_panel(&mut self) {
        let Some(id) = self.active_sidebar_panel_id() else {
            return;
        };
        self.registry.render_panel(&id, &mut self.editor);
        // Anything the plugin queued (events, commands, opens) is picked up by the next
        // `drain_workers` tick, like every other plugin intent.
    }

    /// The contributed `PanelLocation::Bottom` panel with content to show, if any.
    ///
    /// Same story as the sidebar: the results dock drew the literal id `"search.results"`, so a
    /// contributed bottom panel (a problems list, build output) had nowhere to appear.
    pub(crate) fn active_bottom_panel(&self) -> Option<&editor_plugin::PanelSpec> {
        self.registry.panels().iter().find(|p| {
            p.location == editor_plugin::PanelLocation::Bottom
                && self
                    .editor
                    .panels
                    .get(&p.id)
                    .is_some_and(|c| !c.lines.is_empty())
        })
    }

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
        // Opening the dock is a visible layout change; `update_lsp` may reach here on a tick the
        // idle-frame gate did not schedule a draw for, so force the repaint (idempotent per lang).
        self.force_redraw = true;
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
