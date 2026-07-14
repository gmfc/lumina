//! Insert mode plus the single-key edits (x/s/J/~/p, undo/redo, scroll, dot-repeat).

use super::state::{Mode, Operator};
use super::toggle_case;
use super::VimPlugin;
use editor_core::vim as core_vim;
use editor_plugin::input::{Key, KeyCode};
use editor_plugin::Host;

/// The leading whitespace of `line`.
fn leading_ws(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

impl VimPlugin {
    pub(super) fn insert_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        let esc = key.code == KeyCode::Esc || (key.code == KeyCode::Char('[') && key.ctrl);
        if esc {
            self.leave_insert(host);
            return true;
        }
        // During dot-repeat replay there's no app pipeline behind us, so insert the literal char
        // ourselves; live typing falls through to the editor (auto-pairs / auto-indent).
        if self.s().replaying {
            if let KeyCode::Char(c) = key.code {
                if !key.ctrl && !key.alt {
                    let head = Self::primary_head(host);
                    Self::replace(host, head, head, c.to_string());
                    Self::caret(host, head + 1);
                    return true;
                }
            }
            if key.code == KeyCode::Enter {
                let head = Self::primary_head(host);
                Self::replace(host, head, head, "\n".into());
                Self::caret(host, head + 1);
                return true;
            }
        }
        false
    }

    fn leave_insert(&mut self, host: &mut dyn Host) {
        self.sm().mode = Mode::Normal;
        // Vim steps the cursor one left when leaving insert (clamped to line start).
        let target = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let ls = d.line_to_char(line);
            head.saturating_sub(1).max(ls)
        });
        if let Some(t) = target {
            Self::caret(host, t);
        }
    }

    pub(super) fn enter_insert_at(&mut self, forward: Option<usize>, host: &mut dyn Host) {
        if let Some(n) = forward {
            let target = Self::read(host, |d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let end = d.line_to_char(line) + d.line_len_chars(line);
                (head + n).min(end)
            });
            if let Some(t) = target {
                Self::caret(host, t);
            }
        }
        self.sm().mode = Mode::Insert;
    }

    pub(super) fn insert_first_non_blank(&mut self, host: &mut dyn Host) {
        let target = Self::read(host, |d| {
            core_vim::first_non_blank(d, d.char_to_line(d.selections.primary().head))
        });
        if let Some(t) = target {
            Self::caret(host, t);
        }
        self.sm().mode = Mode::Insert;
    }

    pub(super) fn insert_line_end(&mut self, host: &mut dyn Host) {
        let target = Self::read(host, |d| {
            let line = d.char_to_line(d.selections.primary().head);
            d.line_to_char(line) + d.line_len_chars(line)
        });
        if let Some(t) = target {
            Self::caret(host, t);
        }
        self.sm().mode = Mode::Insert;
    }

    pub(super) fn insert_col1(&mut self, host: &mut dyn Host) {
        let target = Self::read(host, |d| {
            d.line_to_char(d.char_to_line(d.selections.primary().head))
        });
        if let Some(t) = target {
            Self::caret(host, t);
        }
        self.sm().mode = Mode::Insert;
    }

    pub(super) fn open_line(&mut self, below: bool, host: &mut dyn Host) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let plan = host.workspace().documents.get(id).map(|d| {
            let line = d.char_to_line(d.selections.primary().head);
            let text = d.line_text(line);
            let indent = leading_ws(text.trim_end_matches(['\n', '\r']));
            let indent_chars = indent.chars().count();
            if below {
                let at = d.line_to_char(line) + d.line_len_chars(line);
                (at, format!("\n{indent}"), at + 1 + indent_chars)
            } else {
                let at = d.line_to_char(line);
                (at, format!("{indent}\n"), at + indent_chars)
            }
        });
        if let Some((at, text, caret)) = plan {
            Self::replace(host, at, at, text);
            Self::caret(host, caret);
        }
        self.sm().mode = Mode::Insert;
    }

    pub(super) fn substitute_char(&mut self, host: &mut dyn Host) {
        let count = self.s().effective_count();
        let reg = self.s().register;
        let plan = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let content_end = d.line_to_char(line) + d.line_len_chars(line);
            let end = (head + count).min(content_end);
            let text = if head < end {
                d.rope().slice(head..end).to_string()
            } else {
                String::new()
            };
            (head, end, text)
        });
        if let Some((start, end, text)) = plan {
            if start < end {
                self.store_register(reg, text, false, false, host);
                self.delete_range(start, end, false, host);
            }
        }
        self.sm().mode = Mode::Insert;
        self.sm().clear_pending();
    }

    pub(super) fn change_current_lines(&mut self, host: &mut dyn Host) {
        self.linewise_current(Operator::Change, host);
        self.sm().clear_pending();
    }

    pub(super) fn change_to_eol(&mut self, host: &mut dyn Host) {
        let (head, end) = self.line_tail_range(host);
        self.apply_operator_range(Operator::Change, head, end, false, host);
        self.sm().clear_pending();
    }

    pub(super) fn delete_to_eol(&mut self, host: &mut dyn Host) {
        let (head, end) = self.line_tail_range(host);
        self.apply_operator_range(Operator::Delete, head, end, false, host);
        self.sm().clear_pending();
    }

    fn line_tail_range(&self, host: &dyn Host) -> (usize, usize) {
        Self::read(host, |d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let end = d.line_to_char(line) + d.line_len_chars(line);
            (head, end)
        })
        .unwrap_or((0, 0))
    }

    pub(super) fn delete_char(&mut self, forward: bool, host: &mut dyn Host) {
        let count = self.s().effective_count();
        let reg = self.s().register;
        let range = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let ls = d.line_to_char(line);
            let content_end = ls + d.line_len_chars(line);
            if forward {
                (head, (head + count).min(content_end))
            } else {
                (head.saturating_sub(count).max(ls), head)
            }
        });
        if let Some((start, end)) = range {
            if start < end {
                let text = Self::read(host, |d| d.rope().slice(start..end).to_string())
                    .unwrap_or_default();
                self.store_register(reg, text, false, false, host);
                self.delete_range(start, end, false, host);
                self.clamp_caret(host);
            }
        }
        self.sm().clear_pending();
    }

    pub(super) fn toggle_case_cmd(&mut self, host: &mut dyn Host) {
        let count = self.s().effective_count();
        let range = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let content_end = d.line_to_char(line) + d.line_len_chars(line);
            (head, (head + count).min(content_end))
        });
        if let Some((start, end)) = range {
            if start < end {
                self.transform_range(start, end, toggle_case, host);
                let target = Self::read(host, |d| {
                    let line = d.char_to_line(end);
                    let content_end = d.line_to_char(line) + d.line_len_chars(line);
                    end.min(content_end)
                });
                if let Some(t) = target {
                    Self::caret(host, t);
                }
            }
        }
        self.sm().clear_pending();
    }

    pub(super) fn join_lines(&mut self, host: &mut dyn Host) {
        let count = self.s().effective_count().max(2);
        let joins = count - 1;
        for _ in 0..joins {
            let plan = Self::read(host, |d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                if line + 1 >= d.len_lines() {
                    return None;
                }
                let end_of_line = d.line_to_char(line) + d.line_len_chars(line);
                let next_first = core_vim::first_non_blank(d, line + 1);
                let empty_line = d.line_len_chars(line) == 0;
                let repl = if empty_line { "" } else { " " };
                Some((end_of_line, next_first, repl.to_string()))
            })
            .flatten();
            let Some((eol, next_first, repl)) = plan else {
                break;
            };
            Self::replace(host, eol, next_first, repl);
            Self::caret(host, eol);
        }
        self.sm().clear_pending();
    }

    pub(super) fn replace_char(&mut self, ch: char, host: &mut dyn Host) {
        let count = self.s().effective_count();
        let range = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let content_end = d.line_to_char(line) + d.line_len_chars(line);
            (head, (head + count).min(content_end))
        });
        let Some((start, end)) = range else {
            self.sm().clear_pending();
            return;
        };
        if start >= end {
            self.sm().clear_pending();
            return;
        }
        let repl = if ch == '\n' {
            "\n".to_string()
        } else {
            ch.to_string().repeat(end - start)
        };
        Self::replace(host, start, end, repl);
        if ch != '\n' {
            Self::caret(host, end.saturating_sub(1).max(start));
        }
        self.sm().clear_pending();
    }

    pub(super) fn paste(&mut self, before: bool, host: &mut dyn Host) {
        let count = self.s().effective_count();
        let reg = self.s().register;
        let data = self.read_register(reg, host);
        if data.text.is_empty() {
            self.sm().clear_pending();
            return;
        }
        if data.linewise {
            let body = data.text.trim_end_matches('\n').to_string();
            let block = std::iter::repeat_n(body.as_str(), count)
                .collect::<Vec<_>>()
                .join("\n");
            let plan = Self::read(host, |d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                (
                    line,
                    d.line_to_char(line),
                    d.line_to_char(line) + d.line_len_chars(line),
                )
            });
            if let Some((line, line_start, line_end)) = plan {
                if before {
                    Self::replace(host, line_start, line_start, format!("{block}\n"));
                    let p = Self::read(host, |d| core_vim::first_non_blank(d, line))
                        .unwrap_or(line_start);
                    Self::caret(host, p);
                } else {
                    Self::replace(host, line_end, line_end, format!("\n{block}"));
                    let p = Self::read(host, |d| core_vim::first_non_blank(d, line + 1))
                        .unwrap_or(line_end);
                    Self::caret(host, p);
                }
            }
        } else {
            let text = data.text.repeat(count);
            let at = Self::read(host, |d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let content_end = d.line_to_char(line) + d.line_len_chars(line);
                if before {
                    head
                } else {
                    (head + 1).min(content_end)
                }
            });
            if let Some(at) = at {
                let n = text.chars().count();
                Self::replace(host, at, at, text);
                Self::caret(host, (at + n).saturating_sub(1).max(at));
            }
        }
        self.sm().clear_pending();
    }

    pub(super) fn undo(&mut self, host: &mut dyn Host) {
        let count = self.s().effective_count();
        for _ in 0..count {
            host.execute("edit.undo");
        }
        self.sm().clear_pending();
    }

    pub(super) fn redo(&mut self, host: &mut dyn Host) {
        let count = self.s().effective_count();
        for _ in 0..count {
            host.execute("edit.redo");
        }
        self.sm().clear_pending();
    }

    pub(super) fn scroll(&mut self, down: bool, half: bool, host: &mut dyn Host) {
        let page = host.viewport_height();
        let amount = if half { (page / 2).max(1) } else { page.max(1) };
        let delta = if down {
            amount as isize
        } else {
            -(amount as isize)
        };
        self.move_lines(delta, false, host);
        self.sm().clear_pending();
    }

    pub(super) fn dot_repeat(&mut self, host: &mut dyn Host) {
        let times = self.count_opt().unwrap_or(1);
        let keys = self.s().last_change.clone();
        self.sm().clear_pending();
        if keys.is_empty() {
            return;
        }
        self.sm().replaying = true;
        for _ in 0..times {
            for k in &keys {
                self.handle_key(*k, host);
            }
        }
        self.sm().replaying = false;
    }

    /// Clamp the caret so it doesn't sit past the last char of its line.
    fn clamp_caret(&mut self, host: &mut dyn Host) {
        let target = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let ls = d.line_to_char(line);
            let content = d.line_len_chars(line);
            let max = if content == 0 { ls } else { ls + content - 1 };
            if head > max {
                Some(max)
            } else {
                None
            }
        })
        .flatten();
        if let Some(t) = target {
            Self::caret(host, t);
        }
    }
}
