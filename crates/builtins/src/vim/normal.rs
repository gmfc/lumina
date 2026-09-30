//! Normal-mode dispatch: key routing, operator/prefix pending, `g`/`z` prefixes, counts.

use super::state::{FindPending, Mode, MotionKind, Operator, Prefix};
use super::VimPlugin;
use editor_core::vim as core_vim;
use editor_core::vim::TextObject;
use editor_plugin::input::{Key, KeyCode};
use editor_plugin::Host;

/// The key that "doubles" an operator into a linewise line op (`dd`, `yy`, `>>`).
fn double_key(op: Operator) -> char {
    match op {
        Operator::Delete => 'd',
        Operator::Change => 'c',
        Operator::Yank => 'y',
        Operator::Indent => '>',
        Operator::Outdent => '<',
        Operator::Reindent => '=',
        Operator::Format => 'q', // gq doubled as gqq → line format
        Operator::Lower => 'u',
        Operator::Upper => 'U',
        Operator::ToggleCase => '~',
    }
}

/// Map the key after `i`/`a` to a text object.
fn object_from_key(code: KeyCode) -> Option<TextObject> {
    let c = match code {
        KeyCode::Char(c) => c,
        _ => return None,
    };
    Some(match c {
        'w' => TextObject::Word { big: false },
        'W' => TextObject::Word { big: true },
        '(' | ')' | 'b' => TextObject::Pair {
            open: '(',
            close: ')',
        },
        '{' | '}' | 'B' => TextObject::Pair {
            open: '{',
            close: '}',
        },
        '[' | ']' => TextObject::Pair {
            open: '[',
            close: ']',
        },
        '<' | '>' => TextObject::Pair {
            open: '<',
            close: '>',
        },
        '"' => TextObject::Quote { quote: '"' },
        '\'' => TextObject::Quote { quote: '\'' },
        '`' => TextObject::Quote { quote: '`' },
        'p' => TextObject::Paragraph,
        't' => TextObject::Tag,
        _ => return None,
    })
}

impl VimPlugin {
    pub(super) fn normal_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        let code = key.code;
        if code == KeyCode::Esc {
            self.sm().clear_pending();
            return true;
        }

        // Resolve a pending single-char argument (f/t/F/T target).
        if let Some(fp) = self.s().find_pending {
            self.sm().find_pending = None;
            if let KeyCode::Char(c) = code {
                self.find_apply(fp, c, true, host);
            } else {
                self.sm().clear_pending();
            }
            return true;
        }

        // Resolve a multi-key prefix (register / replace / text object / g / z).
        if let Some(prefix) = self.s().prefix {
            return self.handle_prefix(prefix, key, host);
        }

        if key.ctrl {
            return match code {
                KeyCode::Char('r') => {
                    self.redo(host);
                    true
                }
                KeyCode::Char('d') => {
                    self.scroll(true, true, host);
                    true
                }
                KeyCode::Char('u') => {
                    self.scroll(false, true, host);
                    true
                }
                KeyCode::Char('f') => {
                    self.scroll(true, false, host);
                    true
                }
                KeyCode::Char('b') => {
                    self.scroll(false, false, host);
                    true
                }
                KeyCode::Char('o') => {
                    let cur = Self::primary_head(host);
                    if let Some(t) = self.sm().jump_older(cur) {
                        Self::caret(host, t);
                    }
                    true
                }
                KeyCode::Char('i') => {
                    if let Some(t) = self.sm().jump_newer() {
                        Self::caret(host, t);
                    }
                    true
                }
                _ => false,
            };
        }
        if key.alt {
            return false;
        }

        if self.s().operator.is_some() {
            self.operator_pending(key, host)
        } else {
            self.normal_idle(key, host)
        }
    }

    fn normal_idle(&mut self, key: Key, host: &mut dyn Host) -> bool {
        let code = key.code;
        match code {
            KeyCode::Char(c @ '1'..='9') => {
                self.sm().push_digit(c as usize - '0' as usize);
            }
            KeyCode::Char('0') if self.s().count_active() => {
                self.sm().push_digit(0);
            }
            KeyCode::Char('"') => self.sm().prefix = Some(Prefix::Register),
            KeyCode::Char('d') => self.sm().operator = Some(Operator::Delete),
            KeyCode::Char('c') => self.sm().operator = Some(Operator::Change),
            KeyCode::Char('y') => self.sm().operator = Some(Operator::Yank),
            KeyCode::Char('>') => self.sm().operator = Some(Operator::Indent),
            KeyCode::Char('<') => self.sm().operator = Some(Operator::Outdent),
            KeyCode::Char('g') => self.sm().prefix = Some(Prefix::G),
            KeyCode::Char('z') => self.sm().prefix = Some(Prefix::Z),
            KeyCode::Char('r') => self.sm().prefix = Some(Prefix::Replace),
            KeyCode::Char('f') => self.sm().find_pending = Some(FindPending::Find),
            KeyCode::Char('F') => self.sm().find_pending = Some(FindPending::FindBack),
            KeyCode::Char('t') => self.sm().find_pending = Some(FindPending::Till),
            KeyCode::Char('T') => self.sm().find_pending = Some(FindPending::TillBack),
            KeyCode::Char(';') => self.repeat_find(false, host),
            KeyCode::Char(',') => self.repeat_find(true, host),
            KeyCode::Char('i') => self.enter_insert_at(None, host),
            KeyCode::Char('a') => self.enter_insert_at(Some(1), host),
            KeyCode::Char('I') => self.insert_first_non_blank(host),
            KeyCode::Char('A') => self.insert_line_end(host),
            KeyCode::Char('o') => self.open_line(true, host),
            KeyCode::Char('O') => self.open_line(false, host),
            KeyCode::Char('s') => self.substitute_char(host),
            KeyCode::Char('S') => self.change_current_lines(host),
            KeyCode::Char('C') => self.change_to_eol(host),
            KeyCode::Char('D') => self.delete_to_eol(host),
            KeyCode::Char('x') => self.delete_char(true, host),
            KeyCode::Char('X') => self.delete_char(false, host),
            KeyCode::Char('~') => self.toggle_case_cmd(host),
            KeyCode::Char('J') => self.join_lines(host),
            KeyCode::Char('p') => self.paste(false, host),
            KeyCode::Char('P') => self.paste(true, host),
            KeyCode::Char('u') => self.undo(host),
            KeyCode::Char('.') => self.dot_repeat(host),
            KeyCode::Char('v') => self.enter_visual(Mode::Visual, host),
            KeyCode::Char('V') => self.enter_visual(Mode::VisualLine, host),
            KeyCode::Char(':') => self.open_command(),
            KeyCode::Char('/') => self.open_search(true),
            KeyCode::Char('?') => self.open_search(false),
            KeyCode::Char('n') => self.search_next(false, host),
            KeyCode::Char('N') => self.search_next(true, host),
            KeyCode::Char('*') => self.search_word(true, host),
            KeyCode::Char('#') => self.search_word(false, host),
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => {
                // Bare j/k: logical lines (#54). Visual-row motion is gj/gk.
                let n = self.s().effective_count() as isize;
                self.move_logical_lines(n, false, host);
                self.sm().count = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let n = self.s().effective_count() as isize;
                self.move_logical_lines(-n, false, host);
                self.sm().count = None;
            }
            KeyCode::Char('m') => self.sm().prefix = Some(Prefix::MarkSet),
            KeyCode::Char('`') => self.sm().prefix = Some(Prefix::MarkJumpExact),
            KeyCode::Char('\'') => self.sm().prefix = Some(Prefix::MarkJumpLine),
            KeyCode::Char('q') => {
                if self.s().macro_recording.is_some() {
                    // Stop recording.
                    if let Some((reg, keys)) = self.sm().macro_recording.take() {
                        self.sm().macros.insert(reg, keys);
                    }
                } else if !self.s().macro_replaying {
                    self.sm().prefix = Some(Prefix::MacroRecord);
                }
            }
            KeyCode::Char('@') if !self.s().macro_replaying => {
                self.sm().prefix = Some(Prefix::MacroPlay);
            }
            KeyCode::Char('=') => self.sm().operator = Some(Operator::Reindent),
            _ => {
                let count_opt = self.count_opt();
                if let Some((target, kind)) = self.motion(code, count_opt, host) {
                    // Jump-list entries for G / %.
                    let is_jump = matches!(code, KeyCode::Char('G') | KeyCode::Char('%'));
                    if is_jump {
                        let from = Self::primary_head(host);
                        let same_line = Self::read(host, |d| {
                            d.char_to_line(from) == d.char_to_line(target)
                        })
                        .unwrap_or(false);
                        self.sm().push_jump(from, target, same_line);
                    }
                    self.caret_move(target, host);
                    self.sm().count = None;
                    let _ = kind;
                } else {
                    self.sm().clear_pending();
                }
            }
        }
        true
    }

    fn operator_pending(&mut self, key: Key, host: &mut dyn Host) -> bool {
        let code = key.code;
        let op = self.s().operator.expect("operator pending");
        if let KeyCode::Char(c) = code {
            if c.is_ascii_digit() && (c != '0' || self.s().count_active()) {
                self.sm().push_digit(c as usize - '0' as usize);
                return true;
            }
            if c == '"' {
                self.sm().prefix = Some(Prefix::Register);
                return true;
            }
            if c == double_key(op) {
                self.linewise_current(op, host);
                self.sm().clear_pending();
                return true;
            }
            match c {
                'i' => {
                    self.sm().prefix = Some(Prefix::Object { around: false });
                    return true;
                }
                'a' => {
                    self.sm().prefix = Some(Prefix::Object { around: true });
                    return true;
                }
                'g' => {
                    self.sm().prefix = Some(Prefix::G);
                    return true;
                }
                'f' => {
                    self.sm().find_pending = Some(FindPending::Find);
                    return true;
                }
                'F' => {
                    self.sm().find_pending = Some(FindPending::FindBack);
                    return true;
                }
                't' => {
                    self.sm().find_pending = Some(FindPending::Till);
                    return true;
                }
                'T' => {
                    self.sm().find_pending = Some(FindPending::TillBack);
                    return true;
                }
                ';' => {
                    self.repeat_find(false, host);
                    return true;
                }
                ',' => {
                    self.repeat_find(true, host);
                    return true;
                }
                _ => {}
            }
        }
        let count_opt = self.count_opt();
        if let Some((target, kind)) = self.motion(code, count_opt, host) {
            let from = Self::primary_head(host);
            self.apply_operator_kind(op, from, target, kind, host);
            self.sm().clear_pending();
        } else {
            self.sm().clear_pending();
        }
        true
    }

    pub(super) fn handle_prefix(&mut self, prefix: Prefix, key: Key, host: &mut dyn Host) -> bool {
        self.sm().prefix = None;
        match prefix {
            Prefix::Register => {
                if let KeyCode::Char(c) = key.code {
                    self.sm().register = Some(c);
                }
                true
            }
            Prefix::Replace => {
                let ch = match key.code {
                    KeyCode::Char(c) => Some(c),
                    KeyCode::Enter => Some('\n'),
                    _ => None,
                };
                match ch {
                    Some(c) if matches!(self.s().mode, Mode::Visual | Mode::VisualLine) => {
                        self.visual_replace(c, host)
                    }
                    Some(c) => self.replace_char(c, host),
                    None => self.sm().clear_pending(),
                }
                true
            }
            Prefix::Object { around } => {
                if let Some(obj) = object_from_key(key.code) {
                    self.apply_text_object(obj, around, host);
                } else {
                    self.sm().clear_pending();
                }
                true
            }
            Prefix::G => self.g_prefix(key, host),
            Prefix::Z => {
                self.z_prefix(key, host);
                true
            }
            Prefix::MarkSet => {
                if let KeyCode::Char(c @ 'a'..='z') = key.code {
                    let pos = Self::primary_head(host);
                    self.sm().marks.insert(c, pos);
                }
                self.sm().clear_pending();
                true
            }
            Prefix::MarkJumpExact => {
                if let KeyCode::Char(c @ 'a'..='z') = key.code {
                    if let Some(&pos) = self.s().marks.get(&c) {
                        self.jump_to(pos, host);
                    }
                }
                self.sm().clear_pending();
                true
            }
            Prefix::MarkJumpLine => {
                if let KeyCode::Char(c @ 'a'..='z') = key.code {
                    if let Some(pos) = self.s().marks.get(&c).copied() {
                        let target = Self::read(host, |d| {
                            let l = d.char_to_line(pos.min(d.len_chars()));
                            core_vim::first_non_blank(d, l)
                        });
                        if let Some(t) = target {
                            self.jump_to(t, host);
                        }
                    }
                }
                self.sm().clear_pending();
                true
            }
            Prefix::MacroRecord => {
                if let KeyCode::Char(c @ 'a'..='z') = key.code {
                    self.sm().macro_recording = Some((c, Vec::new()));
                }
                self.sm().clear_pending();
                true
            }
            Prefix::MacroPlay => {
                let reg = match key.code {
                    KeyCode::Char('@') => self.s().last_macro,
                    KeyCode::Char(c @ 'a'..='z') => Some(c),
                    _ => None,
                };
                if let Some(r) = reg {
                    self.play_macro(r, host);
                }
                self.sm().clear_pending();
                true
            }
        }
    }

    fn play_macro(&mut self, reg: char, host: &mut dyn Host) {
        let keys = self.s().macros.get(&reg).cloned().unwrap_or_default();
        if keys.is_empty() || self.s().macro_replaying {
            return;
        }
        self.sm().last_macro = Some(reg);
        self.sm().macro_replaying = true;
        for k in keys {
            self.handle_key(k, host);
        }
        self.sm().macro_replaying = false;
    }

    fn g_prefix(&mut self, key: Key, host: &mut dyn Host) -> bool {
        let op = self.s().operator;
        let count_opt = self.count_opt();
        match key.code {
            KeyCode::Char('g') => {
                let target = Self::read(host, |doc| {
                    let l = match count_opt {
                        Some(c) => c.saturating_sub(1).min(doc.len_lines().saturating_sub(1)),
                        None => 0,
                    };
                    core_vim::first_non_blank(doc, l)
                });
                match target {
                    Some(t) => {
                        if op.is_none() {
                            self.jump_to(t, host);
                            self.sm().clear_pending();
                        } else {
                            self.motion_result(t, MotionKind::Linewise, host);
                        }
                    }
                    None => self.sm().clear_pending(),
                }
            }
            KeyCode::Char('j') => {
                let n = count_opt.unwrap_or(1) as isize;
                if let Some(op) = op {
                    // Operator + gj: use visual-row motion target via move then apply — fall back
                    // to logical linewise j for operators (gj as operator motion is rare).
                    let from = Self::primary_head(host);
                    self.move_lines(n, false, host);
                    let to = Self::primary_head(host);
                    self.apply_operator_kind(op, from, to, MotionKind::Exclusive, host);
                    self.sm().clear_pending();
                } else {
                    self.move_lines(n, false, host);
                    self.sm().clear_pending();
                }
            }
            KeyCode::Char('k') => {
                let n = count_opt.unwrap_or(1) as isize;
                if let Some(op) = op {
                    let from = Self::primary_head(host);
                    self.move_lines(-n, false, host);
                    let to = Self::primary_head(host);
                    self.apply_operator_kind(op, from, to, MotionKind::Exclusive, host);
                    self.sm().clear_pending();
                } else {
                    self.move_lines(-n, false, host);
                    self.sm().clear_pending();
                }
            }
            KeyCode::Char('q') if op.is_none() => {
                self.sm().operator = Some(Operator::Format);
            }
            KeyCode::Char('e') => {
                let count = count_opt.unwrap_or(1);
                let target =
                    self.iter_motion(|d, p| core_vim::prev_word_end(d, p, false), count, host);
                self.motion_result(target, MotionKind::Inclusive, host);
            }
            KeyCode::Char('E') => {
                let count = count_opt.unwrap_or(1);
                let target =
                    self.iter_motion(|d, p| core_vim::prev_word_end(d, p, true), count, host);
                self.motion_result(target, MotionKind::Inclusive, host);
            }
            KeyCode::Char('_') => {
                let target = Self::read(host, |doc| {
                    core_vim::last_non_blank(doc, doc.selections.primary().head)
                });
                match target {
                    Some(t) => self.motion_result(t, MotionKind::Inclusive, host),
                    None => self.sm().clear_pending(),
                }
            }
            KeyCode::Char('u') if op.is_none() => self.sm().operator = Some(Operator::Lower),
            KeyCode::Char('U') if op.is_none() => self.sm().operator = Some(Operator::Upper),
            KeyCode::Char('~') if op.is_none() => self.sm().operator = Some(Operator::ToggleCase),
            KeyCode::Char('I') if op.is_none() => self.insert_col1(host),
            _ => self.sm().clear_pending(),
        }
        true
    }

    fn z_prefix(&mut self, key: Key, host: &mut dyn Host) {
        let height = host.viewport_height().max(1);
        let info = Self::read(host, |d| {
            let head = d.selections.primary().head;
            (d.char_to_line(head), d.len_lines())
        });
        let Some((cur_line, n_lines)) = info else {
            return;
        };
        let scroll = match key.code {
            KeyCode::Char('z') | KeyCode::Char('.') => cur_line.saturating_sub(height / 2),
            KeyCode::Char('t') | KeyCode::Enter => cur_line,
            KeyCode::Char('b') => cur_line.saturating_sub(height.saturating_sub(1)),
            _ => return,
        };
        if let Some(id) = host.active_doc() {
            host.set_scroll(id, scroll.min(n_lines.saturating_sub(1)));
        }
        self.sm().clear_pending();
    }

    pub(super) fn count_opt(&self) -> Option<usize> {
        let v = self.s();
        if v.has_count() {
            Some(v.effective_count())
        } else {
            None
        }
    }
}
