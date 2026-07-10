//! Vim modal editing state — the types the [`crate::app::App`] key handlers drive.
//!
//! Lumina is a mouse-first, non-modal editor; this adds an *optional* Vim layer on
//! top of it, enabled with `vim = true` in `[settings]` (or the `vim.toggle`
//! command). It is a **native module**, not a plugin: modal editing has to consume
//! raw keystrokes differently per mode — buffering counts, operators, and
//! operator-pending state — which the plugin contribution API (discrete
//! key→command bindings, no raw-keystroke event) can't express. It still respects
//! the architecture: every buffer change flows through `editor_core::edit`
//! (invariant #1), Vim reuses existing command ids where it can (invariant #4),
//! and the pure motion/text-object math lives in `editor_core::vim` (invariant #5).
//!
//! The state machine itself — how each key is interpreted — lives in
//! [`crate::app`]'s `vim` submodule (`impl App`), because acting on a key needs the
//! whole `App` (documents, clipboard, command dispatch). This module holds the
//! data those handlers read and mutate, plus a little pure bookkeeping.

use std::collections::HashMap;

use crossterm::event::KeyEvent;

/// The active editing mode. `Normal` is home base; `Insert` types literal text;
/// `Visual`/`VisualLine` select before acting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl Mode {
    /// The status-line badge for this mode (`-- NORMAL --`, …).
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        }
    }
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

/// The contents of a register: text plus whether it was yanked line-wise (so `p`
/// knows to paste on a new line).
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
    /// A text object is being entered: the next key is the object; `around` picks
    /// `a` (true) vs `i` (false).
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

/// The whole Vim layer's state, hung off [`crate::editor::EditorState`] so the
/// renderer (a pure function of state) can show the mode, and off `App` so the key
/// handlers can drive it.
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
    // --- dot-repeat via keystroke recording ---
    /// Keys captured for the change currently being made.
    pub recording: Option<Vec<KeyEvent>>,
    /// The finished last change, replayed by `.`.
    pub last_change: Vec<KeyEvent>,
    /// True while `.` is feeding recorded keys back through the handler.
    pub replaying: bool,
    /// Document revision when the current recording began (to detect a real change).
    pub rev_at_record_start: u64,
}

impl VimState {
    /// A fresh Vim layer, starting in Normal mode.
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

    /// Push a digit onto the active count accumulator (post-operator once an
    /// operator is pending). A leading `0` with no count in progress is *not* a
    /// count — it's the `0` motion; the caller checks [`Self::count_active`] first.
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

    /// Clear everything pending after a command completes or is cancelled — but
    /// keep the mode, registers, and dot-repeat state.
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

    /// Append `key` to the in-progress dot-repeat recording, starting one if none
    /// is open. Bounded so a very long insert session can't grow without limit.
    pub fn record_key(&mut self, key: KeyEvent, rev: u64) {
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

    /// If a recording is open and we're back at a clean Normal state, commit it as
    /// the last change (when the buffer actually changed) or discard it.
    pub fn finalize_recording(&mut self, rev: u64) {
        if self.recording.is_some() && self.mode == Mode::Normal && self.is_idle() {
            let keys = self.recording.take().unwrap_or_default();
            if rev != self.rev_at_record_start && !keys.is_empty() {
                self.last_change = keys;
            }
        }
    }

    /// A short hint for the status line describing the pending state (count,
    /// register, operator), or `None` when idle.
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
