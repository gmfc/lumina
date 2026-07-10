//! Terminal-dock commands, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the terminal *commands* (toggle / new / close / minimize / next / prev),
//! expressing each as a [`TerminalOp`] through [`Host::terminal_op`]. Everything hard stays
//! app-side: the PTY spawn, the vt100 parse, the byte budgeting, the grid render, focus, and
//! key forwarding to the shell.

use editor_plugin::{Contributions, Host, Plugin, TerminalOp};

pub struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn id(&self) -> &str {
        "terminal"
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
        let op = match command_id {
            "terminal.toggle" => TerminalOp::Toggle,
            "terminal.new" => TerminalOp::New,
            "terminal.close" => TerminalOp::Close,
            "terminal.minimize" => TerminalOp::Minimize,
            "terminal.next" => TerminalOp::Next,
            "terminal.prev" => TerminalOp::Prev,
            _ => return false,
        };
        host.terminal_op(op);
        true
    }
}
