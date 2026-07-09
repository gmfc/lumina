//! In-file find/replace state and search (plan §6). Regex-backed with case, whole-word,
//! and regex toggles; `$1` capture references in replacements when regex mode is on.
//!
//! Matches are tracked as **char ranges** (the rope's unit) so highlighting and transaction
//! building stay consistent with the rest of `core`.

use regex::{Regex, RegexBuilder};

/// Upper bound on in-file matches tracked at once. A broad query (e.g. `.`) on a large file
/// could otherwise produce a match-per-char list that bloats memory and the per-frame highlight
/// scan; capping keeps find responsive (project search caps similarly).
const MAX_MATCHES: usize = 5000;

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
        // Collect match byte ranges, capped so a pathological query on a huge file (e.g. `.`)
        // can't produce a match-per-char list that blows up memory and the per-frame highlight
        // scan. Matches arrive in ascending, non-overlapping order.
        let mut raw: Vec<(usize, usize)> = Vec::new();
        for m in re.find_iter(text) {
            if m.start() == m.end() {
                continue; // skip empty matches
            }
            raw.push((m.start(), m.end()));
            if raw.len() >= MAX_MATCHES {
                break;
            }
        }

        // Convert the ascending byte boundaries to char offsets in a single pass — O(matches)
        // space, rather than materializing a whole-document byte→char table on every keystroke.
        let mut targets: Vec<usize> = Vec::with_capacity(raw.len() * 2);
        for &(s, e) in &raw {
            targets.push(s);
            targets.push(e);
        }
        let mut char_of_target = vec![0usize; targets.len()];
        let mut ti = 0;
        let mut char_idx = 0usize;
        for (b, _) in text.char_indices() {
            while ti < targets.len() && targets[ti] <= b {
                char_of_target[ti] = char_idx;
                ti += 1;
            }
            char_idx += 1;
        }
        // Any remaining targets sit at end-of-text (byte == text.len()).
        while ti < targets.len() {
            char_of_target[ti] = char_idx;
            ti += 1;
        }
        for (i, _) in raw.iter().enumerate() {
            self.matches.push((char_of_target[i * 2], char_of_target[i * 2 + 1]));
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

    #[test]
    fn multibyte_offsets_are_char_ranges_not_byte_ranges() {
        // "é" is two bytes; matches must be reported in char offsets. "café" then "café".
        let mut f = FindState::new(false);
        f.query = "café".into();
        f.recompute("café café", 0);
        // char offsets: first "café" = [0,4); space at 4; second "café" = [5,9).
        assert_eq!(f.matches, vec![(0, 4), (5, 9)]);
    }

    #[test]
    fn match_count_is_capped() {
        let mut f = FindState::new(false);
        f.regex = true;
        f.query = ".".into(); // matches every char
        let text = "a".repeat(MAX_MATCHES + 500);
        f.recompute(&text, 0);
        assert_eq!(f.matches.len(), MAX_MATCHES);
    }
}
