//! The `:` ex command line: line jumps, write/quit, `:noh`, and `:s///` substitution.

use super::VimPlugin;
use editor_core::vim as core_vim;
use editor_plugin::input::{Key, KeyCode};
use editor_plugin::Host;

/// True when `cmd` looks like a substitute command (`s/…` or `%s/…`).
fn is_substitute(cmd: &str) -> bool {
    cmd.starts_with("s/") || cmd.starts_with("%s/")
}

impl VimPlugin {
    pub(super) fn open_command(&mut self) {
        self.sm().command = Some(String::new());
        self.sm().recording = None;
    }

    pub(super) fn command_key(&mut self, key: Key, host: &mut dyn Host) {
        match key.code {
            KeyCode::Esc => self.sm().command = None,
            KeyCode::Enter => {
                let cmd = self.sm().command.take().unwrap_or_default();
                self.run_ex(&cmd, host);
            }
            KeyCode::Backspace => {
                let empty = {
                    let buf = self.sm().command.as_mut().unwrap();
                    buf.pop();
                    buf.is_empty()
                };
                if empty {
                    self.sm().command = None;
                }
            }
            KeyCode::Char(c) if !key.ctrl => {
                if let Some(buf) = self.sm().command.as_mut() {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn run_ex(&mut self, cmd: &str, host: &mut dyn Host) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        if let Ok(n) = cmd.parse::<usize>() {
            let target = Self::read(host, |d| {
                let l = n.saturating_sub(1).min(d.len_lines().saturating_sub(1));
                core_vim::first_non_blank(d, l)
            });
            if let Some(t) = target {
                Self::caret(host, t);
            }
            return;
        }
        match cmd {
            "w" | "write" => host.execute("file.save"),
            "wq" | "x" | "wq!" | "x!" => {
                host.execute("file.save");
                host.execute("tab.close");
            }
            "wa" | "wall" => host.execute("file.saveAll"),
            "q" | "quit" => host.execute("tab.close"),
            "q!" | "quit!" => {
                if let Some(id) = host.active_doc() {
                    host.set_dirty(id, false);
                }
                host.execute("tab.close");
            }
            "qa" | "qall" | "qa!" | "quitall" | "quitall!" => host.execute("app.quit"),
            "noh" | "nohl" | "nohlsearch" => {
                host.dismiss_prompt();
                if let Some(id) = host.active_doc() {
                    host.clear_decorations(id, "find.match");
                }
            }
            _ => {
                if is_substitute(cmd) {
                    self.substitute_ex(cmd, host);
                } else {
                    host.notify(format!("Not an editor command: {cmd}"));
                }
            }
        }
    }

    fn substitute_ex(&mut self, cmd: &str, host: &mut dyn Host) {
        let whole = cmd.starts_with('%');
        let body = cmd.trim_start_matches('%');
        let body = body.strip_prefix('s').unwrap_or(body);
        let mut parts = body.splitn(4, '/');
        let _lead = parts.next();
        let (Some(old), new) = (parts.next(), parts.next().unwrap_or("")) else {
            return;
        };
        if old.is_empty() {
            return;
        }
        let global = parts.next().unwrap_or("").contains('g');
        let (old, new) = (old.to_string(), new.to_string());
        let plan = Self::read(host, |d| {
            let (start, end) = if whole {
                (0, d.len_chars())
            } else {
                let line = d.char_to_line(d.selections.primary().head);
                let ls = d.line_to_char(line);
                (ls, ls + d.line_len_chars(line))
            };
            let src = d.rope().slice(start..end).to_string();
            let out = if global {
                src.replace(&old, &new)
            } else {
                src.replacen(&old, &new, 1)
            };
            (start, end, src, out)
        });
        if let Some((start, end, src, out)) = plan {
            if out != src {
                Self::replace(host, start, end, out);
                Self::caret(host, start);
            }
        }
    }
}
