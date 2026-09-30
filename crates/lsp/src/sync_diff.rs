//! Compute a single ranged `textDocument/didChange` from two full snapshots.
//!
//! When the client does not retain a transaction log, common-prefix / common-suffix
//! reduction still yields a correct incremental `ContentChangeEvent` for
//! `TextDocumentSyncKind::Incremental` servers.

/// One LSP `TextDocumentContentChangeEvent` with an explicit range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentChange {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub text: String,
}

/// Reduce `old` → `new` to one ranged change. Falls back to a whole-document replace
/// when the snapshots share no useful prefix/suffix (or either is empty).
pub fn incremental_change(old: &str, new: &str) -> ContentChange {
    if old.is_empty() && new.is_empty() {
        return ContentChange {
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 0,
            text: String::new(),
        };
    }

    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut prefix = 0usize;
    let max_prefix = old_chars.len().min(new_chars.len());
    while prefix < max_prefix && old_chars[prefix] == new_chars[prefix] {
        prefix += 1;
    }

    let mut suffix = 0usize;
    let max_suffix = (old_chars.len() - prefix).min(new_chars.len() - prefix);
    while suffix < max_suffix
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let start = char_offset_to_position(old, prefix);
    let end = char_offset_to_position(old, old_chars.len() - suffix);
    let text: String = new_chars[prefix..new_chars.len() - suffix].iter().collect();
    ContentChange {
        start_line: start.0,
        start_character: start.1,
        end_line: end.0,
        end_character: end.1,
        text,
    }
}

/// Char offset (rope/LSP document coordinates after LF normalization) → `(line, utf16 col)`.
pub fn char_offset_to_position(text: &str, char_offset: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut col_utf16 = 0u32;
    for (seen, ch) in text.chars().enumerate() {
        if seen >= char_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col_utf16 = 0;
        } else {
            col_utf16 += ch.len_utf16() as u32;
        }
    }
    (line, col_utf16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_edit_is_ranged() {
        let change = incremental_change("hello world", "hello rusty world");
        assert_eq!(change.start_line, 0);
        assert_eq!(change.start_character, 6);
        assert_eq!(change.end_line, 0);
        assert_eq!(change.end_character, 6);
        assert_eq!(change.text, "rusty ");
    }

    #[test]
    fn append_is_ranged_at_end() {
        let change = incremental_change("abc", "abcd");
        assert_eq!(change.start_character, 3);
        assert_eq!(change.end_character, 3);
        assert_eq!(change.text, "d");
    }

    #[test]
    fn multiline_replace() {
        let change = incremental_change("a\nb\nc", "a\nX\nc");
        assert_eq!(change.start_line, 1);
        assert_eq!(change.end_line, 1);
        assert_eq!(change.text, "X");
    }

    #[test]
    fn emoji_counts_two_utf16_units() {
        let change = incremental_change("a😀b", "a😀Xb");
        assert_eq!(change.start_character, 3); // after a + 😀
        assert_eq!(change.end_character, 3);
        assert_eq!(change.text, "X");
    }
}
