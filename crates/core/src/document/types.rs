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
///
/// **Mixed files:** when more than one style appears, [`LineEnding::detect`] picks the
/// dominant style and [`super::Document::mixed_line_endings`] is set. On save, every
/// newline is re-emitted in that dominant style — mixed files do not round-trip
/// byte-for-byte. See README / ARCHITECTURE for the honesty note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
    /// Classic Mac OS CR-only (`\r`). Rare, but modeled so line counting and save are defined.
    Cr,
}

/// Result of sniffing a buffer's newline style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineEndingInfo {
    pub style: LineEnding,
    /// True when more than one of LF / CRLF / CR appeared in the source text.
    pub mixed: bool,
}

impl LineEnding {
    pub fn as_str(&self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    /// Guess the dominant line ending of `text`.
    pub fn detect(text: &str) -> LineEnding {
        Self::detect_info(text).style
    }

    /// Guess the dominant line ending and whether the file mixed styles.
    pub fn detect_info(text: &str) -> LineEndingInfo {
        let mut crlf = 0usize;
        let mut lf = 0usize;
        let mut cr = 0usize;
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                    crlf += 1;
                    i += 2;
                }
                b'\r' => {
                    cr += 1;
                    i += 1;
                }
                b'\n' => {
                    lf += 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }
        let kinds = usize::from(crlf > 0) + usize::from(lf > 0) + usize::from(cr > 0);
        let mixed = kinds > 1;
        let style = if crlf >= lf && crlf >= cr && crlf > 0 {
            LineEnding::Crlf
        } else if cr > lf && cr > 0 {
            LineEnding::Cr
        } else {
            LineEnding::Lf
        };
        LineEndingInfo { style, mixed }
    }
}

/// Normalize any CR / CRLF / LF mix to LF-only text for the in-memory rope.
pub fn normalize_to_lf(text: &str) -> String {
    // CRLF first so lone-CR rewrite does not leave orphan LFs from the pair.
    text.replace("\r\n", "\n").replace('\r', "\n")
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
