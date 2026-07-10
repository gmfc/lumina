//! The Vim modal state machine: how each key is interpreted per mode.
//!
//! These are `impl App` blocks (they need the whole `App` — documents, clipboard,
//! command dispatch), split out from [`crate::app`] by concern like the other key
//! handlers. The state they read/write lives in [`crate::vim::VimState`], hung off
//! `EditorState`. All buffer changes go through `editor_core::edit` (invariant #1);
//! the pure motion/text-object math is `editor_core::vim` (invariant #5).

use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use editor_core::vim as core_vim;
use editor_core::vim::{FindKind, TextObject};
use editor_core::Motion;

use crate::vim::{FindPending, Mode, MotionKind, Operator, Prefix, Register, VimState};

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

impl App {
    // --- accessors -------------------------------------------------------

    fn vim(&self) -> &VimState {
        self.editor.vim.as_ref().expect("vim layer enabled")
    }

    fn vim_mut(&mut self) -> &mut VimState {
        self.editor.vim.as_mut().expect("vim layer enabled")
    }

    fn current_revision(&self) -> u64 {
        self.editor
            .active_document()
            .map(|d| d.revision)
            .unwrap_or(0)
    }

    fn primary_head(&self) -> usize {
        self.editor
            .active_document()
            .map(|d| d.selections.primary().head)
            .unwrap_or(0)
    }

    /// The typed count as an option (`None` when the user typed no count).
    fn vim_count_opt(&self) -> Option<usize> {
        let v = self.vim();
        if v.has_count() {
            Some(v.effective_count())
        } else {
            None
        }
    }

    /// Turn the Vim layer on or off (the `vim.toggle`/`vim.enable`/`vim.disable`
    /// commands). Enabling starts in Normal mode; disabling drops the state.
    pub(super) fn set_vim(&mut self, on: bool) {
        if on {
            if self.editor.vim.is_none() {
                self.editor.vim = Some(VimState::new());
            }
            self.editor.status_message = Some("Vim mode enabled".into());
        } else {
            self.editor.vim = None;
            self.editor.status_message = Some("Vim mode disabled".into());
        }
    }

    // --- entry point -----------------------------------------------------

    /// Intercept a key for the Vim layer. Returns `true` when Vim consumed it;
    /// `false` lets it fall through to the normal chord keymap (so global shortcuts
    /// like Ctrl+S keep working, and Insert-mode text still reaches the editor).
    pub(super) fn handle_vim_key(&mut self, key: KeyEvent) -> bool {
        if self.editor.vim.is_none() || self.editor.focus != Focus::Editor || self.settings_active()
        {
            return false;
        }
        let mode = self.vim().mode;

        // The `:` command line and `/` search line are sub-modes that own the keyboard.
        if self.vim().command.is_some() {
            self.vim_command_key(key);
            return true;
        }
        if self.vim().search.is_some() {
            self.vim_search_key(key);
            return true;
        }

        // Record keys for `.` (dot-repeat), except while replaying and for `.` itself.
        let replaying = self.vim().replaying;
        let is_dot =
            mode == Mode::Normal && matches!(key.code, KeyCode::Char('.')) && self.vim().is_idle();
        if !replaying && !is_dot {
            let rev = self.current_revision();
            self.vim_mut().record_key(key, rev);
        }

        let consumed = match mode {
            Mode::Insert => self.vim_insert_key(key),
            Mode::Normal => self.vim_normal_key(key),
            Mode::Visual | Mode::VisualLine => self.vim_visual_key(key),
        };

        if !self.vim().replaying {
            let rev = self.current_revision();
            self.vim_mut().finalize_recording(rev);
        }
        consumed
    }

    // --- insert mode -----------------------------------------------------

    /// Insert mode: only `Esc`/`Ctrl-[` are special (leave to Normal). Everything
    /// else falls through so normal text entry, auto-pairs, and Ctrl chords work.
    fn vim_insert_key(&mut self, key: KeyEvent) -> bool {
        let esc = key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('[') && key.modifiers.contains(KeyModifiers::CONTROL));
        if esc {
            self.vim_leave_insert();
            return true;
        }
        false
    }

    fn vim_leave_insert(&mut self) {
        self.vim_mut().mode = Mode::Normal;
        // Vim steps the cursor one left when leaving insert (clamped to line start).
        self.with_doc(|d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let ls = d.line_to_char(line);
            d.set_caret(head.saturating_sub(1).max(ls));
        });
    }

    // --- normal mode -----------------------------------------------------

    fn vim_normal_key(&mut self, key: KeyEvent) -> bool {
        let code = key.code;

        // Esc clears any pending state.
        if code == KeyCode::Esc {
            self.vim_mut().clear_pending();
            return true;
        }

        // Resolve a pending single-char argument (f/t/F/T target).
        if let Some(fp) = self.vim().find_pending {
            self.vim_mut().find_pending = None;
            if let KeyCode::Char(c) = code {
                self.vim_find_apply(fp, c, true);
            } else {
                self.vim_mut().clear_pending();
            }
            return true;
        }

        // Resolve a multi-key prefix (register / replace / text object / g / z).
        if let Some(prefix) = self.vim().prefix {
            return self.vim_handle_prefix(prefix, key);
        }

        // Ctrl chords: Vim owns a few; the rest fall through to the global keymap.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match code {
                KeyCode::Char('r') => {
                    self.vim_redo();
                    true
                }
                KeyCode::Char('d') => {
                    self.vim_scroll(true, true);
                    true
                }
                KeyCode::Char('u') => {
                    self.vim_scroll(false, true);
                    true
                }
                KeyCode::Char('f') => {
                    self.vim_scroll(true, false);
                    true
                }
                KeyCode::Char('b') => {
                    self.vim_scroll(false, false);
                    true
                }
                _ => false,
            };
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return false;
        }

        if self.vim().operator.is_some() {
            self.vim_operator_pending(key)
        } else {
            self.vim_normal_idle(key)
        }
    }

    /// Normal mode with no operator pending.
    fn vim_normal_idle(&mut self, key: KeyEvent) -> bool {
        let code = key.code;
        match code {
            // counts
            KeyCode::Char(c @ '1'..='9') => {
                self.vim_mut().push_digit(c as usize - '0' as usize);
            }
            KeyCode::Char('0') if self.vim().count_active() => {
                self.vim_mut().push_digit(0);
            }
            // register select
            KeyCode::Char('"') => self.vim_mut().prefix = Some(Prefix::Register),
            // operators
            KeyCode::Char('d') => self.vim_mut().operator = Some(Operator::Delete),
            KeyCode::Char('c') => self.vim_mut().operator = Some(Operator::Change),
            KeyCode::Char('y') => self.vim_mut().operator = Some(Operator::Yank),
            KeyCode::Char('>') => self.vim_mut().operator = Some(Operator::Indent),
            KeyCode::Char('<') => self.vim_mut().operator = Some(Operator::Outdent),
            // prefixes
            KeyCode::Char('g') => self.vim_mut().prefix = Some(Prefix::G),
            KeyCode::Char('z') => self.vim_mut().prefix = Some(Prefix::Z),
            KeyCode::Char('r') => self.vim_mut().prefix = Some(Prefix::Replace),
            // find-char
            KeyCode::Char('f') => self.vim_mut().find_pending = Some(FindPending::Find),
            KeyCode::Char('F') => self.vim_mut().find_pending = Some(FindPending::FindBack),
            KeyCode::Char('t') => self.vim_mut().find_pending = Some(FindPending::Till),
            KeyCode::Char('T') => self.vim_mut().find_pending = Some(FindPending::TillBack),
            KeyCode::Char(';') => self.vim_repeat_find(false),
            KeyCode::Char(',') => self.vim_repeat_find(true),
            // insert-entering
            KeyCode::Char('i') => self.vim_enter_insert_at(None),
            KeyCode::Char('a') => self.vim_enter_insert_at(Some(1)),
            KeyCode::Char('I') => self.vim_insert_first_non_blank(),
            KeyCode::Char('A') => self.vim_insert_line_end(),
            KeyCode::Char('o') => self.vim_open_line(true),
            KeyCode::Char('O') => self.vim_open_line(false),
            KeyCode::Char('s') => self.vim_substitute_char(),
            KeyCode::Char('S') => self.vim_change_current_lines(),
            KeyCode::Char('C') => self.vim_change_to_eol(),
            KeyCode::Char('D') => self.vim_delete_to_eol(),
            // simple edits
            KeyCode::Char('x') => self.vim_delete_char(true),
            KeyCode::Char('X') => self.vim_delete_char(false),
            KeyCode::Char('~') => self.vim_toggle_case(),
            KeyCode::Char('J') => self.vim_join_lines(),
            KeyCode::Char('p') => self.vim_paste(false),
            KeyCode::Char('P') => self.vim_paste(true),
            KeyCode::Char('u') => self.vim_undo(),
            KeyCode::Char('.') => self.vim_dot_repeat(),
            // mode switches
            KeyCode::Char('v') => self.vim_enter_visual(Mode::Visual),
            KeyCode::Char('V') => self.vim_enter_visual(Mode::VisualLine),
            KeyCode::Char(':') => self.vim_open_command(),
            KeyCode::Char('/') => self.vim_open_search(true),
            KeyCode::Char('?') => self.vim_open_search(false),
            KeyCode::Char('n') => self.vim_search_next(false),
            KeyCode::Char('N') => self.vim_search_next(true),
            KeyCode::Char('*') => self.vim_search_word(true),
            KeyCode::Char('#') => self.vim_search_word(false),
            // vertical movement (goal-column aware)
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => {
                let n = self.vim().effective_count() as isize;
                self.vim_move_lines(n);
                self.vim_mut().count = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let n = self.vim().effective_count() as isize;
                self.vim_move_lines(-n);
                self.vim_mut().count = None;
            }
            // every other motion: move the caret
            _ => {
                if let Some((target, kind)) = self.vim_motion(code, self.vim_count_opt()) {
                    self.vim_move_to(target, kind);
                    self.vim_mut().count = None;
                } else {
                    self.vim_mut().clear_pending();
                }
            }
        }
        true
    }

    /// Normal mode with an operator pending — the next key is a motion, text object,
    /// or the operator key again (linewise).
    fn vim_operator_pending(&mut self, key: KeyEvent) -> bool {
        let code = key.code;
        let op = self.vim().operator.expect("operator pending");
        if let KeyCode::Char(c) = code {
            // count (after operator). A leading `0` is the motion, not a count.
            if c.is_ascii_digit() && (c != '0' || self.vim().count_active()) {
                self.vim_mut().push_digit(c as usize - '0' as usize);
                return true;
            }
            if c == '"' {
                self.vim_mut().prefix = Some(Prefix::Register);
                return true;
            }
            if c == double_key(op) {
                self.vim_linewise_current(op);
                self.vim_mut().clear_pending();
                return true;
            }
            match c {
                'i' => {
                    self.vim_mut().prefix = Some(Prefix::Object { around: false });
                    return true;
                }
                'a' => {
                    self.vim_mut().prefix = Some(Prefix::Object { around: true });
                    return true;
                }
                'g' => {
                    self.vim_mut().prefix = Some(Prefix::G);
                    return true;
                }
                'f' => {
                    self.vim_mut().find_pending = Some(FindPending::Find);
                    return true;
                }
                'F' => {
                    self.vim_mut().find_pending = Some(FindPending::FindBack);
                    return true;
                }
                't' => {
                    self.vim_mut().find_pending = Some(FindPending::Till);
                    return true;
                }
                'T' => {
                    self.vim_mut().find_pending = Some(FindPending::TillBack);
                    return true;
                }
                ';' => {
                    self.vim_repeat_find(false);
                    return true;
                }
                ',' => {
                    self.vim_repeat_find(true);
                    return true;
                }
                _ => {}
            }
        }
        if let Some((target, kind)) = self.vim_motion(code, self.vim_count_opt()) {
            let from = self.primary_head();
            self.vim_apply_operator_kind(op, from, target, kind);
            self.vim_mut().clear_pending();
        } else {
            self.vim_mut().clear_pending();
        }
        true
    }

    /// Resolve a multi-key prefix.
    fn vim_handle_prefix(&mut self, prefix: Prefix, key: KeyEvent) -> bool {
        self.vim_mut().prefix = None;
        match prefix {
            Prefix::Register => {
                if let KeyCode::Char(c) = key.code {
                    self.vim_mut().register = Some(c);
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
                    Some(c) if matches!(self.vim().mode, Mode::Visual | Mode::VisualLine) => {
                        self.vim_visual_replace(c)
                    }
                    Some(c) => self.vim_replace_char(c),
                    None => self.vim_mut().clear_pending(),
                }
                true
            }
            Prefix::Object { around } => {
                if let Some(obj) = object_from_key(key.code) {
                    self.vim_apply_text_object(obj, around);
                } else {
                    self.vim_mut().clear_pending();
                }
                true
            }
            Prefix::G => self.vim_g_prefix(key),
            Prefix::Z => {
                self.vim_z_prefix(key);
                true
            }
        }
    }

    /// The `g…` family: `gg`, `ge`/`gE`, `g_`, `gu`/`gU`/`g~`, `gI`.
    fn vim_g_prefix(&mut self, key: KeyEvent) -> bool {
        let op = self.vim().operator;
        let count_opt = self.vim_count_opt();
        match key.code {
            KeyCode::Char('g') => {
                // gg: go to line `count` (or the first line), linewise.
                let target = {
                    let Some(doc) = self.editor.active_document() else {
                        self.vim_mut().clear_pending();
                        return true;
                    };
                    let l = match count_opt {
                        Some(c) => c.saturating_sub(1).min(doc.len_lines().saturating_sub(1)),
                        None => 0,
                    };
                    core_vim::first_non_blank(doc, l)
                };
                self.vim_motion_result(target, MotionKind::Linewise);
            }
            KeyCode::Char('e') => {
                let count = count_opt.unwrap_or(1);
                let target = self.iter_motion(|d, p| core_vim::prev_word_end(d, p, false), count);
                self.vim_motion_result(target, MotionKind::Inclusive);
            }
            KeyCode::Char('E') => {
                let count = count_opt.unwrap_or(1);
                let target = self.iter_motion(|d, p| core_vim::prev_word_end(d, p, true), count);
                self.vim_motion_result(target, MotionKind::Inclusive);
            }
            KeyCode::Char('_') => {
                let target = {
                    let Some(doc) = self.editor.active_document() else {
                        self.vim_mut().clear_pending();
                        return true;
                    };
                    core_vim::last_non_blank(doc, doc.selections.primary().head)
                };
                self.vim_motion_result(target, MotionKind::Inclusive);
            }
            KeyCode::Char('u') if op.is_none() => self.vim_mut().operator = Some(Operator::Lower),
            KeyCode::Char('U') if op.is_none() => self.vim_mut().operator = Some(Operator::Upper),
            KeyCode::Char('~') if op.is_none() => {
                self.vim_mut().operator = Some(Operator::ToggleCase)
            }
            KeyCode::Char('I') if op.is_none() => self.vim_insert_col1(),
            _ => self.vim_mut().clear_pending(),
        }
        true
    }

    /// The `z…` family: recentre the viewport.
    fn vim_z_prefix(&mut self, key: KeyEvent) {
        let height = self.page_height.max(1);
        let (line, n_lines) = match self.editor.active_document() {
            Some(d) => (d.selections.primary().head, d.len_lines()),
            None => return,
        };
        let cur_line = self
            .editor
            .active_document()
            .map(|d| d.char_to_line(line))
            .unwrap_or(0);
        let scroll = match key.code {
            KeyCode::Char('z') | KeyCode::Char('.') => cur_line.saturating_sub(height / 2),
            KeyCode::Char('t') | KeyCode::Enter => cur_line,
            KeyCode::Char('b') => cur_line.saturating_sub(height.saturating_sub(1)),
            _ => return,
        };
        self.with_doc(|d| d.view.scroll_line = scroll.min(n_lines.saturating_sub(1)));
        self.vim_mut().clear_pending();
    }

    // --- motion resolution ----------------------------------------------

    /// Resolve a motion key to `(target_offset, kind)` from the primary caret.
    /// Returns `None` when `code` isn't a motion. `count_opt` distinguishes an
    /// explicit count from the default 1 (needed by `G`).
    fn vim_motion(&self, code: KeyCode, count_opt: Option<usize>) -> Option<(usize, MotionKind)> {
        use MotionKind::*;
        let doc = self.editor.active_document()?;
        let pos = doc.selections.primary().head;
        let page = self.page_height.max(1);
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
                // Inclusive: the caret rests on the last char, and `d$` deletes
                // through it (its `+1` lands at the newline, which stays).
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
            KeyCode::Char('}') => (nth(doc, pos, count, core_vim::paragraph_forward), Exclusive),
            KeyCode::Char('%') => (self.vim_percent_target()?, Inclusive),
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
                    core_vim::first_non_blank(doc, bottom.saturating_sub(count.saturating_sub(1))),
                    Linewise,
                )
            }
            _ => return None,
        };
        Some(result)
    }

    /// Iterate a step function `count` times over the active document (for `g`-prefixed
    /// motions that need the current doc but aren't in [`Self::vim_motion`]).
    fn iter_motion(&self, step: impl Fn(&Document, usize) -> usize, count: usize) -> usize {
        let Some(doc) = self.editor.active_document() else {
            return 0;
        };
        let mut p = doc.selections.primary().head;
        for _ in 0..count {
            let np = step(doc, p);
            if np == p {
                break;
            }
            p = np;
        }
        p
    }

    /// `%`: the partner of the bracket under the cursor, or of the next bracket on
    /// the line if the cursor isn't on one.
    fn vim_percent_target(&self) -> Option<usize> {
        let doc = self.editor.active_document()?;
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

    /// Apply a resolved motion in the current mode: operate (operator pending),
    /// extend (visual), or move the caret.
    fn vim_motion_result(&mut self, target: usize, kind: MotionKind) {
        match self.vim().mode {
            Mode::Visual | Mode::VisualLine => {
                self.vim_visual_set_head(target);
                self.vim_mut().clear_pending();
            }
            _ => {
                if let Some(op) = self.vim().operator {
                    let from = self.primary_head();
                    self.vim_apply_operator_kind(op, from, target, kind);
                }
                self.vim_move_to_if_no_op(target, kind);
                self.vim_mut().clear_pending();
            }
        }
    }

    fn vim_move_to_if_no_op(&mut self, target: usize, kind: MotionKind) {
        if self.vim().operator.is_none() {
            self.vim_move_to(target, kind);
        }
    }

    /// Pure caret movement to a motion target.
    fn vim_move_to(&mut self, target: usize, _kind: MotionKind) {
        self.with_doc(|d| {
            let p = d.clamp(target);
            d.set_caret(p);
        });
    }

    fn vim_move_lines(&mut self, delta: isize) {
        let n = delta.unsigned_abs();
        let m = if delta < 0 { Motion::Up } else { Motion::Down };
        let page = self.page_height;
        self.with_doc(|d| {
            for _ in 0..n {
                edit::move_selections(d, m, page, false);
            }
        });
    }

    // --- operators -------------------------------------------------------

    /// Apply `op` over the range spanned by `[from, to]` with `kind`'s inclusivity.
    fn vim_apply_operator_kind(&mut self, op: Operator, from: usize, to: usize, kind: MotionKind) {
        let Some(doc) = self.editor.active_document() else {
            return;
        };
        let (lo, hi) = (from.min(to), from.max(to));
        let (start, end, linewise) = match kind {
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
        };
        self.vim_apply_operator_range(op, start, end, linewise);
    }

    /// Apply `op` over an explicit `[start, end)` char range.
    fn vim_apply_operator_range(&mut self, op: Operator, start: usize, end: usize, linewise: bool) {
        let reg = self.vim().register;
        let text = match self.editor.active_document() {
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
        match op {
            Operator::Yank => {
                self.vim_store_register(reg, text, linewise, true);
                self.with_doc(|d| {
                    let p = d.clamp(start);
                    d.set_caret(p);
                });
            }
            Operator::Delete => {
                self.vim_store_register(reg, text, linewise, false);
                self.vim_delete_range(start, end, linewise);
            }
            Operator::Change => {
                self.vim_store_register(reg, text, linewise, false);
                if linewise {
                    self.vim_change_lines_content(start, end);
                } else {
                    self.vim_delete_range(start, end, false);
                }
                self.vim_mut().mode = Mode::Insert;
            }
            Operator::Indent => self.vim_indent_range(start, end, true),
            Operator::Outdent => self.vim_indent_range(start, end, false),
            Operator::Lower => self.vim_transform_range(start, end, |c| c.to_lowercase().collect()),
            Operator::Upper => self.vim_transform_range(start, end, |c| c.to_uppercase().collect()),
            Operator::ToggleCase => self.vim_transform_range(start, end, toggle_case),
        }
    }

    fn vim_delete_range(&mut self, start: usize, end: usize, linewise: bool) {
        if start >= end {
            return;
        }
        // A linewise delete that reaches end-of-buffer must also consume the
        // preceding newline, so deleting the last line leaves no empty line behind
        // (matching Vim's `dd`/`dG`).
        let start = if linewise {
            self.editor
                .active_document()
                .map(|d| {
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
        self.with_doc(|d| {
            d.selections.set_single(Selection::new(start, end));
            edit::edit_selections(
                d,
                |_x, s| (s.span(), String::new()),
                editor_core::GroupBreak::Force,
            );
        });
        if linewise {
            self.with_doc(|d| {
                let at = d.clamp(start);
                let l = d.char_to_line(at);
                let p = core_vim::first_non_blank(d, l);
                d.set_caret(p);
            });
        }
    }

    /// Linewise change (`cc`, `S`): blank the lines' content but keep one line to
    /// type into.
    fn vim_change_lines_content(&mut self, start: usize, end: usize) {
        self.with_doc(|d| {
            let first = d.char_to_line(start);
            let last = d.char_to_line(end.saturating_sub(1).max(start));
            let content_start = d.line_to_char(first);
            let content_end = d.line_to_char(last) + d.line_len_chars(last);
            d.selections
                .set_single(Selection::new(content_start, content_end));
            edit::edit_selections(
                d,
                |_x, s| (s.span(), String::new()),
                editor_core::GroupBreak::Force,
            );
        });
    }

    fn vim_indent_range(&mut self, start: usize, end: usize, indent: bool) {
        self.with_doc(|d| {
            let fl = d.char_to_line(start);
            let ll = d.char_to_line(end.saturating_sub(1).max(start));
            let s = d.line_to_char(fl);
            let e = d.line_to_char(ll) + d.line_len_chars(ll);
            d.selections.set_single(Selection::new(s, e));
            if indent {
                edit::indent(d);
            } else {
                edit::outdent(d);
            }
            let p = core_vim::first_non_blank(d, fl);
            d.set_caret(p);
        });
    }

    fn vim_transform_range(&mut self, start: usize, end: usize, f: impl Fn(char) -> String) {
        let out: String = match self.editor.active_document() {
            Some(d) => {
                let (s, e) = (start.min(d.len_chars()), end.min(d.len_chars()));
                d.rope()
                    .slice(s..e)
                    .chars()
                    .flat_map(|c| f(c).chars().collect::<Vec<_>>())
                    .collect()
            }
            None => return,
        };
        if out.is_empty() {
            return;
        }
        self.with_doc(|d| {
            d.selections
                .set_single(Selection::new(start, end.min(d.len_chars())));
            edit::edit_selections(
                d,
                |_x, s| (s.span(), out.clone()),
                editor_core::GroupBreak::Force,
            );
            let p = d.clamp(start);
            d.set_caret(p);
        });
    }

    /// Operate on the current line(s) (`dd`, `yy`, `>>`, …).
    fn vim_linewise_current(&mut self, op: Operator) {
        let count = self.vim().effective_count();
        let (start, end) = match self.editor.active_document() {
            Some(d) => {
                let line = d.char_to_line(d.selections.primary().head);
                let last = (line + count - 1).min(d.len_lines().saturating_sub(1));
                let s = d.line_to_char(line);
                let e = if last + 1 < d.len_lines() {
                    d.line_to_char(last + 1)
                } else {
                    d.len_chars()
                };
                (s, e)
            }
            None => return,
        };
        self.vim_apply_operator_range(op, start, end, true);
    }

    /// Apply an operator to a resolved text object (or select it in visual mode).
    fn vim_apply_text_object(&mut self, obj: TextObject, around: bool) {
        let pos = self.primary_head();
        let range = self
            .editor
            .active_document()
            .and_then(|d| core_vim::text_object(d, pos, obj, around));
        let Some((s, e)) = range else {
            self.vim_mut().clear_pending();
            return;
        };
        let linewise = matches!(obj, TextObject::Paragraph);
        if matches!(self.vim().mode, Mode::Visual | Mode::VisualLine) {
            self.with_doc(|d| {
                d.selections
                    .set_single(Selection::new(s, e.saturating_sub(1).max(s)));
            });
        } else if let Some(op) = self.vim().operator {
            self.vim_apply_operator_range(op, s, e, linewise);
        }
        self.vim_mut().clear_pending();
    }

    // --- find-char (f/t/F/T) --------------------------------------------

    fn vim_find_apply(&mut self, fp: FindPending, ch: char, remember: bool) {
        if remember {
            self.vim_mut().last_find = Some((fp, ch));
        }
        let (kind, corekind) = match fp {
            FindPending::Find => (MotionKind::Inclusive, FindKind::Find),
            FindPending::Till => (MotionKind::Inclusive, FindKind::Till),
            FindPending::FindBack => (MotionKind::Exclusive, FindKind::FindBack),
            FindPending::TillBack => (MotionKind::Exclusive, FindKind::TillBack),
        };
        let target = self
            .editor
            .active_document()
            .and_then(|d| core_vim::find_char(d, d.selections.primary().head, ch, corekind));
        match target {
            Some(t) => self.vim_motion_result(t, kind),
            None => self.vim_mut().clear_pending(),
        }
    }

    fn vim_repeat_find(&mut self, reverse: bool) {
        let Some((fp, ch)) = self.vim().last_find else {
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
        self.vim_find_apply(fp, ch, false);
    }

    // --- insert-entering commands ---------------------------------------

    /// `i` (offset None) or `a` (offset Some(1)): enter insert at/after the caret.
    fn vim_enter_insert_at(&mut self, forward: Option<usize>) {
        if let Some(n) = forward {
            self.with_doc(|d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let end = d.line_to_char(line) + d.line_len_chars(line);
                d.set_caret((head + n).min(end));
            });
        }
        self.vim_mut().mode = Mode::Insert;
    }

    fn vim_insert_first_non_blank(&mut self) {
        self.with_doc(|d| {
            let line = d.char_to_line(d.selections.primary().head);
            let p = core_vim::first_non_blank(d, line);
            d.set_caret(p);
        });
        self.vim_mut().mode = Mode::Insert;
    }

    fn vim_insert_line_end(&mut self) {
        self.with_doc(|d| {
            let line = d.char_to_line(d.selections.primary().head);
            let p = d.line_to_char(line) + d.line_len_chars(line);
            d.set_caret(p);
        });
        self.vim_mut().mode = Mode::Insert;
    }

    fn vim_insert_col1(&mut self) {
        self.with_doc(|d| {
            let line = d.char_to_line(d.selections.primary().head);
            d.set_caret(d.line_to_char(line));
        });
        self.vim_mut().mode = Mode::Insert;
    }

    fn vim_open_line(&mut self, below: bool) {
        if below {
            self.with_doc(edit::insert_line_below);
        } else {
            self.with_doc(edit::insert_line_above);
        }
        self.vim_mut().mode = Mode::Insert;
    }

    fn vim_substitute_char(&mut self) {
        let count = self.vim().effective_count();
        let reg = self.vim().register;
        let (start, end, text) = match self.editor.active_document() {
            Some(d) => {
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
            }
            None => return,
        };
        if start < end {
            self.vim_store_register(reg, text, false, false);
            self.vim_delete_range(start, end, false);
        }
        self.vim_mut().mode = Mode::Insert;
        self.vim_mut().clear_pending();
    }

    fn vim_change_current_lines(&mut self) {
        self.vim_linewise_current(Operator::Change);
        self.vim_mut().clear_pending();
    }

    fn vim_change_to_eol(&mut self) {
        let (head, end) = self.line_tail_range();
        self.vim_apply_operator_range(Operator::Change, head, end, false);
        self.vim_mut().clear_pending();
    }

    fn vim_delete_to_eol(&mut self) {
        let (head, end) = self.line_tail_range();
        self.vim_apply_operator_range(Operator::Delete, head, end, false);
        self.vim_mut().clear_pending();
    }

    fn line_tail_range(&self) -> (usize, usize) {
        match self.editor.active_document() {
            Some(d) => {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let end = d.line_to_char(line) + d.line_len_chars(line);
                (head, end)
            }
            None => (0, 0),
        }
    }

    // --- single-key edits ------------------------------------------------

    /// `x` (forward == true) or `X` (backward).
    fn vim_delete_char(&mut self, forward: bool) {
        let count = self.vim().effective_count();
        let reg = self.vim().register;
        let (start, end) = match self.editor.active_document() {
            Some(d) => {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let ls = d.line_to_char(line);
                let content_end = ls + d.line_len_chars(line);
                if forward {
                    (head, (head + count).min(content_end))
                } else {
                    (head.saturating_sub(count).max(ls), head)
                }
            }
            None => return,
        };
        if start < end {
            let text = self
                .editor
                .active_document()
                .map(|d| d.rope().slice(start..end).to_string())
                .unwrap_or_default();
            self.vim_store_register(reg, text, false, false);
            self.vim_delete_range(start, end, false);
            self.vim_clamp_caret();
        }
        self.vim_mut().clear_pending();
    }

    fn vim_toggle_case(&mut self) {
        let count = self.vim().effective_count();
        let (start, end) = match self.editor.active_document() {
            Some(d) => {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let content_end = d.line_to_char(line) + d.line_len_chars(line);
                (head, (head + count).min(content_end))
            }
            None => return,
        };
        if start < end {
            self.vim_transform_range(start, end, toggle_case);
            self.with_doc(|d| {
                let line = d.char_to_line(end);
                let content_end = d.line_to_char(line) + d.line_len_chars(line);
                d.set_caret(end.min(content_end));
            });
        }
        self.vim_mut().clear_pending();
    }

    fn vim_join_lines(&mut self) {
        let count = self.vim().effective_count().max(2);
        let joins = count - 1;
        for _ in 0..joins {
            let done = self.with_doc_ret(|d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                if line + 1 >= d.len_lines() {
                    return true;
                }
                let end_of_line = d.line_to_char(line) + d.line_len_chars(line);
                let next_first = core_vim::first_non_blank(d, line + 1);
                let empty_line = d.line_len_chars(line) == 0;
                let repl = if empty_line { "" } else { " " };
                d.selections.set_single(Selection::caret(end_of_line));
                edit::edit_selections(
                    d,
                    |_x, _s| (end_of_line..next_first, repl.to_string()),
                    editor_core::GroupBreak::Force,
                );
                d.set_caret(end_of_line);
                false
            });
            if done.unwrap_or(true) {
                break;
            }
        }
        self.vim_mut().clear_pending();
    }

    fn vim_replace_char(&mut self, ch: char) {
        let count = self.vim().effective_count();
        let (start, end) = match self.editor.active_document() {
            Some(d) => {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let content_end = d.line_to_char(line) + d.line_len_chars(line);
                (head, (head + count).min(content_end))
            }
            None => return,
        };
        if start >= end {
            self.vim_mut().clear_pending();
            return;
        }
        let repl = if ch == '\n' {
            "\n".to_string()
        } else {
            ch.to_string().repeat(end - start)
        };
        self.with_doc(|d| {
            d.selections.set_single(Selection::new(start, end));
            edit::edit_selections(
                d,
                |_x, s| (s.span(), repl.clone()),
                editor_core::GroupBreak::Force,
            );
        });
        if ch != '\n' {
            self.with_doc(|d| d.set_caret(end.saturating_sub(1).max(start)));
        }
        self.vim_mut().clear_pending();
    }

    fn vim_paste(&mut self, before: bool) {
        let count = self.vim().effective_count();
        let reg = self.vim().register;
        let data = self.vim_read_register(reg);
        if data.text.is_empty() {
            self.vim_mut().clear_pending();
            return;
        }
        if data.linewise {
            let body = data.text.trim_end_matches('\n').to_string();
            let block = std::iter::repeat_n(body.as_str(), count)
                .collect::<Vec<_>>()
                .join("\n");
            self.with_doc(|d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                if before {
                    let at = d.line_to_char(line);
                    d.set_caret(at);
                    edit::insert_text(d, &format!("{block}\n"), editor_core::GroupBreak::Force);
                    let p = core_vim::first_non_blank(d, line);
                    d.set_caret(p);
                } else {
                    let at = d.line_to_char(line) + d.line_len_chars(line);
                    d.set_caret(at);
                    edit::insert_text(d, &format!("\n{block}"), editor_core::GroupBreak::Force);
                    let p = core_vim::first_non_blank(d, line + 1);
                    d.set_caret(p);
                }
            });
        } else {
            let text = data.text.repeat(count);
            self.with_doc(|d| {
                let head = d.selections.primary().head;
                let line = d.char_to_line(head);
                let content_end = d.line_to_char(line) + d.line_len_chars(line);
                let at = if before {
                    head
                } else {
                    (head + 1).min(content_end)
                };
                d.set_caret(at);
                edit::insert_text(d, &text, editor_core::GroupBreak::Force);
                // Leave the caret on the last pasted char (Vim behaviour).
                let end = at + text.chars().count();
                d.set_caret(end.saturating_sub(1).max(at));
            });
        }
        self.vim_mut().clear_pending();
    }

    fn vim_undo(&mut self) {
        let count = self.vim().effective_count();
        for _ in 0..count {
            self.dispatch(Command::Undo);
        }
        self.vim_mut().clear_pending();
    }

    fn vim_redo(&mut self) {
        let count = self.vim().effective_count();
        for _ in 0..count {
            self.dispatch(Command::Redo);
        }
        self.vim_mut().clear_pending();
    }

    fn vim_scroll(&mut self, down: bool, half: bool) {
        let amount = if half {
            (self.page_height / 2).max(1)
        } else {
            self.page_height.max(1)
        };
        let delta = if down {
            amount as isize
        } else {
            -(amount as isize)
        };
        self.vim_move_lines(delta);
        self.vim_mut().clear_pending();
    }

    fn vim_dot_repeat(&mut self) {
        let times = self.vim_count_opt().unwrap_or(1);
        let keys = self.vim().last_change.clone();
        self.vim_mut().clear_pending();
        if keys.is_empty() {
            return;
        }
        self.vim_mut().replaying = true;
        for _ in 0..times {
            for ev in &keys {
                self.on_key(*ev);
            }
        }
        self.vim_mut().replaying = false;
    }

    /// Clamp the caret so it doesn't sit past the last char of its line (Vim's
    /// block cursor rests on a char, not after it).
    fn vim_clamp_caret(&mut self) {
        self.with_doc(|d| {
            let head = d.selections.primary().head;
            let line = d.char_to_line(head);
            let ls = d.line_to_char(line);
            let content = d.line_len_chars(line);
            let max = if content == 0 { ls } else { ls + content - 1 };
            if head > max {
                d.set_caret(max);
            }
        });
    }

    // --- registers -------------------------------------------------------

    fn vim_store_register(
        &mut self,
        reg: Option<char>,
        text: String,
        linewise: bool,
        is_yank: bool,
    ) {
        match reg {
            Some('_') => {}
            Some('+') | Some('*') => {
                self.clipboard.set(text.clone());
                self.vim_mut().unnamed = Register { text, linewise };
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let lower = c.to_ascii_lowercase();
                if c.is_ascii_uppercase() {
                    let entry = self.vim_mut().registers.entry(lower).or_default();
                    entry.text.push_str(&text);
                    entry.linewise = linewise;
                    let combined = self.vim().registers[&lower].clone();
                    self.vim_mut().unnamed = combined;
                } else {
                    self.vim_mut().registers.insert(
                        lower,
                        Register {
                            text: text.clone(),
                            linewise,
                        },
                    );
                    self.vim_mut().unnamed = Register { text, linewise };
                }
            }
            _ => {
                self.vim_mut().unnamed = Register {
                    text: text.clone(),
                    linewise,
                };
                if is_yank {
                    self.vim_mut().yanked = Register { text, linewise };
                }
            }
        }
    }

    fn vim_read_register(&mut self, reg: Option<char>) -> Register {
        match reg {
            Some('+') | Some('*') => Register {
                text: self.clipboard.get(),
                linewise: false,
            },
            Some('0') => self.vim().yanked.clone(),
            Some('_') => Register::default(),
            Some(c) if c.is_ascii_alphabetic() => self
                .vim()
                .registers
                .get(&c.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default(),
            _ => self.vim().unnamed.clone(),
        }
    }

    // --- visual mode -----------------------------------------------------

    fn vim_enter_visual(&mut self, mode: Mode) {
        // Toggling the same visual mode returns to Normal.
        if self.vim().mode == mode {
            self.vim_mut().mode = Mode::Normal;
            self.with_doc(|d| {
                let head = d.selections.primary().head;
                d.set_caret(head);
            });
            return;
        }
        self.vim_mut().mode = mode;
        // Anchor the selection at the caret.
        self.with_doc(|d| {
            let head = d.selections.primary().head;
            d.selections.set_single(Selection::new(head, head));
        });
    }

    fn vim_visual_set_head(&mut self, target: usize) {
        self.with_doc(|d| {
            let anchor = d.selections.primary().anchor;
            let head = d.clamp(target);
            d.selections.set_single(Selection::new(anchor, head));
        });
    }

    fn vim_visual_key(&mut self, key: KeyEvent) -> bool {
        let code = key.code;
        if code == KeyCode::Esc {
            self.vim_mut().mode = Mode::Normal;
            self.with_doc(|d| {
                let head = d.selections.primary().head;
                d.set_caret(head);
            });
            self.vim_mut().clear_pending();
            return true;
        }

        if let Some(fp) = self.vim().find_pending {
            self.vim_mut().find_pending = None;
            if let KeyCode::Char(c) = code {
                self.vim_find_apply(fp, c, true);
            }
            return true;
        }
        if let Some(prefix) = self.vim().prefix {
            return self.vim_handle_prefix(prefix, key);
        }

        match code {
            KeyCode::Char(c @ '1'..='9') => self.vim_mut().push_digit(c as usize - '0' as usize),
            KeyCode::Char('0') if self.vim().count_active() => self.vim_mut().push_digit(0),
            KeyCode::Char('"') => self.vim_mut().prefix = Some(Prefix::Register),
            // switch/toggle visual submode
            KeyCode::Char('v') => self.vim_enter_visual(Mode::Visual),
            KeyCode::Char('V') => self.vim_enter_visual(Mode::VisualLine),
            // swap ends
            KeyCode::Char('o') => self.with_doc(|d| {
                let s = d.selections.primary();
                d.selections.set_single(Selection::new(s.head, s.anchor));
            }),
            // text objects
            KeyCode::Char('i') => self.vim_mut().prefix = Some(Prefix::Object { around: false }),
            KeyCode::Char('a') => self.vim_mut().prefix = Some(Prefix::Object { around: true }),
            KeyCode::Char('g') => self.vim_mut().prefix = Some(Prefix::G),
            KeyCode::Char('f') => self.vim_mut().find_pending = Some(FindPending::Find),
            KeyCode::Char('F') => self.vim_mut().find_pending = Some(FindPending::FindBack),
            KeyCode::Char('t') => self.vim_mut().find_pending = Some(FindPending::Till),
            KeyCode::Char('T') => self.vim_mut().find_pending = Some(FindPending::TillBack),
            KeyCode::Char(';') => self.vim_repeat_find(false),
            KeyCode::Char(',') => self.vim_repeat_find(true),
            // operators act on the selection, then return to Normal
            KeyCode::Char('d') | KeyCode::Char('x') => self.vim_visual_operator(Operator::Delete),
            KeyCode::Char('c') | KeyCode::Char('s') => self.vim_visual_operator(Operator::Change),
            KeyCode::Char('y') => self.vim_visual_operator(Operator::Yank),
            KeyCode::Char('>') => self.vim_visual_operator(Operator::Indent),
            KeyCode::Char('<') => self.vim_visual_operator(Operator::Outdent),
            KeyCode::Char('u') => self.vim_visual_operator(Operator::Lower),
            KeyCode::Char('U') => self.vim_visual_operator(Operator::Upper),
            KeyCode::Char('~') => self.vim_visual_operator(Operator::ToggleCase),
            KeyCode::Char('r') => self.vim_mut().prefix = Some(Prefix::Replace),
            KeyCode::Char('J') => {
                // Join every selected line, from the first line of the selection.
                let lines = self
                    .editor
                    .active_document()
                    .map(|d| {
                        let s = d.selections.primary();
                        let fl = d.char_to_line(s.from());
                        let ll = d.char_to_line(s.to());
                        (d.line_to_char(fl), (ll - fl) + 1)
                    })
                    .unwrap_or((0, 1));
                self.vim_mut().mode = Mode::Normal;
                self.vim_mut().count = Some(lines.1.max(2));
                self.with_doc(|d| d.set_caret(lines.0));
                self.vim_join_lines();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let n = self.vim().effective_count() as isize;
                self.vim_visual_move_lines(n);
                self.vim_mut().count = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let n = self.vim().effective_count() as isize;
                self.vim_visual_move_lines(-n);
                self.vim_mut().count = None;
            }
            _ => {
                if let Some((target, _kind)) = self.vim_motion(code, self.vim_count_opt()) {
                    self.vim_visual_set_head(target);
                    self.vim_mut().count = None;
                } else {
                    self.vim_mut().clear_pending();
                }
            }
        }
        true
    }

    fn vim_visual_move_lines(&mut self, delta: isize) {
        let n = delta.unsigned_abs();
        let m = if delta < 0 { Motion::Up } else { Motion::Down };
        let page = self.page_height;
        self.with_doc(|d| {
            for _ in 0..n {
                edit::move_selections(d, m, page, true);
            }
        });
    }

    /// Visual-mode `r`: replace every selected char with `ch`, then return to Normal.
    fn vim_visual_replace(&mut self, ch: char) {
        let (start, end) = match self.editor.active_document() {
            Some(d) => {
                let s = d.selections.primary();
                (s.from(), (s.to() + 1).min(d.len_chars()))
            }
            None => return,
        };
        self.vim_mut().mode = Mode::Normal;
        if start >= end {
            self.vim_mut().clear_pending();
            return;
        }
        let out: String = match self.editor.active_document() {
            Some(d) => d
                .rope()
                .slice(start..end)
                .chars()
                .map(|c| if c == '\n' { '\n' } else { ch })
                .collect(),
            None => return,
        };
        self.with_doc(|d| {
            d.selections.set_single(Selection::new(start, end));
            edit::edit_selections(
                d,
                |_x, s| (s.span(), out.clone()),
                editor_core::GroupBreak::Force,
            );
            d.set_caret(start);
        });
        self.vim_mut().clear_pending();
    }

    /// Apply `op` to the current visual selection, then return to Normal.
    fn vim_visual_operator(&mut self, op: Operator) {
        let linewise = self.vim().mode == Mode::VisualLine;
        let (start, end) = match self.editor.active_document() {
            Some(d) => {
                let s = d.selections.primary();
                // Visual is inclusive of the char under the cursor.
                (s.from(), (s.to() + 1).min(d.len_chars()))
            }
            None => return,
        };
        // Leave visual mode before the edit so the resulting caret is a Normal caret.
        self.vim_mut().mode = Mode::Normal;
        if linewise {
            self.vim_apply_operator_kind(
                op,
                start,
                end.saturating_sub(1).max(start),
                MotionKind::Linewise,
            );
        } else {
            self.vim_apply_operator_range(op, start, end, false);
        }
        self.vim_mut().clear_pending();
    }

    // --- ex command line (`:`) ------------------------------------------

    fn vim_open_command(&mut self) {
        self.vim_mut().command = Some(String::new());
        self.vim_mut().recording = None;
    }

    fn vim_command_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.vim_mut().command = None,
            KeyCode::Enter => {
                let cmd = self.vim_mut().command.take().unwrap_or_default();
                self.vim_run_ex(&cmd);
            }
            KeyCode::Backspace => {
                let empty = {
                    let buf = self.vim_mut().command.as_mut().unwrap();
                    buf.pop();
                    buf.is_empty()
                };
                if empty {
                    self.vim_mut().command = None;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(buf) = self.vim_mut().command.as_mut() {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn vim_run_ex(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        if let Ok(n) = cmd.parse::<usize>() {
            self.with_doc(|d| {
                let l = n.saturating_sub(1).min(d.len_lines().saturating_sub(1));
                let p = core_vim::first_non_blank(d, l);
                d.set_caret(p);
            });
            return;
        }
        match cmd {
            "w" | "write" => self.dispatch(Command::Save),
            "wq" | "x" | "wq!" | "x!" => {
                self.dispatch(Command::Save);
                self.dispatch(Command::CloseTab);
            }
            "wa" | "wall" => self.dispatch(Command::SaveAll),
            "q" | "quit" => self.dispatch(Command::CloseTab),
            "q!" | "quit!" => {
                self.with_doc(|d| d.dirty = false);
                self.dispatch(Command::CloseTab);
            }
            "qa" | "qall" | "qa!" | "quitall" | "quitall!" => self.quit = true,
            "noh" | "nohl" | "nohlsearch" => self.editor.find = None,
            _ => {
                if is_substitute(cmd) {
                    self.vim_substitute_ex(cmd);
                } else {
                    self.editor.status_message = Some(format!("Not an editor command: {cmd}"));
                }
            }
        }
    }

    /// A minimal literal `:[%]s/old/new/[g]` (no regex, no escaping — documented).
    fn vim_substitute_ex(&mut self, cmd: &str) {
        let whole = cmd.starts_with('%');
        let body = cmd.trim_start_matches('%');
        let body = body.strip_prefix('s').unwrap_or(body);
        let mut parts = body.splitn(4, '/');
        let _lead = parts.next(); // empty (delimiter)
        let (Some(old), new) = (parts.next(), parts.next().unwrap_or("")) else {
            return;
        };
        if old.is_empty() {
            return;
        }
        let global = parts.next().unwrap_or("").contains('g');
        let (old, new) = (old.to_string(), new.to_string());
        self.with_doc(|d| {
            let (start, end) = if whole {
                (0, d.len_chars())
            } else {
                let line = d.char_to_line(d.selections.primary().head);
                let ls = d.line_to_char(line);
                (ls, ls + d.line_len_chars(line))
            };
            substitute_span(d, start, end, &old, &new, global);
        });
    }

    // --- search line (`/`, `?`) -----------------------------------------

    fn vim_open_search(&mut self, forward: bool) {
        self.vim_mut().search = Some((forward, String::new()));
        self.vim_mut().recording = None;
    }

    fn vim_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.vim_mut().search = None,
            KeyCode::Enter => {
                let (fwd, pat) = self
                    .vim_mut()
                    .search
                    .take()
                    .unwrap_or((true, String::new()));
                if !pat.is_empty() {
                    self.vim_mut().last_search = Some((fwd, pat.clone()));
                    self.vim_do_search(fwd, &pat, true);
                }
            }
            KeyCode::Backspace => {
                let empty = {
                    let (_, buf) = self.vim_mut().search.as_mut().unwrap();
                    buf.pop();
                    buf.is_empty()
                };
                if empty {
                    self.vim_mut().search = None;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some((_, buf)) = self.vim_mut().search.as_mut() {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    /// Literal search for `pat`; move the caret to the match. `from_next` starts one
    /// char past the caret so a repeat advances.
    fn vim_do_search(&mut self, forward: bool, pat: &str, from_next: bool) {
        let found = match self.editor.active_document() {
            Some(d) => {
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
            }
            None => None,
        };
        if let Some(pos) = found {
            self.with_doc(|d| d.set_caret(pos));
        } else {
            self.editor.status_message = Some(format!("Pattern not found: {pat}"));
        }
    }

    fn vim_search_next(&mut self, reverse: bool) {
        let Some((fwd, pat)) = self.vim().last_search.clone() else {
            return;
        };
        let dir = if reverse { !fwd } else { fwd };
        self.vim_do_search(dir, &pat, true);
    }

    fn vim_search_word(&mut self, forward: bool) {
        let word = self.editor.active_document().map(|d| {
            let head = d.selections.primary().head;
            let (s, e) = motion::word_at(d, head);
            d.rope().slice(s..e).to_string()
        });
        if let Some(word) = word {
            if !word.trim().is_empty() {
                self.vim_mut().last_search = Some((forward, word.clone()));
                self.vim_do_search(forward, &word, true);
            }
        }
    }

    /// Run a closure on the active document, returning its value.
    fn with_doc_ret<T, F: FnOnce(&mut Document) -> T>(&mut self, f: F) -> Option<T> {
        self.editor.active_document_mut().map(f)
    }
}

/// Step `f` `count` times over `doc` from `start`, stopping if it stalls.
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

/// Literal find/replace of `old`→`new` within `doc`'s `[start, end)` span (all
/// occurrences when `global`, else the first), applied as one transaction.
fn substitute_span(
    doc: &mut Document,
    start: usize,
    end: usize,
    old: &str,
    new: &str,
    global: bool,
) {
    let src = doc.rope().slice(start..end).to_string();
    let out = if global {
        src.replace(old, new)
    } else {
        src.replacen(old, new, 1)
    };
    if out == src {
        return;
    }
    doc.selections.set_single(Selection::new(start, end));
    edit::edit_selections(
        doc,
        |_x, s| (s.span(), out.clone()),
        editor_core::GroupBreak::Force,
    );
    doc.set_caret(start);
}

/// Naive literal substring search over `chars` for `pat`, wrapping around the
/// buffer. Returns the char offset of the match start.
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
