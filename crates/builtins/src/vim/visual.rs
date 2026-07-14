//! Visual and Visual-Line mode: selection extension, operators, and `r`/`J` over a range.

use super::state::{FindPending, Mode, MotionKind, Operator, Prefix};
use super::VimPlugin;
use editor_plugin::input::{Key, KeyCode};
use editor_plugin::Host;

impl VimPlugin {
    pub(super) fn enter_visual(&mut self, mode: Mode, host: &mut dyn Host) {
        if self.s().mode == mode {
            self.sm().mode = Mode::Normal;
            let head = Self::primary_head(host);
            Self::caret(host, head);
            return;
        }
        self.sm().mode = mode;
        let head = Self::primary_head(host);
        Self::select(host, head, head);
    }

    pub(super) fn visual_set_head(&mut self, target: usize, host: &mut dyn Host) {
        let anchor = Self::read(host, |d| d.selections.primary().anchor).unwrap_or(0);
        Self::select(host, anchor, target);
    }

    pub(super) fn visual_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        let code = key.code;
        if code == KeyCode::Esc {
            self.sm().mode = Mode::Normal;
            let head = Self::primary_head(host);
            Self::caret(host, head);
            self.sm().clear_pending();
            return true;
        }

        if let Some(fp) = self.s().find_pending {
            self.sm().find_pending = None;
            if let KeyCode::Char(c) = code {
                self.find_apply(fp, c, true, host);
            }
            return true;
        }
        if let Some(prefix) = self.s().prefix {
            return self.handle_prefix(prefix, key, host);
        }

        match code {
            KeyCode::Char(c @ '1'..='9') => self.sm().push_digit(c as usize - '0' as usize),
            KeyCode::Char('0') if self.s().count_active() => self.sm().push_digit(0),
            KeyCode::Char('"') => self.sm().prefix = Some(Prefix::Register),
            KeyCode::Char('v') => self.enter_visual(Mode::Visual, host),
            KeyCode::Char('V') => self.enter_visual(Mode::VisualLine, host),
            KeyCode::Char('o') => {
                let swap = Self::read(host, |d| {
                    let s = d.selections.primary();
                    (s.head, s.anchor)
                });
                if let Some((anchor, head)) = swap {
                    Self::select(host, anchor, head);
                }
            }
            KeyCode::Char('i') => self.sm().prefix = Some(Prefix::Object { around: false }),
            KeyCode::Char('a') => self.sm().prefix = Some(Prefix::Object { around: true }),
            KeyCode::Char('g') => self.sm().prefix = Some(Prefix::G),
            KeyCode::Char('f') => self.sm().find_pending = Some(FindPending::Find),
            KeyCode::Char('F') => self.sm().find_pending = Some(FindPending::FindBack),
            KeyCode::Char('t') => self.sm().find_pending = Some(FindPending::Till),
            KeyCode::Char('T') => self.sm().find_pending = Some(FindPending::TillBack),
            KeyCode::Char(';') => self.repeat_find(false, host),
            KeyCode::Char(',') => self.repeat_find(true, host),
            KeyCode::Char('d') | KeyCode::Char('x') => self.visual_operator(Operator::Delete, host),
            KeyCode::Char('c') | KeyCode::Char('s') => self.visual_operator(Operator::Change, host),
            KeyCode::Char('y') => self.visual_operator(Operator::Yank, host),
            KeyCode::Char('>') => self.visual_operator(Operator::Indent, host),
            KeyCode::Char('<') => self.visual_operator(Operator::Outdent, host),
            KeyCode::Char('u') => self.visual_operator(Operator::Lower, host),
            KeyCode::Char('U') => self.visual_operator(Operator::Upper, host),
            KeyCode::Char('~') => self.visual_operator(Operator::ToggleCase, host),
            KeyCode::Char('r') => self.sm().prefix = Some(Prefix::Replace),
            KeyCode::Char('J') => {
                let lines = Self::read(host, |d| {
                    let s = d.selections.primary();
                    let fl = d.char_to_line(s.from());
                    let ll = d.char_to_line(s.to());
                    (d.line_to_char(fl), (ll - fl) + 1)
                })
                .unwrap_or((0, 1));
                self.sm().mode = Mode::Normal;
                self.sm().count = Some(lines.1.max(2));
                Self::caret(host, lines.0);
                self.join_lines(host);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let n = self.s().effective_count() as isize;
                self.move_lines(n, true, host);
                self.sm().count = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let n = self.s().effective_count() as isize;
                self.move_lines(-n, true, host);
                self.sm().count = None;
            }
            _ => {
                let count_opt = self.count_opt();
                if let Some((target, _kind)) = self.motion(code, count_opt, host) {
                    self.visual_set_head(target, host);
                    self.sm().count = None;
                } else {
                    self.sm().clear_pending();
                }
            }
        }
        true
    }

    pub(super) fn visual_replace(&mut self, ch: char, host: &mut dyn Host) {
        let range = Self::read(host, |d| {
            let s = d.selections.primary();
            (s.from(), (s.to() + 1).min(d.len_chars()))
        });
        self.sm().mode = Mode::Normal;
        let Some((start, end)) = range else {
            self.sm().clear_pending();
            return;
        };
        if start >= end {
            self.sm().clear_pending();
            return;
        }
        let out: String = Self::read(host, |d| {
            d.rope()
                .slice(start..end)
                .chars()
                .map(|c| if c == '\n' { '\n' } else { ch })
                .collect()
        })
        .unwrap_or_default();
        Self::replace(host, start, end, out);
        Self::caret(host, start);
        self.sm().clear_pending();
    }

    fn visual_operator(&mut self, op: Operator, host: &mut dyn Host) {
        let linewise = self.s().mode == Mode::VisualLine;
        let range = Self::read(host, |d| {
            let s = d.selections.primary();
            (s.from(), (s.to() + 1).min(d.len_chars()))
        });
        self.sm().mode = Mode::Normal;
        let Some((start, end)) = range else {
            return;
        };
        if linewise {
            self.apply_operator_kind(
                op,
                start,
                end.saturating_sub(1).max(start),
                MotionKind::Linewise,
                host,
            );
        } else {
            self.apply_operator_range(op, start, end, false, host);
        }
        self.sm().clear_pending();
    }
}
