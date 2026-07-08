//! The `Document`: a rope plus everything that travels with an open buffer.
//!
//! All mutation goes through [`crate::transaction`] — never poke the rope directly from
//! outside `core` (CLAUDE.md invariant #1). The raw `apply_raw_*` helpers here are the
//! single internal chokepoint transactions use.

use std::path::PathBuf;

use ropey::Rope;

use crate::history::History;
use crate::selection::{Selection, Selections};
use crate::view::ViewState;

/// Text encoding of the on-disk file. UTF-8 is the default; we preserve what we detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

/// Line terminator style. Preserved from the original file; never silently rewritten
/// (CLAUDE.md / plan §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn as_str(&self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }

    /// Guess the dominant line ending of `text`.
    pub fn detect(text: &str) -> LineEnding {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count();
        // If most newlines are CRLF, treat the file as CRLF.
        if crlf > 0 && crlf * 2 >= lf {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    }
}

/// Content fingerprint used for external-sync reconciliation (plan §6).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiskFingerprint {
    pub hash: u64,
    pub len: usize,
}

/// An open buffer.
pub struct Document {
    pub text: Rope,
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
        }
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

    // --- raw mutation chokepoint (used ONLY by transactions) -------------------

    /// Remove chars in `[start, end)`. Internal; transactions call this.
    pub(crate) fn apply_raw_remove(&mut self, start: usize, end: usize) {
        let s = start.min(self.len_chars());
        let e = end.min(self.len_chars());
        if s < e {
            self.text.remove(s..e);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Insert `text` at char offset `at`. Internal; transactions call this.
    pub(crate) fn apply_raw_insert(&mut self, at: usize, text: &str) {
        let a = at.min(self.len_chars());
        self.text.insert(a, text);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Replace the whole buffer (external reload path).
    pub fn set_text(&mut self, rope: Rope) {
        self.text = rope;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Overwrite the current selection set.
    pub fn set_selections(&mut self, sel: Selections) {
        self.selections = sel;
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
mod tests {
    use super::*;

    #[test]
    fn from_str_round_trips() {
        let d = Document::from_str("hello\nworld");
        assert_eq!(d.to_string(), "hello\nworld");
        assert_eq!(d.len_lines(), 2);
    }

    #[test]
    fn crlf_detected_and_normalized() {
        let d = Document::from_str("a\r\nb\r\n");
        assert_eq!(d.line_ending, LineEnding::Crlf);
        assert_eq!(d.to_string(), "a\nb\n"); // stored as LF internally
    }

    #[test]
    fn line_len_excludes_newline() {
        let d = Document::from_str("abc\nde");
        assert_eq!(d.line_len_chars(0), 3);
        assert_eq!(d.line_len_chars(1), 2);
    }
}
