//! In-file find/replace state and search (plan §6). Regex-backed with case, whole-word,
//! and regex toggles; `$1` capture references in replacements when regex mode is on.
//!
//! Matches are tracked as **char ranges** (the rope's unit) so highlighting and transaction
//! building stay consistent with the rest of `core`.

use regex::{Regex, RegexBuilder};

/// Which field of the find widget currently receives typed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Query,
    Replace,
}

pub struct FindState {
    pub query: String,
    pub replace: String,
    /// Whether the replace row is shown (Ctrl+H vs Ctrl+F).
    pub replace_mode: bool,
    pub field: Field,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// All match char ranges `[start, end)`, in document order.
    pub matches: Vec<(usize, usize)>,
    /// Index of the "current" match within `matches`.
    pub current: usize,
    /// Char offset the widget opened at — the stable anchor for "nearest match" while typing
    /// (so the current match doesn't drift as the query grows).
    pub origin: usize,
    /// Set when the regex fails to compile.
    pub error: Option<String>,
}

impl FindState {
    pub fn new(replace_mode: bool) -> FindState {
        FindState {
            query: String::new(),
            replace: String::new(),
            replace_mode,
            field: Field::Query,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            matches: Vec::new(),
            current: 0,
            origin: 0,
            error: None,
        }
    }

    /// Build the effective regex from the query + toggles.
    fn build(&self) -> Result<Regex, String> {
        let base = if self.regex {
            self.query.clone()
        } else {
            regex::escape(&self.query)
        };
        let pattern = if self.whole_word {
            format!(r"\b(?:{base})\b")
        } else {
            base
        };
        RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .build()
            .map_err(|e| e.to_string())
    }

    /// Recompute matches over `text`, keeping the current match near `cursor_char`.
    pub fn recompute(&mut self, text: &str, cursor_char: usize) {
        self.matches.clear();
        self.error = None;
        if self.query.is_empty() {
            return;
        }
        let re = match self.build() {
            Ok(re) => re,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        // Map byte offsets to char offsets with a single running counter.
        let mut byte_to_char: Vec<usize> = Vec::new();
        let mut char_idx = 0;
        let mut next_byte = 0;
        for (b, _) in text.char_indices() {
            while next_byte <= b {
                byte_to_char.push(char_idx);
                next_byte += 1;
            }
            char_idx += 1;
        }
        while byte_to_char.len() <= text.len() {
            byte_to_char.push(char_idx);
        }

        for m in re.find_iter(text) {
            if m.start() == m.end() {
                continue; // skip empty matches
            }
            let s = byte_to_char[m.start()];
            let e = byte_to_char[m.end()];
            self.matches.push((s, e));
        }
        // Current = first match at/after the cursor, else the last.
        self.current = self
            .matches
            .iter()
            .position(|&(s, _)| s >= cursor_char)
            .unwrap_or(0);
    }

    pub fn current_match(&self) -> Option<(usize, usize)> {
        self.matches.get(self.current).copied()
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + 1) % self.matches.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + self.matches.len() - 1) % self.matches.len();
        }
    }

    /// The replacement string for a match's captured text (expands `$1` etc. in regex mode).
    pub fn replacement_for(&self, matched: &str) -> String {
        if !self.regex {
            return self.replace.clone();
        }
        match self.build() {
            Ok(re) => {
                if let Some(caps) = re.captures(matched) {
                    let mut out = String::new();
                    caps.expand(&self.replace, &mut out);
                    out
                } else {
                    self.replace.clone()
                }
            }
            Err(_) => self.replace.clone(),
        }
    }

    /// Type a char into the focused field.
    pub fn input_char(&mut self, ch: char) {
        match self.field {
            Field::Query => self.query.push(ch),
            Field::Replace => self.replace.push(ch),
        }
    }

    /// Backspace the focused field.
    pub fn backspace(&mut self) {
        match self.field {
            Field::Query => {
                self.query.pop();
            }
            Field::Replace => {
                self.replace.pop();
            }
        }
    }

    /// Switch focus between query and replace (only meaningful in replace mode).
    pub fn toggle_field(&mut self) {
        if self.replace_mode {
            self.field = match self.field {
                Field::Query => Field::Replace,
                Field::Replace => Field::Query,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matches() {
        let mut f = FindState::new(false);
        f.query = "lo".into();
        f.recompute("hello world lo", 0);
        assert_eq!(f.matches, vec![(3, 5), (12, 14)]);
    }

    #[test]
    fn case_and_word_toggles() {
        let mut f = FindState::new(false);
        f.query = "the".into();
        f.recompute("The theme is theirs", 0);
        assert_eq!(f.matches.len(), 3); // case-insensitive by default
        f.case_sensitive = true;
        f.recompute("The theme is theirs", 0);
        assert_eq!(f.matches.len(), 2); // "The" excluded
        f.whole_word = true;
        f.recompute("The theme is theirs the", 0);
        assert_eq!(f.matches.len(), 1); // only standalone "the"
    }

    #[test]
    fn regex_capture_replacement() {
        let mut f = FindState::new(true);
        f.regex = true;
        f.query = r"(\w+)@(\w+)".into();
        f.replace = "$2.$1".into();
        assert_eq!(f.replacement_for("user@host"), "host.user");
    }

    #[test]
    fn cursor_selects_nearest_forward_match() {
        let mut f = FindState::new(false);
        f.query = "x".into();
        f.recompute("x..x..x", 3);
        assert_eq!(f.current_match(), Some((3, 4)));
    }
}
