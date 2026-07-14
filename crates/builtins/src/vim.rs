//! Vim modal editing, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the whole modal state machine ([`VimState`]) and intercepts keys through
//! [`Plugin::capture_key`] before chord resolution. Every buffer change is a `Transaction` applied
//! via [`Host::apply_transaction`] (+ [`Host::set_selections`] for the caret); the pure
//! motion/text-object math is `editor_core::vim`. App-level actions (undo/redo/save/close/quit) go
//! through [`Host::execute`]; page motions/recenter/goal-column vertical motion use the small
//! `viewport_height`/`move_lines`/`set_scroll` ports; clipboard registers use `clipboard_*`. The
//! mode + pending hint are mirrored to the app via [`Host::set_vim_view`] for the badge + visual
//! shading; the renderer never reaches into the plugin.

use editor_core::transaction::Change;
use editor_core::vim as core_vim;
use editor_core::vim::{FindKind, TextObject};
use editor_core::{motion, DocId, Document, Selection, Selections, Transaction};
use editor_plugin::input::{Key, KeyCode};
use editor_plugin::{Contributions, Host, Plugin, VimMode, VimView};

mod state;
use state::{FindPending, Mode, MotionKind, Operator, Prefix, Register, VimState};

// ------------------------------------------------------------------------------------------------
// free helpers (pure)
// ------------------------------------------------------------------------------------------------

/// The key that "doubles" an operator into a linewise line op (`dd`, `yy`, `>>`).
fn double_key(op: Operator) -> char {
    match op {
        Operator::Delete => 'd',
        Operator::Change => 'c',
        Operator::Yank => 'y',
        Operator::Indent => '>',
        Operator::Outdent => '<',
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
        _ => return None,
    })
}

/// Toggle the case of a single char, as a `String` (case folding can widen).
fn toggle_case(c: char) -> String {
    if c.is_uppercase() {
        c.to_lowercase().collect()
    } else {
        c.to_uppercase().collect()
    }
}

/// Step `f` `count` times over `doc` from `start`, stopping if it stalls.
/// The one indent/outdent [`Change`] for line `l`, or `None` when outdenting a line with no
/// leading indentation. Indent inserts four spaces at the line start; outdent removes one tab or
/// up to `width` leading spaces.
fn line_indent_change(d: &Document, l: usize, indent: bool, width: usize) -> Option<Change> {
    let ls = d.line_to_char(l);
    if indent {
        return Some(Change {
            at: ls,
            removed: String::new(),
            inserted: "    ".into(),
        });
    }
    let chars: Vec<char> = d.line_text(l).chars().collect();
    let remove = leading_indent_width(&chars, width);
    (remove > 0).then(|| Change {
        at: ls,
        removed: d.rope().slice(ls..ls + remove).to_string(),
        inserted: String::new(),
    })
}

/// Number of leading whitespace chars one outdent removes: a single tab, else up to `width` spaces.
fn leading_indent_width(chars: &[char], width: usize) -> usize {
    if chars.first() == Some(&'\t') {
        return 1;
    }
    let mut n = 0;
    while n < width && chars.get(n) == Some(&' ') {
        n += 1;
    }
    n
}

fn nth(doc: &Document, start: usize, count: usize, f: impl Fn(&Document, usize) -> usize) -> usize {
    let mut p = start;
    for _ in 0..count {
        let np = f(doc, p);
        if np == p {
            break;
        }
        p = np;
    }
    p
}

/// True when `cmd` looks like a substitute command (`s/…` or `%s/…`).
fn is_substitute(cmd: &str) -> bool {
    cmd.starts_with("s/") || cmd.starts_with("%s/")
}

/// The leading whitespace of `line`.
fn leading_ws(line: &str) -> String {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Naive literal substring search over `chars` for `pat`, wrapping around. Returns the match start.
fn search_literal(chars: &[char], pat: &[char], start: usize, forward: bool) -> Option<usize> {
    if pat.is_empty() || chars.len() < pat.len() {
        return None;
    }
    let last_start = chars.len() - pat.len();
    let matches_at = |i: usize| chars[i..i + pat.len()] == *pat;
    if forward {
        let begin = start.min(chars.len());
        (begin..=last_start)
            .find(|&i| matches_at(i))
            .or_else(|| (0..begin.min(last_start + 1)).find(|&i| matches_at(i)))
    } else {
        let begin = start.min(last_start);
        (0..=begin)
            .rev()
            .find(|&i| matches_at(i))
            .or_else(|| (begin + 1..=last_start).rev().find(|&i| matches_at(i)))
    }
}

// ------------------------------------------------------------------------------------------------
// the plugin
// ------------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct VimPlugin {
    /// `Some` while the vim layer is enabled; the modal state machine lives here.
    state: Option<VimState>,
}

impl VimPlugin {
    const ID: &'static str = "vim";

    fn s(&self) -> &VimState {
        self.state.as_ref().expect("vim enabled")
    }
    fn sm(&mut self) -> &mut VimState {
        self.state.as_mut().expect("vim enabled")
    }

    // --- doc read/edit helpers over Host --------------------------------------------------------

    fn primary_head(host: &dyn Host) -> usize {
        host.active_doc()
            .and_then(|id| host.workspace().documents.get(id))
            .map(|d| d.selections.primary().head)
            .unwrap_or(0)
    }

    fn revision(host: &dyn Host) -> u64 {
        host.active_doc()
            .and_then(|id| host.workspace().documents.get(id))
            .map(|d| d.revision)
            .unwrap_or(0)
    }

    /// Read a value from the active document.
    fn read<T>(host: &dyn Host, f: impl FnOnce(&Document) -> T) -> Option<T> {
        let id = host.active_doc()?;
        host.workspace().documents.get(id).map(f)
    }

    /// Replace `[start, end)` in the active document with `text`, as one transaction.
    fn replace(host: &mut dyn Host, start: usize, end: usize, text: String) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let removed = match host.workspace().documents.get(id) {
            Some(d) => {
                let (s, e) = (start.min(d.len_chars()), end.min(d.len_chars()));
                if s < e {
                    d.rope().slice(s..e).to_string()
                } else {
                    String::new()
                }
            }
            None => return,
        };
        let at = start.min(removed_end_cap(host, id));
        let txn = Transaction::from_changes(vec![Change {
            at,
            removed,
            inserted: text,
        }]);
        host.apply_transaction(id, txn);
    }

    /// Set the primary caret to `pos` (clamped), collapsing any selection.
    fn caret(host: &mut dyn Host, pos: usize) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let p = match host.workspace().documents.get(id) {
            Some(d) => d.clamp(pos),
            None => return,
        };
        host.set_selections(id, Selections::single(Selection::caret(p)));
    }

    /// Set the primary selection to `[anchor, head]` (clamped head).
    fn select(host: &mut dyn Host, anchor: usize, head: usize) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let h = match host.workspace().documents.get(id) {
            Some(d) => d.clamp(head),
            None => return,
        };
        host.set_selections(id, Selections::single(Selection::new(anchor, h)));
    }

    // --- enable/disable + view mirror -----------------------------------------------------------

    fn set_enabled(&mut self, on: bool, host: &mut dyn Host) {
        if on {
            if self.state.is_none() {
                self.state = Some(VimState::new());
            }
            host.notify("Vim mode enabled".into());
            self.publish(host);
        } else {
            self.state = None;
            host.notify("Vim mode disabled".into());
            host.set_vim_view(None);
        }
    }

    /// Publish the mode + pending hint for the status badge / visual shading.
    fn publish(&self, host: &mut dyn Host) {
        let Some(v) = self.state.as_ref() else {
            host.set_vim_view(None);
            return;
        };
        let mode = match v.mode {
            Mode::Normal => VimMode::Normal,
            Mode::Insert => VimMode::Insert,
            Mode::Visual => VimMode::Visual,
            Mode::VisualLine => VimMode::VisualLine,
        };
        host.set_vim_view(Some(VimView {
            mode,
            pending: v.pending_hint(),
        }));
    }

    // --- entry point ----------------------------------------------------------------------------

    /// Intercept a key. Returns `true` when Vim consumed it; `false` lets it fall through (so
    /// Insert-mode text still reaches the editor and global chords keep working).
    fn handle_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        if self.state.is_none() {
            return false;
        }
        let mode = self.s().mode;

        // The `:` command line and `/` search line are sub-modes that own the keyboard.
        if self.s().command.is_some() {
            self.command_key(key, host);
            self.publish(host);
            return true;
        }
        if self.s().search.is_some() {
            self.search_key(key, host);
            self.publish(host);
            return true;
        }

        // Record keys for `.` (dot-repeat), except while replaying and for `.` itself.
        let replaying = self.s().replaying;
        let is_dot = mode == Mode::Normal && key.code == KeyCode::Char('.') && self.s().is_idle();
        if !replaying && !is_dot {
            let rev = Self::revision(host);
            self.sm().record_key(key, rev);
        }

        let consumed = match mode {
            Mode::Insert => self.insert_key(key, host),
            Mode::Normal => self.normal_key(key, host),
            Mode::Visual | Mode::VisualLine => self.visual_key(key, host),
        };

        if !self.s().replaying {
            let rev = Self::revision(host);
            self.sm().finalize_recording(rev);
        }
        self.publish(host);
        consumed
    }

    // --- insert mode ----------------------------------------------------------------------------

    fn insert_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
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

    // --- normal mode ----------------------------------------------------------------------------

    fn normal_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
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
                let n = self.s().effective_count() as isize;
                self.move_lines(n, false, host);
                self.sm().count = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let n = self.s().effective_count() as isize;
                self.move_lines(-n, false, host);
                self.sm().count = None;
            }
            _ => {
                let count_opt = self.count_opt();
                if let Some((target, _kind)) = self.motion(code, count_opt, host) {
                    self.caret_move(target, host);
                    self.sm().count = None;
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

    fn handle_prefix(&mut self, prefix: Prefix, key: Key, host: &mut dyn Host) -> bool {
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
        }
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
                    Some(t) => self.motion_result(t, MotionKind::Linewise, host),
                    None => self.sm().clear_pending(),
                }
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

    // --- motion resolution ----------------------------------------------------------------------

    fn count_opt(&self) -> Option<usize> {
        let v = self.s();
        if v.has_count() {
            Some(v.effective_count())
        } else {
            None
        }
    }

    /// Resolve a motion key to `(target_offset, kind)` from the primary caret.
    fn motion(
        &self,
        code: KeyCode,
        count_opt: Option<usize>,
        host: &dyn Host,
    ) -> Option<(usize, MotionKind)> {
        use MotionKind::*;
        let page = host.viewport_height().max(1);
        Self::read(host, |doc| {
            let pos = doc.selections.primary().head;
            let count = count_opt.unwrap_or(1);
            let line = doc.char_to_line(pos);
            let last_line = doc.len_lines().saturating_sub(1);
            let line_start = doc.line_to_char(line);
            let content_end = line_start + doc.line_len_chars(line);
            let result = match code {
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                    (pos.saturating_sub(count).max(line_start), Exclusive)
                }
                KeyCode::Char('l') | KeyCode::Right | KeyCode::Char(' ') => {
                    ((pos + count).min(content_end), Exclusive)
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    (doc.line_to_char((line + count).min(last_line)), Linewise)
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    (doc.line_to_char(line.saturating_sub(count)), Linewise)
                }
                KeyCode::Char('w') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::next_word_start(d, p, false)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('W') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::next_word_start(d, p, true)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('b') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::prev_word_start(d, p, false)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('B') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::prev_word_start(d, p, true)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('e') => (
                    nth(doc, pos, count, |d, p| core_vim::next_word_end(d, p, false)),
                    Inclusive,
                ),
                KeyCode::Char('E') => (
                    nth(doc, pos, count, |d, p| core_vim::next_word_end(d, p, true)),
                    Inclusive,
                ),
                KeyCode::Char('0') | KeyCode::Home => (line_start, Exclusive),
                KeyCode::Char('^') => (core_vim::first_non_blank(doc, line), Exclusive),
                KeyCode::Char('$') | KeyCode::End => {
                    let l = (line + count - 1).min(last_line);
                    let ls_l = doc.line_to_char(l);
                    let ce = ls_l + doc.line_len_chars(l);
                    (ce.saturating_sub(1).max(ls_l), Inclusive)
                }
                KeyCode::Char('|') => {
                    let col = count.saturating_sub(1).min(doc.line_len_chars(line));
                    (line_start + col, Exclusive)
                }
                KeyCode::Char('{') => (
                    nth(doc, pos, count, core_vim::paragraph_backward),
                    Exclusive,
                ),
                KeyCode::Char('}') => {
                    (nth(doc, pos, count, core_vim::paragraph_forward), Exclusive)
                }
                KeyCode::Char('%') => (percent_target(doc)?, Inclusive),
                KeyCode::Char('G') => {
                    let l = match count_opt {
                        Some(c) => c.saturating_sub(1).min(last_line),
                        None => last_line,
                    };
                    (core_vim::first_non_blank(doc, l), Linewise)
                }
                KeyCode::Char('H') => {
                    let l = (doc.view.scroll_line + count.saturating_sub(1)).min(last_line);
                    (core_vim::first_non_blank(doc, l), Linewise)
                }
                KeyCode::Char('M') => {
                    let l = (doc.view.scroll_line + page / 2).min(last_line);
                    (core_vim::first_non_blank(doc, l), Linewise)
                }
                KeyCode::Char('L') => {
                    let bottom = (doc.view.scroll_line + page.saturating_sub(1)).min(last_line);
                    (
                        core_vim::first_non_blank(
                            doc,
                            bottom.saturating_sub(count.saturating_sub(1)),
                        ),
                        Linewise,
                    )
                }
                _ => return None,
            };
            Some(result)
        })
        .flatten()
    }

    fn iter_motion(
        &self,
        step: impl Fn(&Document, usize) -> usize,
        count: usize,
        host: &dyn Host,
    ) -> usize {
        Self::read(host, |doc| {
            let mut p = doc.selections.primary().head;
            for _ in 0..count {
                let np = step(doc, p);
                if np == p {
                    break;
                }
                p = np;
            }
            p
        })
        .unwrap_or(0)
    }

    fn motion_result(&mut self, target: usize, kind: MotionKind, host: &mut dyn Host) {
        match self.s().mode {
            Mode::Visual | Mode::VisualLine => {
                self.visual_set_head(target, host);
                self.sm().clear_pending();
            }
            _ => {
                if let Some(op) = self.s().operator {
                    let from = Self::primary_head(host);
                    self.apply_operator_kind(op, from, target, kind, host);
                } else {
                    self.caret_move(target, host);
                }
                self.sm().clear_pending();
            }
        }
    }

    fn caret_move(&mut self, target: usize, host: &mut dyn Host) {
        Self::caret(host, target);
    }

    fn move_lines(&mut self, delta: isize, extend: bool, host: &mut dyn Host) {
        if let Some(id) = host.active_doc() {
            host.move_lines(id, delta, extend);
        }
    }

    // --- operators ------------------------------------------------------------------------------

    fn apply_operator_kind(
        &mut self,
        op: Operator,
        from: usize,
        to: usize,
        kind: MotionKind,
        host: &mut dyn Host,
    ) {
        let range = Self::read(host, |doc| {
            let (lo, hi) = (from.min(to), from.max(to));
            match kind {
                MotionKind::Exclusive => (lo, hi, false),
                MotionKind::Inclusive => (lo, (hi + 1).min(doc.len_chars()), false),
                MotionKind::Linewise => {
                    let fl = doc.char_to_line(lo);
                    let ll = doc.char_to_line(hi);
                    let s = doc.line_to_char(fl);
                    let e = if ll + 1 < doc.len_lines() {
                        doc.line_to_char(ll + 1)
                    } else {
                        doc.len_chars()
                    };
                    (s, e, true)
                }
            }
        });
        let Some((start, end, linewise)) = range else {
            return;
        };
        self.apply_operator_range(op, start, end, linewise, host);
    }

    fn apply_operator_range(
        &mut self,
        op: Operator,
        start: usize,
        end: usize,
        linewise: bool,
        host: &mut dyn Host,
    ) {
        let reg = self.s().register;
        let text = Self::read(host, |d| {
            let (s, e) = (start.min(d.len_chars()), end.min(d.len_chars()));
            if s < e {
                d.rope().slice(s..e).to_string()
            } else {
                String::new()
            }
        })
        .unwrap_or_default();
        match op {
            Operator::Yank => {
                self.store_register(reg, text, linewise, true, host);
                Self::caret(host, start);
            }
            Operator::Delete => {
                self.store_register(reg, text, linewise, false, host);
                self.delete_range(start, end, linewise, host);
            }
            Operator::Change => {
                self.store_register(reg, text, linewise, false, host);
                if linewise {
                    self.change_lines_content(start, end, host);
                } else {
                    self.delete_range(start, end, false, host);
                }
                self.sm().mode = Mode::Insert;
            }
            Operator::Indent => self.indent_range(start, end, true, host),
            Operator::Outdent => self.indent_range(start, end, false, host),
            Operator::Lower => {
                self.transform_range(start, end, |c| c.to_lowercase().collect(), host)
            }
            Operator::Upper => {
                self.transform_range(start, end, |c| c.to_uppercase().collect(), host)
            }
            Operator::ToggleCase => self.transform_range(start, end, toggle_case, host),
        }
    }

    fn delete_range(&mut self, start: usize, end: usize, linewise: bool, host: &mut dyn Host) {
        if start >= end {
            return;
        }
        // A linewise delete reaching end-of-buffer must also consume the preceding newline.
        let start = if linewise {
            Self::read(host, |d| {
                if end >= d.len_chars() && start > 0 && d.rope().char(start - 1) == '\n' {
                    start - 1
                } else {
                    start
                }
            })
            .unwrap_or(start)
        } else {
            start
        };
        Self::replace(host, start, end, String::new());
        if linewise {
            let target = Self::read(host, |d| {
                let at = d.clamp(start);
                let l = d.char_to_line(at);
                core_vim::first_non_blank(d, l)
            });
            if let Some(t) = target {
                Self::caret(host, t);
            }
        } else {
            Self::caret(host, start);
        }
    }

    fn change_lines_content(&mut self, start: usize, end: usize, host: &mut dyn Host) {
        let range = Self::read(host, |d| {
            let first = d.char_to_line(start);
            let last = d.char_to_line(end.saturating_sub(1).max(start));
            let content_start = d.line_to_char(first);
            let content_end = d.line_to_char(last) + d.line_len_chars(last);
            (content_start, content_end)
        });
        if let Some((cs, ce)) = range {
            Self::replace(host, cs, ce, String::new());
            Self::caret(host, cs);
        }
    }

    fn indent_range(&mut self, start: usize, end: usize, indent: bool, host: &mut dyn Host) {
        let Some(id) = host.active_doc() else {
            return;
        };
        // Build the per-line indent/outdent changes directly (mirrors `edit::indent`/`outdent`).
        let (changes, caret) = match host.workspace().documents.get(id) {
            Some(d) => {
                let fl = d.char_to_line(start);
                let ll = d.char_to_line(end.saturating_sub(1).max(start));
                let width = d.tab_width.max(1);
                let changes: Vec<Change> = (fl..=ll)
                    .filter_map(|l| line_indent_change(d, l, indent, width))
                    .collect();
                (changes, core_vim::first_non_blank(d, fl))
            }
            None => return,
        };
        if !changes.is_empty() {
            host.apply_transaction(id, Transaction::from_changes(changes));
        }
        // Re-resolve first-non-blank after the edit shifted columns.
        let caret = Self::read(host, |d| {
            core_vim::first_non_blank(d, d.char_to_line(d.clamp(caret)))
        })
        .unwrap_or(caret);
        Self::caret(host, caret);
    }

    fn transform_range(
        &mut self,
        start: usize,
        end: usize,
        f: impl Fn(char) -> String,
        host: &mut dyn Host,
    ) {
        let out: String = Self::read(host, |d| {
            let (s, e) = (start.min(d.len_chars()), end.min(d.len_chars()));
            d.rope()
                .slice(s..e)
                .chars()
                .flat_map(|c| f(c).chars().collect::<Vec<_>>())
                .collect()
        })
        .unwrap_or_default();
        if out.is_empty() {
            return;
        }
        let end = Self::read(host, |d| end.min(d.len_chars())).unwrap_or(end);
        Self::replace(host, start, end, out);
        Self::caret(host, start);
    }

    fn linewise_current(&mut self, op: Operator, host: &mut dyn Host) {
        let count = self.s().effective_count();
        let range = Self::read(host, |d| {
            let line = d.char_to_line(d.selections.primary().head);
            let last = (line + count - 1).min(d.len_lines().saturating_sub(1));
            let s = d.line_to_char(line);
            let e = if last + 1 < d.len_lines() {
                d.line_to_char(last + 1)
            } else {
                d.len_chars()
            };
            (s, e)
        });
        if let Some((s, e)) = range {
            self.apply_operator_range(op, s, e, true, host);
        }
    }

    fn apply_text_object(&mut self, obj: TextObject, around: bool, host: &mut dyn Host) {
        let pos = Self::primary_head(host);
        let range = Self::read(host, |d| core_vim::text_object(d, pos, obj, around)).flatten();
        let Some((s, e)) = range else {
            self.sm().clear_pending();
            return;
        };
        let linewise = matches!(obj, TextObject::Paragraph);
        if matches!(self.s().mode, Mode::Visual | Mode::VisualLine) {
            Self::select(host, s, e.saturating_sub(1).max(s));
        } else if let Some(op) = self.s().operator {
            self.apply_operator_range(op, s, e, linewise, host);
        }
        self.sm().clear_pending();
    }

    // --- find-char (f/t/F/T) --------------------------------------------------------------------

    fn find_apply(&mut self, fp: FindPending, ch: char, remember: bool, host: &mut dyn Host) {
        if remember {
            self.sm().last_find = Some((fp, ch));
        }
        let (kind, corekind) = match fp {
            FindPending::Find => (MotionKind::Inclusive, FindKind::Find),
            FindPending::Till => (MotionKind::Inclusive, FindKind::Till),
            FindPending::FindBack => (MotionKind::Exclusive, FindKind::FindBack),
            FindPending::TillBack => (MotionKind::Exclusive, FindKind::TillBack),
        };
        let target = Self::read(host, |d| {
            core_vim::find_char(d, d.selections.primary().head, ch, corekind)
        })
        .flatten();
        match target {
            Some(t) => self.motion_result(t, kind, host),
            None => self.sm().clear_pending(),
        }
    }

    fn repeat_find(&mut self, reverse: bool, host: &mut dyn Host) {
        let Some((fp, ch)) = self.s().last_find else {
            return;
        };
        let fp = if reverse {
            match fp {
                FindPending::Find => FindPending::FindBack,
                FindPending::FindBack => FindPending::Find,
                FindPending::Till => FindPending::TillBack,
                FindPending::TillBack => FindPending::Till,
            }
        } else {
            fp
        };
        self.find_apply(fp, ch, false, host);
    }

    // --- insert-entering commands ---------------------------------------------------------------

    fn enter_insert_at(&mut self, forward: Option<usize>, host: &mut dyn Host) {
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

    fn insert_first_non_blank(&mut self, host: &mut dyn Host) {
        let target = Self::read(host, |d| {
            core_vim::first_non_blank(d, d.char_to_line(d.selections.primary().head))
        });
        if let Some(t) = target {
            Self::caret(host, t);
        }
        self.sm().mode = Mode::Insert;
    }

    fn insert_line_end(&mut self, host: &mut dyn Host) {
        let target = Self::read(host, |d| {
            let line = d.char_to_line(d.selections.primary().head);
            d.line_to_char(line) + d.line_len_chars(line)
        });
        if let Some(t) = target {
            Self::caret(host, t);
        }
        self.sm().mode = Mode::Insert;
    }

    fn insert_col1(&mut self, host: &mut dyn Host) {
        let target = Self::read(host, |d| {
            d.line_to_char(d.char_to_line(d.selections.primary().head))
        });
        if let Some(t) = target {
            Self::caret(host, t);
        }
        self.sm().mode = Mode::Insert;
    }

    fn open_line(&mut self, below: bool, host: &mut dyn Host) {
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

    fn substitute_char(&mut self, host: &mut dyn Host) {
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

    fn change_current_lines(&mut self, host: &mut dyn Host) {
        self.linewise_current(Operator::Change, host);
        self.sm().clear_pending();
    }

    fn change_to_eol(&mut self, host: &mut dyn Host) {
        let (head, end) = self.line_tail_range(host);
        self.apply_operator_range(Operator::Change, head, end, false, host);
        self.sm().clear_pending();
    }

    fn delete_to_eol(&mut self, host: &mut dyn Host) {
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

    // --- single-key edits -----------------------------------------------------------------------

    fn delete_char(&mut self, forward: bool, host: &mut dyn Host) {
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

    fn toggle_case_cmd(&mut self, host: &mut dyn Host) {
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

    fn join_lines(&mut self, host: &mut dyn Host) {
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

    fn replace_char(&mut self, ch: char, host: &mut dyn Host) {
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

    fn paste(&mut self, before: bool, host: &mut dyn Host) {
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

    fn undo(&mut self, host: &mut dyn Host) {
        let count = self.s().effective_count();
        for _ in 0..count {
            host.execute("edit.undo");
        }
        self.sm().clear_pending();
    }

    fn redo(&mut self, host: &mut dyn Host) {
        let count = self.s().effective_count();
        for _ in 0..count {
            host.execute("edit.redo");
        }
        self.sm().clear_pending();
    }

    fn scroll(&mut self, down: bool, half: bool, host: &mut dyn Host) {
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

    fn dot_repeat(&mut self, host: &mut dyn Host) {
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

    // --- registers ------------------------------------------------------------------------------

    fn store_register(
        &mut self,
        reg: Option<char>,
        text: String,
        linewise: bool,
        is_yank: bool,
        host: &mut dyn Host,
    ) {
        match reg {
            Some('_') => {}
            Some('+') | Some('*') => {
                host.clipboard_write(text.clone());
                self.sm().unnamed = Register { text, linewise };
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let lower = c.to_ascii_lowercase();
                if c.is_ascii_uppercase() {
                    let entry = self.sm().registers.entry(lower).or_default();
                    entry.text.push_str(&text);
                    entry.linewise = linewise;
                    let combined = self.s().registers[&lower].clone();
                    self.sm().unnamed = combined;
                } else {
                    self.sm().registers.insert(
                        lower,
                        Register {
                            text: text.clone(),
                            linewise,
                        },
                    );
                    self.sm().unnamed = Register { text, linewise };
                }
            }
            _ => {
                self.sm().unnamed = Register {
                    text: text.clone(),
                    linewise,
                };
                if is_yank {
                    self.sm().yanked = Register { text, linewise };
                }
            }
        }
    }

    fn read_register(&mut self, reg: Option<char>, host: &mut dyn Host) -> Register {
        match reg {
            Some('+') | Some('*') => Register {
                text: host.clipboard_read(),
                linewise: false,
            },
            Some('0') => self.s().yanked.clone(),
            Some('_') => Register::default(),
            Some(c) if c.is_ascii_alphabetic() => self
                .s()
                .registers
                .get(&c.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default(),
            _ => self.s().unnamed.clone(),
        }
    }

    // --- visual mode ----------------------------------------------------------------------------

    fn enter_visual(&mut self, mode: Mode, host: &mut dyn Host) {
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

    fn visual_set_head(&mut self, target: usize, host: &mut dyn Host) {
        let anchor = Self::read(host, |d| d.selections.primary().anchor).unwrap_or(0);
        Self::select(host, anchor, target);
    }

    fn visual_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
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

    fn visual_replace(&mut self, ch: char, host: &mut dyn Host) {
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

    // --- ex command line (`:`) ------------------------------------------------------------------

    fn open_command(&mut self) {
        self.sm().command = Some(String::new());
        self.sm().recording = None;
    }

    fn command_key(&mut self, key: Key, host: &mut dyn Host) {
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

    // --- search line (`/`, `?`) -----------------------------------------------------------------

    fn open_search(&mut self, forward: bool) {
        self.sm().search = Some((forward, String::new()));
        self.sm().recording = None;
    }

    fn search_key(&mut self, key: Key, host: &mut dyn Host) {
        match key.code {
            KeyCode::Esc => self.sm().search = None,
            KeyCode::Enter => {
                let (fwd, pat) = self.sm().search.take().unwrap_or((true, String::new()));
                if !pat.is_empty() {
                    self.sm().last_search = Some((fwd, pat.clone()));
                    self.do_search(fwd, &pat, true, host);
                }
            }
            KeyCode::Backspace => {
                let empty = {
                    let (_, buf) = self.sm().search.as_mut().unwrap();
                    buf.pop();
                    buf.is_empty()
                };
                if empty {
                    self.sm().search = None;
                }
            }
            KeyCode::Char(c) if !key.ctrl => {
                if let Some((_, buf)) = self.sm().search.as_mut() {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn do_search(&mut self, forward: bool, pat: &str, from_next: bool, host: &mut dyn Host) {
        let found = Self::read(host, |d| {
            let chars: Vec<char> = d.rope().chars().collect();
            let pat: Vec<char> = pat.chars().collect();
            let head = d.selections.primary().head;
            let start = if from_next {
                if forward {
                    head + 1
                } else {
                    head.saturating_sub(1)
                }
            } else {
                head
            };
            search_literal(&chars, &pat, start, forward)
        })
        .flatten();
        if let Some(pos) = found {
            Self::caret(host, pos);
        } else {
            host.notify(format!("Pattern not found: {pat}"));
        }
    }

    fn search_next(&mut self, reverse: bool, host: &mut dyn Host) {
        let Some((fwd, pat)) = self.s().last_search.clone() else {
            return;
        };
        let dir = if reverse { !fwd } else { fwd };
        self.do_search(dir, &pat, true, host);
    }

    fn search_word(&mut self, forward: bool, host: &mut dyn Host) {
        let word = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let (s, e) = motion::word_at(d, head);
            d.rope().slice(s..e).to_string()
        });
        if let Some(word) = word {
            if !word.trim().is_empty() {
                self.sm().last_search = Some((forward, word.clone()));
                self.do_search(forward, &word, true, host);
            }
        }
    }
}

/// `%`: the partner of the bracket under the cursor, or of the next bracket on the line.
fn percent_target(doc: &Document) -> Option<usize> {
    let pos = doc.selections.primary().head;
    if let Some(m) = motion::matching_bracket(doc, pos) {
        return Some(m);
    }
    let line = doc.char_to_line(pos);
    let end = doc.line_to_char(line) + doc.line_len_chars(line);
    for i in pos..end {
        if matches!(doc.rope().char(i), '(' | ')' | '[' | ']' | '{' | '}') {
            return motion::matching_bracket(doc, i);
        }
    }
    None
}

/// Cap for `Change.at` so a bad range can't index past the buffer end.
fn removed_end_cap(host: &dyn Host, id: DocId) -> usize {
    host.workspace()
        .documents
        .get(id)
        .map(|d| d.len_chars())
        .unwrap_or(usize::MAX)
}

impl Plugin for VimPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("vim.toggle", "Vim: Toggle Vim Mode")
            .command("vim.enable", "Vim: Enable Vim Mode")
            .command("vim.disable", "Vim: Disable Vim Mode")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "vim.enable" => self.set_enabled(true, host),
            "vim.disable" => self.set_enabled(false, host),
            "vim.toggle" => self.set_enabled(self.state.is_none(), host),
            _ => return false,
        }
        true
    }

    fn capture_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        self.handle_key(key, host)
    }
}
