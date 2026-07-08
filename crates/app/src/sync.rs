//! External-sync helpers (plan §6): pure position-following across an external reload, so a
//! clean buffer's cursor/scroll stay on the same *logical* line as edits stream in.
//!
//! Line-based diff: match the common leading and trailing lines, and map positions through
//! the changed middle. Simple, allocation-light, and unit-testable (the plan allows
//! "imara-diff or similar"; this is the "similar").

/// Map a char offset from `old` text to the corresponding offset in `new` text.
pub fn map_offset(old: &str, new: &str, offset: usize) -> usize {
    let old_lines: Vec<&str> = split_lines(old);
    let new_lines: Vec<&str> = split_lines(new);
    let (line, col) = offset_to_line_col(old, offset);

    let prefix = common_prefix_lines(&old_lines, &new_lines);
    let suffix = common_suffix_lines(&old_lines, &new_lines, prefix);

    let new_line = if line < prefix {
        line
    } else if line + suffix >= old_lines.len() && old_lines.len() >= suffix {
        // In the common trailing region: shift by the line-count delta.
        let delta = new_lines.len() as isize - old_lines.len() as isize;
        (line as isize + delta).max(0) as usize
    } else {
        // In the changed middle: clamp to the start of the changed region.
        prefix.min(new_lines.len().saturating_sub(1))
    };
    let new_line = new_line.min(new_lines.len().saturating_sub(1));
    line_col_to_offset(&new_lines, new_line, col)
}

/// The first line index that differs between `old` and `new` (for follow-mode scroll).
pub fn first_changed_line(old: &str, new: &str) -> usize {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    common_prefix_lines(&old_lines, &new_lines)
}

fn split_lines(s: &str) -> Vec<&str> {
    // Preserve a trailing empty line so line counts match rope semantics.
    s.split('\n').collect()
}

fn common_prefix_lines(a: &[&str], b: &[&str]) -> usize {
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    i
}

fn common_suffix_lines(a: &[&str], b: &[&str], prefix: usize) -> usize {
    let mut i = 0;
    while i < a.len().saturating_sub(prefix)
        && i < b.len().saturating_sub(prefix)
        && a[a.len() - 1 - i] == b[b.len() - 1 - i]
    {
        i += 1;
    }
    i
}

fn offset_to_line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 0;
    let mut col = 0;
    for (i, ch) in text.chars().enumerate() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn line_col_to_offset(lines: &[&str], line: usize, col: usize) -> usize {
    let mut offset = 0;
    for l in lines.iter().take(line) {
        offset += l.chars().count() + 1; // + newline
    }
    let line_len = lines.get(line).map(|l| l.chars().count()).unwrap_or(0);
    offset + col.min(line_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_holds_line_when_edit_is_above() {
        let old = "a\nb\nCURSOR\nd";
        let new = "a\nX\nY\nb\nCURSOR\nd"; // two lines inserted above
                                           // Cursor at start of "CURSOR" (line 2 in old).
        let off = old.find("CURSOR").unwrap();
        // find() returns a byte offset; here all ASCII so byte == char.
        let mapped = map_offset(old, new, off);
        let (line, _) = offset_to_line_col(new, mapped);
        assert_eq!(new.lines().nth(line), Some("CURSOR"));
    }

    #[test]
    fn cursor_holds_when_edit_is_below() {
        let old = "top\nMID\nbottom";
        let new = "top\nMID\nbottom\nextra\nmore";
        let off = old.find("MID").unwrap();
        let mapped = map_offset(old, new, off);
        let (line, _) = offset_to_line_col(new, mapped);
        assert_eq!(new.lines().nth(line), Some("MID"));
    }

    #[test]
    fn first_changed_line_detected() {
        assert_eq!(first_changed_line("a\nb\nc", "a\nX\nc"), 1);
        assert_eq!(first_changed_line("a\nb", "a\nb"), 2); // identical → past end
    }
}
