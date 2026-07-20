//! The `Document`: a rope plus everything that travels with an open buffer.
//!
//! All mutation goes through [`crate::transaction`] — never poke the rope directly from
//! outside `core` (CLAUDE.md invariant #1). The raw `apply_raw_*` helpers (see [`mutate`])
//! are the single internal chokepoint transactions use.

use std::path::PathBuf;

use ropey::Rope;

use crate::history::History;
use crate::selection::{Selection, Selections};
use crate::view::ViewState;

mod mutate;
mod types;

pub use types::{DiskFingerprint, Encoding, LineEnding, SyntaxEdit};

/// An open buffer.
pub struct Document {
    /// The rope. `pub(crate)` so invariant #1 is enforced by the type system, not convention:
    /// nothing outside `core` can poke the rope directly — external readers go through
    /// [`Document::rope`] and its accessors, and the only writer is [`crate::transaction`] via
    /// the `apply_raw_*` chokepoint (see [`mutate`]).
    pub(crate) text: Rope,
    pub path: Option<PathBuf>,
    pub selections: Selections,
    pub history: History,
    pub view: ViewState,
    pub dirty: bool,
    pub language: Option<String>,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    pub tab_width: usize,
    /// Monotonic counter bumped on every text mutation (drives syntax re-parse caching).
    pub revision: u64,
    /// Last-known on-disk fingerprint (for external-change reconciliation).
    pub disk: DiskFingerprint,
    /// Set when the disk copy changed under a dirty buffer — a conflict to resolve.
    pub external_conflict: Option<DiskFingerprint>,
    /// Set for one frame after a clean external reload (draw a ↻ badge).
    pub externally_reloaded: bool,
    /// Set when the file was deleted on disk while still open.
    pub deleted_on_disk: bool,
    /// Edits accumulated since the syntax layer last reparsed (drained by the highlighter).
    pub syntax_edits: Vec<SyntaxEdit>,
    /// False when `syntax_edits` no longer faithfully describes the change stream (overflow or
    /// a whole-buffer replace) — the highlighter must then do a full reparse.
    pub syntax_edits_valid: bool,
}

impl Document {
    /// Build an in-memory document from a string (no path). Infallible by contract — the
    /// pinned `transaction_roundtrip` suite calls `Document::from_str(&str) -> Document`,
    /// so this deliberately does not implement the fallible `std::str::FromStr`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Document {
        let line_ending = LineEnding::detect(s);
        // Store text normalized to LF internally; re-emit line_ending on save.
        let normalized = s.replace("\r\n", "\n");
        Document {
            text: Rope::from_str(&normalized),
            path: None,
            selections: Selections::default(),
            history: History::default(),
            view: ViewState::default(),
            dirty: false,
            language: None,
            encoding: Encoding::default(),
            line_ending,
            tab_width: 4,
            revision: 0,
            disk: DiskFingerprint::default(),
            external_conflict: None,
            externally_reloaded: false,
            deleted_on_disk: false,
            syntax_edits: Vec::new(),
            syntax_edits_valid: true,
        }
    }

    /// Read-only access to the underlying rope. This is the *only* way code outside `core`
    /// touches the buffer text (invariant #1): it hands out `&Rope` for reads — `slice`, `char`,
    /// `chars`, `len_*` — but never `&mut`, so the rope can only be mutated through a
    /// [`crate::transaction::Transaction`].
    pub fn rope(&self) -> &Rope {
        &self.text
    }

    /// Total number of chars (the rope's natural index unit).
    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.text.len_lines()
    }

    /// Char offset of the start of `line`.
    pub fn line_to_char(&self, line: usize) -> usize {
        self.text.line_to_char(line.min(self.text.len_lines()))
    }

    /// Line containing char offset `char_idx`.
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.text.char_to_line(char_idx.min(self.len_chars()))
    }

    /// `(line, column_in_chars)` for a char offset.
    pub fn char_to_line_col(&self, char_idx: usize) -> (usize, usize) {
        let idx = char_idx.min(self.len_chars());
        let line = self.text.char_to_line(idx);
        let col = idx - self.text.line_to_char(line);
        (line, col)
    }

    /// The text of `line` including its trailing newline (if any), as a String.
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.text.len_lines() {
            return String::new();
        }
        self.text.line(line).to_string()
    }

    /// The text of `line` **including its trailing newline**, borrowed from the rope when the
    /// line lives in a single contiguous chunk (the common case) and only allocated when it
    /// straddles a chunk boundary. The render hot path calls this per visible line per frame, so
    /// borrowing here (instead of [`Self::line_text`]'s owned `String`) removes one heap
    /// allocation per rendered line.
    pub fn line_str(&self, line: usize) -> std::borrow::Cow<'_, str> {
        use std::borrow::Cow;
        if line >= self.text.len_lines() {
            return Cow::Borrowed("");
        }
        let slice = self.text.line(line);
        match slice.as_str() {
            Some(s) => Cow::Borrowed(s),
            None => Cow::Owned(slice.to_string()),
        }
    }

    /// Length of `line` in chars, excluding the trailing newline.
    pub fn line_len_chars(&self, line: usize) -> usize {
        if line >= self.text.len_lines() {
            return 0;
        }
        let slice = self.text.line(line);
        let mut n = slice.len_chars();
        // Drop trailing newline chars from the count.
        let s = slice;
        if n > 0 && s.char(n - 1) == '\n' {
            n -= 1;
            if n > 0 && s.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        n
    }

    /// Clamp a char offset into `[0, len]`.
    pub fn clamp(&self, char_idx: usize) -> usize {
        char_idx.min(self.len_chars())
    }

    /// Overwrite the current selection set. Normalizes at the boundary (invariant #2:
    /// parse-don't-validate), so downstream edit code can rely on the set being sorted and
    /// non-overlapping without re-checking — even when a caller built it with `single` + `push`.
    pub fn set_selections(&mut self, sel: Selections) {
        self.selections = sel.normalized();
    }

    /// Replace the whole buffer as an external reload (the file changed on disk under a clean
    /// buffer). The new content is authoritative, so the prior undo history — recorded against
    /// the *old* text — is discarded: a transaction's offsets no longer describe this buffer, and
    /// replaying one on undo would restore wrong text (invariant #1). Callers set
    /// selections/dirty/disk afterward. This is the sanctioned whole-buffer replace; the raw
    /// `set_text*` helpers stay `pub(crate)`.
    pub fn reload_from_str(&mut self, s: &str) {
        self.set_text_str(s);
        self.history = History::default();
    }

    /// Convenience: collapse to a single caret at `pos`.
    pub fn set_caret(&mut self, pos: usize) {
        let p = self.clamp(pos);
        self.selections.set_single(Selection::caret(p));
    }
}

impl std::fmt::Display for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.text)
    }
}

#[cfg(test)]
mod tests;
