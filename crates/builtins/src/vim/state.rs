//! Vim modal state — the data the [`super::VimPlugin`] state machine reads and mutates, plus a
//! little pure bookkeeping (counts, dot-repeat recording). Moved out of the app with vim; the only
//! change is that dot-repeat records the crossterm-free [`Key`] the plugin sees.

use std::collections::HashMap;

use editor_plugin::input::Key;

/// The active editing mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

/// A Vim operator — the verb that acts on the range a motion or text object spans.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lower,
    Upper,
    ToggleCase,
}

/// How much of the text a motion grabs when an operator is applied over it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionKind {
    /// The landing char is **not** included (`w`, `0`, `{`).
    Exclusive,
    /// The landing char **is** included (`e`, `f`, `%`, `$`).
    Inclusive,
    /// Whole lines, regardless of column (`j`, `G`, `dd`).
    Linewise,
}

/// The contents of a register: text plus whether it was yanked line-wise.
#[derive(Clone, Default, Debug)]
pub struct Register {
    pub text: String,
    pub linewise: bool,
}

/// A multi-key prefix that changes how the next key is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prefix {
    /// `g…` — `gg`, `ge`, `gu`, `gU`, `g~`, `gI`, `g_`.
    G,
    /// `z…` — `zz`, `zt`, `zb`.
    Z,
    /// A text object: the next key is the object; `around` picks `a` (true) vs `i` (false).
    Object { around: bool },
    /// Replace-with (`r`): the next key is the replacement char.
    Replace,
    /// A register was requested with `"`: the next key names it.
    Register,
}

/// A pending single-char argument for the `f`/`t`/`F`/`T` family.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FindPending {
    Find,
    Till,
    FindBack,
    TillBack,
}

/// The whole Vim layer's state.
pub struct VimState {
    pub mode: Mode,
    /// Count typed before the operator (or before a bare motion).
    pub count: Option<usize>,
    /// Count typed after the operator (`d2w`); multiplies with `count`.
    pub op_count: Option<usize>,
    pub operator: Option<Operator>,
    pub register: Option<char>,
    pub prefix: Option<Prefix>,
    pub find_pending: Option<FindPending>,
    /// Last `f`/`t`/`F`/`T` for `;` (repeat) and `,` (reverse).
    pub last_find: Option<(FindPending, char)>,
    /// Named registers `a`–`z` (and any single char).
    pub registers: HashMap<char, Register>,
    /// The unnamed register `""` — last yank or delete.
    pub unnamed: Register,
    /// The yank register `"0` — survives deletes.
    pub yanked: Register,
    /// `:` ex command-line buffer; `Some` while the command line is open.
    pub command: Option<String>,
    /// `/` (true) or `?` (false) search buffer; `Some` while the search line is open.
    pub search: Option<(bool, String)>,
    /// The last search pattern, for `n`/`N`.
    pub last_search: Option<(bool, String)>,
    /// Keys captured for the change currently being made.
    pub recording: Option<Vec<Key>>,
    /// The finished last change, replayed by `.`.
    pub last_change: Vec<Key>,
    /// True while `.` is feeding recorded keys back through the handler.
    pub replaying: bool,
    /// Document revision when the current recording began (to detect a real change).
    pub rev_at_record_start: u64,
}

impl VimState {
    pub fn new() -> VimState {
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
        }
    }

    /// The effective repeat count: `count × op_count`, defaulting to 1.
    pub fn effective_count(&self) -> usize {
        let a = self.count.unwrap_or(1);
        let b = self.op_count.unwrap_or(1);
        (a * b).max(1)
    }

    /// True when the raw count (either accumulator) was explicitly typed.
    pub fn has_count(&self) -> bool {
        self.count.is_some() || self.op_count.is_some()
    }

    /// Push a digit onto the active count accumulator (post-operator once an operator is pending).
    pub fn push_digit(&mut self, d: usize) {
        if self.operator.is_some() {
            self.op_count = Some(self.op_count.unwrap_or(0) * 10 + d);
        } else {
            self.count = Some(self.count.unwrap_or(0) * 10 + d);
        }
    }

    /// True when a count is mid-entry, so `0` extends it rather than being a motion.
    pub fn count_active(&self) -> bool {
        if self.operator.is_some() {
            self.op_count.is_some()
        } else {
            self.count.is_some()
        }
    }

    /// Clear everything pending after a command completes/cancels — but keep mode, registers,
    /// and dot-repeat state.
    pub fn clear_pending(&mut self) {
        self.count = None;
        self.op_count = None;
        self.operator = None;
        self.register = None;
        self.prefix = None;
        self.find_pending = None;
    }

    /// True when no command is mid-flight (a clean idle Normal state).
    pub fn is_idle(&self) -> bool {
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
    pub fn record_key(&mut self, key: Key, rev: u64) {
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
    pub fn finalize_recording(&mut self, rev: u64) {
        if self.recording.is_some() && self.mode == Mode::Normal && self.is_idle() {
            let keys = self.recording.take().unwrap_or_default();
            if rev != self.rev_at_record_start && !keys.is_empty() {
                self.last_change = keys;
            }
        }
    }

    /// A short status-line hint for the pending state (count, register, operator), or `None`.
    pub fn pending_hint(&self) -> Option<String> {
        if let Some((fwd, pat)) = &self.search {
            return Some(format!("{}{pat}", if *fwd { '/' } else { '?' }));
        }
        if let Some(cmd) = &self.command {
            return Some(format!(":{cmd}"));
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
