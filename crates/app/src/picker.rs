//! The fuzzy picker overlay — one component parameterized by its item source, backing both
//! the command palette (`Ctrl+Shift+P`) and quick open (`Ctrl+P`), plus a Go-to-Line prompt
//! (plan §5). A tiny built-in fuzzy matcher scores and ranks candidates.

/// What the picker is choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// Command ids (built-in + plugin-contributed).
    Command,
    /// Project files; `id` is the absolute path.
    File,
    /// A line-number prompt; `query` holds the digits.
    GotoLine,
    /// LSP completion candidates; `id` holds the text to insert.
    Completion,
}

/// One selectable row.
#[derive(Debug, Clone)]
pub struct PickerItem {
    pub id: String,
    pub label: String,
}

pub struct Picker {
    pub kind: PickerKind,
    pub prompt: String,
    pub query: String,
    pub items: Vec<PickerItem>,
    /// Indices into `items` that pass the current filter, best first.
    pub filtered: Vec<usize>,
    pub selected: usize,
}

impl Picker {
    pub fn new(kind: PickerKind, prompt: &str, items: Vec<PickerItem>) -> Picker {
        let mut p = Picker {
            kind,
            prompt: prompt.to_string(),
            query: String::new(),
            items,
            filtered: Vec::new(),
            selected: 0,
        };
        p.refilter();
        p
    }

    /// Recompute the filtered/ranked list for the current query.
    pub fn refilter(&mut self) {
        if self.kind == PickerKind::GotoLine {
            self.filtered.clear();
            self.selected = 0;
            return;
        }
        let mut scored: Vec<(usize, i64)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| fuzzy_score(&self.query, &item.label).map(|s| (i, s)))
            .collect();
        // Higher score first; ties keep original order (stable sort).
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        self.selected = 0;
    }

    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.filtered.get(self.selected).map(|&i| &self.items[i])
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let n = self.filtered.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    pub fn input_char(&mut self, ch: char) {
        self.query.push(ch);
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }
}

/// Subsequence fuzzy score: `None` if `query` isn't a subsequence of `text`; otherwise a
/// score rewarding contiguous runs and word-boundary matches (higher is better).
pub fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let t: Vec<char> = text.chars().collect();
    let mut qi = 0;
    let mut score = 0i64;
    let mut last: isize = -2;
    for (ti, &ch) in t.iter().enumerate() {
        if qi < q.len() && ch.to_ascii_lowercase() == q[qi] {
            score += if ti as isize == last + 1 { 10 } else { 1 };
            let word_start = ti == 0 || !t[ti - 1].is_alphanumeric();
            if word_start {
                score += 8;
            }
            last = ti as isize;
            qi += 1;
        }
    }
    if qi == q.len() {
        // Slight penalty for longer candidates so tighter matches rank first.
        Some(score - (t.len() as i64) / 8)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_score("abc", "aXbXc").is_some());
        assert!(fuzzy_score("abc", "acb").is_none());
    }

    #[test]
    fn contiguous_and_word_start_rank_higher() {
        let contiguous = fuzzy_score("save", "File: Save").unwrap();
        let scattered = fuzzy_score("save", "s a v e scattered").unwrap();
        assert!(contiguous > scattered, "{contiguous} !> {scattered}");
    }

    #[test]
    fn picker_ranks_and_selects() {
        let items = vec![
            PickerItem {
                id: "1".into(),
                label: "Reload Window".into(),
            },
            PickerItem {
                id: "2".into(),
                label: "File: Save".into(),
            },
        ];
        let mut p = Picker::new(PickerKind::Command, "Command", items);
        p.query = "save".into();
        p.refilter();
        assert_eq!(p.selected_item().unwrap().id, "2");
    }
}
