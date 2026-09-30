//! Vim modal state — the data the [`super::VimPlugin`] state machine reads and mutates, plus a
//! little pure bookkeeping (counts, dot-repeat recording, marks, jump list, macros).

use std::collections::HashMap;

use editor_plugin::input::Key;

/// The active editing mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

/// A Vim operator — the verb that acts on the range a motion or text object spans.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    /// `=` — reindent lines to match the previous line's leading whitespace.
    Reindent,
    /// `gq` — hard-wrap / fill lines to the wrap width.
    Format,
    Lower,
    Upper,
    ToggleCase,
}

/// How much of the text a motion grabs when an operator is applied over it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MotionKind {
    /// The landing char is **not** included (`w`, `0`, `{`).
    Exclusive,
    /// The landing char **is** included (`e`, `f`, `%`, `$`).
    Inclusive,
    /// Whole lines, regardless of column (`j`, `G`, `dd`).
    Linewise,
}

/// The contents of a register: text plus whether it was yanked line-wise.
#[derive(Clone, Default, Debug)]
pub(crate) struct Register {
    pub(crate) text: String,
    pub(crate) linewise: bool,
}

/// A multi-key prefix that changes how the next key is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Prefix {
    /// `g…` — `gg`, `ge`, `gu`, `gU`, `g~`, `gI`, `g_`, `gj`/`gk`, `gq`.
    G,
    /// `z…` — `zz`, `zt`, `zb`.
    Z,
    /// A text object: the next key is the object; `around` picks `a` (true) vs `i` (false).
    Object { around: bool },
    /// Replace-with (`r`): the next key is the replacement char.
    Replace,
    /// A register was requested with `"`: the next key names it.
    Register,
    /// `m{a-z}` — set a mark.
    MarkSet,
    /// `` `{a-z} `` — jump to a mark's exact position.
    MarkJumpExact,
    /// `'{a-z}` — jump to the first non-blank of a mark's line.
    MarkJumpLine,
    /// `q{a-z}` — start recording a macro into that register.
    MacroRecord,
    /// `@{a-z}` / `@@` — replay a macro register.
    MacroPlay,
}

/// A pending single-char argument for the `f`/`t`/`F`/`T` family.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FindPending {
    Find,
    Till,
    FindBack,
    TillBack,
}

/// Maximum jump-list entries (older entries drop off the front).
pub(crate) const JUMP_LIST_CAP: usize = 100;

/// Maximum keys stored in one macro register.
pub(crate) const MACRO_CAP: usize = 4096;

/// The whole Vim layer's state.
pub(crate) struct VimState {
    pub(crate) mode: Mode,
    /// Count typed before the operator (or before a bare motion).
    pub(crate) count: Option<usize>,
    /// Count typed after the operator (`d2w`); multiplies with `count`.
    pub(crate) op_count: Option<usize>,
    pub(crate) operator: Option<Operator>,
    pub(crate) register: Option<char>,
    pub(crate) prefix: Option<Prefix>,
    pub(crate) find_pending: Option<FindPending>,
    /// Last `f`/`t`/`F`/`T` for `;` (repeat) and `,` (reverse).
    pub(crate) last_find: Option<(FindPending, char)>,
    /// Named registers `a`–`z` (and any single char).
    pub(crate) registers: HashMap<char, Register>,
    /// The unnamed register `""` — last yank or delete.
    pub(crate) unnamed: Register,
    /// The yank register `"0` — survives deletes.
    pub(crate) yanked: Register,
    /// `:` ex command-line buffer; `Some` while the command line is open.
    pub(crate) command: Option<String>,
    /// `/` (true) or `?` (false) search buffer; `Some` while the search line is open.
    pub(crate) search: Option<(bool, String)>,
    /// The last search pattern, for `n`/`N`.
    pub(crate) last_search: Option<(bool, String)>,
    /// Keys captured for the change currently being made (dot-repeat).
    pub(crate) recording: Option<Vec<Key>>,
    /// The finished last change, replayed by `.`.
    pub(crate) last_change: Vec<Key>,
    /// True while `.` is feeding recorded keys back through the handler.
    pub(crate) replaying: bool,
    /// Document revision when the current recording began (to detect a real change).
    pub(crate) rev_at_record_start: u64,
    /// Local marks `a`–`z` → char offset in the current buffer (path-free, per VimState lifetime).
    pub(crate) marks: HashMap<char, usize>,
    /// Jump list: older → newer. `jump_idx` points at the current position (or `len` = "at tip").
    pub(crate) jumps: Vec<usize>,
    pub(crate) jump_idx: usize,
    /// Macro register currently being recorded (`q{a-z}` … `q`), separate from dot-repeat.
    pub(crate) macro_recording: Option<(char, Vec<Key>)>,
    /// Named macro registers `a`–`z`.
    pub(crate) macros: HashMap<char, Vec<Key>>,
    /// Last macro register played, for `@@`.
    pub(crate) last_macro: Option<char>,
    /// True while a macro is feeding keys back (blocks nested `@` / re-record).
    pub(crate) macro_replaying: bool,
}

impl VimState {
    pub(crate) fn new() -> VimState {
        VimState {
            mode: Mode::Normal,
            count: None,
            op_count: None,
            operator: None,
            register: None,
            prefix: None,
            find_pending: None,
            last_find: None,
            registers: HashMap::new(),
            unnamed: Register::default(),
            yanked: Register::default(),
            command: None,
            search: None,
            last_search: None,
            recording: None,
            last_change: Vec::new(),
            replaying: false,
            rev_at_record_start: 0,
            marks: HashMap::new(),
            jumps: Vec::new(),
            jump_idx: 0,
            macro_recording: None,
            macros: HashMap::new(),
            last_macro: None,
            macro_replaying: false,
        }
    }

    /// The effective repeat count: `count × op_count`, defaulting to 1.
    pub(crate) fn effective_count(&self) -> usize {
        let a = self.count.unwrap_or(1);
        let b = self.op_count.unwrap_or(1);
        (a * b).max(1)
    }

    /// True when the raw count (either accumulator) was explicitly typed.
    pub(crate) fn has_count(&self) -> bool {
        self.count.is_some() || self.op_count.is_some()
    }

    /// Push a digit onto the active count accumulator (post-operator once an operator is pending).
    pub(crate) fn push_digit(&mut self, d: usize) {
        if self.operator.is_some() {
            self.op_count = Some(self.op_count.unwrap_or(0) * 10 + d);
        } else {
            self.count = Some(self.count.unwrap_or(0) * 10 + d);
        }
    }

    /// True when a count is mid-entry, so `0` extends it rather than being a motion.
    pub(crate) fn count_active(&self) -> bool {
        if self.operator.is_some() {
            self.op_count.is_some()
        } else {
            self.count.is_some()
        }
    }

    /// Clear everything pending after a command completes/cancels — but keep mode, registers,
    /// and dot-repeat / mark / jump / macro state.
    pub(crate) fn clear_pending(&mut self) {
        self.count = None;
        self.op_count = None;
        self.operator = None;
        self.register = None;
        self.prefix = None;
        self.find_pending = None;
    }

    /// True when no command is mid-flight (a clean idle Normal state).
    pub(crate) fn is_idle(&self) -> bool {
        self.operator.is_none()
            && self.prefix.is_none()
            && self.find_pending.is_none()
            && self.command.is_none()
            && self.search.is_none()
            && self.count.is_none()
            && self.op_count.is_none()
            && self.register.is_none()
    }

    /// Append `key` to the in-progress dot-repeat recording (bounded).
    pub(crate) fn record_key(&mut self, key: Key, rev: u64) {
        if self.recording.is_none() {
            self.recording = Some(Vec::new());
            self.rev_at_record_start = rev;
        }
        if let Some(rec) = &mut self.recording {
            if rec.len() < 4096 {
                rec.push(key);
            }
        }
    }

    /// Commit an open recording as the last change (when the buffer changed) once back at a clean
    /// Normal state, or discard it.
    pub(crate) fn finalize_recording(&mut self, rev: u64) {
        if self.recording.is_some() && self.mode == Mode::Normal && self.is_idle() {
            let keys = self.recording.take().unwrap_or_default();
            if rev != self.rev_at_record_start && !keys.is_empty() {
                self.last_change = keys;
            }
        }
    }

    /// Record a jump from `from` before landing at `to`. Skips same-line moves.
    pub(crate) fn push_jump(&mut self, from: usize, to: usize, same_line: bool) {
        if from == to || same_line {
            return;
        }
        if self.jump_idx < self.jumps.len() {
            self.jumps.truncate(self.jump_idx);
        }
        if self.jumps.last() != Some(&from) {
            self.jumps.push(from);
        }
        if self.jumps.len() > JUMP_LIST_CAP {
            let drop = self.jumps.len() - JUMP_LIST_CAP;
            self.jumps.drain(..drop);
        }
        self.jump_idx = self.jumps.len();
    }

    /// Move to an older jump; returns the offset, or `None` at the oldest end.
    pub(crate) fn jump_older(&mut self, current: usize) -> Option<usize> {
        if self.jumps.is_empty() {
            return None;
        }
        if self.jump_idx == self.jumps.len() {
            self.jumps.push(current);
            if self.jumps.len() > JUMP_LIST_CAP {
                self.jumps.remove(0);
            }
            self.jump_idx = self.jumps.len() - 1;
        }
        if self.jump_idx == 0 {
            return None;
        }
        self.jump_idx -= 1;
        Some(self.jumps[self.jump_idx])
    }

    /// Move to a newer jump; returns the offset, or `None` at the tip.
    pub(crate) fn jump_newer(&mut self) -> Option<usize> {
        if self.jump_idx + 1 >= self.jumps.len() {
            return None;
        }
        self.jump_idx += 1;
        Some(self.jumps[self.jump_idx])
    }

    /// Append `key` to the open macro recording (if any).
    pub(crate) fn macro_record_key(&mut self, key: Key) {
        if let Some((_, keys)) = &mut self.macro_recording {
            if keys.len() < MACRO_CAP {
                keys.push(key);
            }
        }
    }

    /// A short status-line hint for the pending state (count, register, operator), or `None`.
    pub(crate) fn pending_hint(&self) -> Option<String> {
        if let Some((fwd, pat)) = &self.search {
            return Some(format!("{}{pat}", if *fwd { '/' } else { '?' }));
        }
        if let Some(cmd) = &self.command {
            return Some(format!(":{cmd}"));
        }
        if let Some((reg, _)) = &self.macro_recording {
            return Some(format!("recording @{reg}"));
        }
        let mut s = String::new();
        if let Some(r) = self.register {
            s.push('"');
            s.push(r);
        }
        if let Some(c) = self.count {
            s.push_str(&c.to_string());
        }
        if let Some(op) = self.operator {
            s.push_str(match op {
                Operator::Delete => "d",
                Operator::Change => "c",
                Operator::Yank => "y",
                Operator::Indent => ">",
                Operator::Outdent => "<",
                Operator::Reindent => "=",
                Operator::Format => "gq",
                Operator::Lower => "gu",
                Operator::Upper => "gU",
                Operator::ToggleCase => "g~",
            });
        }
        if let Some(oc) = self.op_count {
            s.push_str(&oc.to_string());
        }
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

impl Default for VimState {
    fn default() -> Self {
        VimState::new()
    }
}
