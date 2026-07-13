//! The terminal dock, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the dock **lifecycle** — which terminals exist (`order`), the active tab, and
//! the open/minimized state — driving it through the RawPTY Host port: [`Host::terminal_open`]
//! allocates a [`TerminalId`] (the app spawns the PTY), [`Host::terminal_close`] kills one, and
//! [`Host::set_terminal_view`] publishes the [`TerminalView`] the app renders. Focus goes through
//! [`Host::set_terminal_focus`]. Everything hard stays app-side: the PTY spawn, the vt100 parse,
//! the byte budgeting, the grid render, and key/mouse forwarding (all keyed by the ids here).
//!
//! Mouse header clicks arrive through [`Plugin::on_panel_activate`] (the app hit-tests the tab bar
//! and hands us `minimize` / `new` / `select:N` / `close:N`).

use editor_plugin::{Contributions, Host, Plugin, TerminalId, TerminalView};

#[derive(Default)]
pub struct TerminalPlugin {
    /// The tab order, left to right.
    order: Vec<TerminalId>,
    /// Index into `order` of the focused tab.
    active: usize,
    open: bool,
    minimized: bool,
}

impl TerminalPlugin {
    const ID: &'static str = "terminal";

    /// Publish the current lifecycle for the app to render.
    fn publish(&self, host: &mut dyn Host) {
        host.set_terminal_view(TerminalView {
            open: self.open,
            minimized: self.minimized,
            active: self.active,
            order: self.order.clone(),
        });
    }

    /// Spawn a shell, append it, and make it active. `false` if the spawn failed.
    fn spawn(&mut self, host: &mut dyn Host) -> bool {
        let cwd = host.root().to_path_buf();
        match host.terminal_open(&cwd) {
            Some(id) => {
                self.order.push(id);
                self.active = self.order.len() - 1;
                true
            }
            None => {
                host.notify("Failed to start terminal".to_string());
                false
            }
        }
    }

    /// Toggle: open + focus when closed or minimized, else close the dock.
    fn toggle(&mut self, host: &mut dyn Host) {
        if self.open && !self.minimized {
            self.open = false;
            host.set_terminal_focus(false);
        } else {
            self.focus(host);
        }
    }

    /// Open (spawning a first shell if empty), expand, and focus the dock.
    fn focus(&mut self, host: &mut dyn Host) {
        self.open = true;
        self.minimized = false;
        if self.order.is_empty() && !self.spawn(host) {
            self.open = false; // spawn failed — don't hold an empty dock open
            return;
        }
        host.set_terminal_focus(true);
    }

    /// Open a brand-new tab and focus the dock.
    fn new_terminal(&mut self, host: &mut dyn Host) {
        self.open = true;
        self.minimized = false;
        if self.spawn(host) {
            host.set_terminal_focus(true);
        }
    }

    /// Close the active tab; close the dock and return to the editor when it was the last.
    fn close(&mut self, host: &mut dyn Host) {
        if self.order.is_empty() {
            return;
        }
        let id = self.order.remove(self.active);
        host.terminal_close(id);
        if self.order.is_empty() {
            self.active = 0;
            self.open = false;
            host.set_terminal_focus(false);
        } else {
            self.active = self.active.min(self.order.len() - 1);
        }
    }

    /// Collapse to the header row (or restore).
    fn minimize(&mut self, host: &mut dyn Host) {
        if !self.open {
            return;
        }
        self.minimized = !self.minimized;
        host.set_terminal_focus(!self.minimized && !self.order.is_empty());
    }

    fn next(&mut self) {
        if self.open && !self.order.is_empty() {
            self.active = (self.active + 1) % self.order.len();
        }
    }

    fn prev(&mut self) {
        if self.open && !self.order.is_empty() {
            self.active = (self.active + self.order.len() - 1) % self.order.len();
        }
    }

    /// Select tab `i` (mouse), opening + focusing the dock.
    fn select(&mut self, i: usize, host: &mut dyn Host) {
        if i < self.order.len() {
            self.active = i;
            self.open = true;
            self.minimized = false;
            host.set_terminal_focus(true);
        }
    }
}

impl Plugin for TerminalPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("terminal.toggle", "Terminal: Toggle Panel")
            .command("terminal.new", "Terminal: New Terminal")
            .command("terminal.close", "Terminal: Close Active Terminal")
            .command("terminal.minimize", "Terminal: Minimize/Restore Panel")
            .command("terminal.next", "Terminal: Next Terminal")
            .command("terminal.prev", "Terminal: Previous Terminal")
            .keybinding("ctrl+j", "terminal.toggle")
            .keybinding("ctrl+`", "terminal.toggle")
            .keybinding("ctrl+pagedown", "terminal.next")
            .keybinding("ctrl+pageup", "terminal.prev")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "terminal.toggle" => self.toggle(host),
            "terminal.new" => self.new_terminal(host),
            "terminal.close" => self.close(host),
            "terminal.minimize" => self.minimize(host),
            "terminal.next" => self.next(),
            "terminal.prev" => self.prev(),
            _ => return false,
        }
        self.publish(host);
        true
    }

    fn on_panel_activate(&mut self, _panel_id: &str, payload: &str, host: &mut dyn Host) {
        if let Some(n) = payload.strip_prefix("select:").and_then(|s| s.parse().ok()) {
            self.select(n, host);
        } else if let Some(n) = payload
            .strip_prefix("close:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if n < self.order.len() {
                self.active = n;
                self.close(host);
            }
        } else if payload == "minimize" {
            self.minimize(host);
        } else if payload == "new" {
            self.new_terminal(host);
        } else {
            return;
        }
        self.publish(host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{DocId, Selections, Transaction, Workspace};
    use editor_plugin::PanelContent;
    use std::path::{Path, PathBuf};

    /// A mock host that fakes PTY spawning (each `terminal_open` succeeds with a fresh id, unless
    /// `fail_spawn`) and records the published view + focus, so the lifecycle state machine tests
    /// without a real shell.
    struct MockHost {
        ws: Workspace,
        next: u64,
        fail_spawn: bool,
        opened: Vec<TerminalId>,
        closed: Vec<TerminalId>,
        view: TerminalView,
        focused: bool,
        notes: Vec<String>,
    }

    impl MockHost {
        fn new() -> MockHost {
            MockHost {
                ws: Workspace::new(PathBuf::from(".")),
                next: 1,
                fail_spawn: false,
                opened: Vec::new(),
                closed: Vec::new(),
                view: TerminalView::default(),
                focused: false,
                notes: Vec::new(),
            }
        }
    }

    impl Host for MockHost {
        fn workspace(&self) -> &Workspace {
            &self.ws
        }
        fn apply_transaction(&mut self, _doc: DocId, _txn: Transaction) {}
        fn set_selections(&mut self, _doc: DocId, _sels: Selections) {}
        fn open_path(&mut self, _path: &Path) {}
        fn set_panel(&mut self, _panel_id: &str, _content: PanelContent) {}
        fn set_status(&mut self, _item_id: &str, _text: String) {}
        fn notify(&mut self, message: String) {
            self.notes.push(message);
        }
        fn terminal_open(&mut self, _cwd: &Path) -> Option<TerminalId> {
            if self.fail_spawn {
                return None;
            }
            let id = TerminalId(self.next);
            self.next += 1;
            self.opened.push(id);
            Some(id)
        }
        fn terminal_close(&mut self, id: TerminalId) {
            self.closed.push(id);
        }
        fn set_terminal_view(&mut self, view: TerminalView) {
            self.view = view;
        }
        fn set_terminal_focus(&mut self, focused: bool) {
            self.focused = focused;
        }
        fn execute(&mut self, _command_id: &str) {}
    }

    fn cmd(plugin: &mut TerminalPlugin, host: &mut MockHost, id: &str) {
        assert!(plugin.run_command(id, host));
    }

    #[test]
    fn toggle_spawns_focuses_then_closes() {
        let mut p = TerminalPlugin::default();
        let mut h = MockHost::new();
        cmd(&mut p, &mut h, "terminal.toggle");
        assert_eq!(h.opened.len(), 1, "first toggle spawns a shell");
        assert!(h.view.open && !h.view.minimized && h.focused);
        assert_eq!(h.view.order.len(), 1);
        // Toggling again closes the dock (keeps the shell) and returns focus to the editor.
        cmd(&mut p, &mut h, "terminal.toggle");
        assert!(!h.view.open && !h.focused);
        assert!(h.closed.is_empty(), "toggle-closed keeps the shell alive");
        assert_eq!(h.view.order.len(), 1);
    }

    #[test]
    fn failed_spawn_leaves_the_dock_closed() {
        let mut p = TerminalPlugin::default();
        let mut h = MockHost::new();
        h.fail_spawn = true;
        cmd(&mut p, &mut h, "terminal.toggle");
        assert!(
            !h.view.open,
            "a failed spawn must not hold an empty dock open"
        );
        assert!(h.view.order.is_empty());
        assert_eq!(h.notes, vec!["Failed to start terminal".to_string()]);
    }

    #[test]
    fn new_next_prev_cycle_the_active_tab() {
        let mut p = TerminalPlugin::default();
        let mut h = MockHost::new();
        cmd(&mut p, &mut h, "terminal.new"); // tab 0
        cmd(&mut p, &mut h, "terminal.new"); // tab 1
        cmd(&mut p, &mut h, "terminal.new"); // tab 2 (active)
        assert_eq!(h.view.order.len(), 3);
        assert_eq!(h.view.active, 2);
        cmd(&mut p, &mut h, "terminal.next"); // wraps to 0
        assert_eq!(h.view.active, 0);
        cmd(&mut p, &mut h, "terminal.prev"); // wraps to 2
        assert_eq!(h.view.active, 2);
    }

    #[test]
    fn close_removes_the_active_tab_and_closes_the_last() {
        let mut p = TerminalPlugin::default();
        let mut h = MockHost::new();
        cmd(&mut p, &mut h, "terminal.new"); // id 1
        cmd(&mut p, &mut h, "terminal.new"); // id 2 (active)
        cmd(&mut p, &mut h, "terminal.close"); // removes id 2
        assert_eq!(h.closed, vec![TerminalId(2)]);
        assert_eq!(h.view.order, vec![TerminalId(1)]);
        assert_eq!(h.view.active, 0);
        assert!(h.view.open);
        // Closing the last returns focus to the editor and closes the dock.
        cmd(&mut p, &mut h, "terminal.close");
        assert_eq!(h.closed, vec![TerminalId(2), TerminalId(1)]);
        assert!(!h.view.open && !h.focused);
    }

    #[test]
    fn minimize_toggles_and_drops_focus() {
        let mut p = TerminalPlugin::default();
        let mut h = MockHost::new();
        cmd(&mut p, &mut h, "terminal.toggle"); // open + focused
        cmd(&mut p, &mut h, "terminal.minimize");
        assert!(h.view.minimized && !h.focused);
        cmd(&mut p, &mut h, "terminal.minimize");
        assert!(!h.view.minimized && h.focused);
    }

    #[test]
    fn mouse_header_selects_and_closes_tabs() {
        let mut p = TerminalPlugin::default();
        let mut h = MockHost::new();
        cmd(&mut p, &mut h, "terminal.new"); // 0
        cmd(&mut p, &mut h, "terminal.new"); // 1 (active)
        p.on_panel_activate("terminal", "select:0", &mut h);
        assert_eq!(h.view.active, 0);
        assert!(h.focused);
        p.on_panel_activate("terminal", "close:1", &mut h); // close the non-active tab
        assert_eq!(h.closed, vec![TerminalId(2)]);
        assert_eq!(h.view.order, vec![TerminalId(1)]);
        p.on_panel_activate("terminal", "new", &mut h);
        assert_eq!(h.view.order.len(), 2);
        p.on_panel_activate("terminal", "minimize", &mut h);
        assert!(h.view.minimized);
    }
}
