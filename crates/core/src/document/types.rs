//! Value types that travel with a [`Document`](super::Document): encoding, line ending,
//! the on-disk fingerprint, and the tree-sitter edit record.

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

/// A byte/point-level edit record, in the exact shape tree-sitter's `InputEdit` needs, so the
/// syntax layer can reparse **incrementally** instead of from scratch on every keystroke
/// (plan §4 perf, §9 "incremental highlighting"). Points are `(row, column-in-bytes)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
}

/// Cap on buffered edits before we give up on incremental reparse and force a full one — a
/// safety valve so a huge programmatic rewrite doesn't accumulate unbounded edit records.
pub(crate) const SYNTAX_EDIT_CAP: usize = 4096;
