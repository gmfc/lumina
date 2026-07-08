//! Position conversion between the editor's char columns and LSP's UTF-16 columns.
//!
//! LSP `Position.character` counts **UTF-16 code units**, so a char above U+FFFF (most
//! emoji) counts as 2. Getting this wrong misplaces diagnostics next to emoji/CJK — hence
//! the dedicated, tested conversion.

/// Char column → UTF-16 column within a single line.
pub fn char_col_to_utf16(line_text: &str, char_col: usize) -> u32 {
    line_text
        .chars()
        .take(char_col)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// UTF-16 column → char column within a single line (clamped to the line length).
pub fn utf16_to_char_col(line_text: &str, utf16: u32) -> usize {
    let mut acc = 0u32;
    for (i, c) in line_text.chars().enumerate() {
        if acc >= utf16 {
            return i;
        }
        acc += c.len_utf16() as u32;
    }
    line_text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_to_one() {
        assert_eq!(char_col_to_utf16("hello", 3), 3);
        assert_eq!(utf16_to_char_col("hello", 3), 3);
    }

    #[test]
    fn astral_char_counts_two_utf16_units() {
        // "a😀b": 😀 is one char but two UTF-16 units.
        let line = "a😀b";
        assert_eq!(char_col_to_utf16(line, 1), 1); // before 😀
        assert_eq!(char_col_to_utf16(line, 2), 3); // after 😀 (1 + 2)
        assert_eq!(char_col_to_utf16(line, 3), 4); // after b
                                                   // Inverse: UTF-16 col 3 lands on char index 2 ('b').
        assert_eq!(utf16_to_char_col(line, 3), 2);
        assert_eq!(utf16_to_char_col(line, 1), 1);
    }
}
