//! Completion popup state (plan §2.1). A caret-anchored floating list over the editor:
//! the state lives here (pure, testable), the input handling in `app.rs`, and the render in
//! `ui.rs` — so `render` stays a pure function of this state (invariant #2).

use editor_lsp::CompletionItem;

/// A live completion session: the server's items, the subset matching what's been typed since
/// the trigger, and the current selection.
pub struct CompletionState {
    /// Every candidate the server returned.
    pub items: Vec<CompletionItem>,
    /// Indices into `items` matching `prefix`, best-first.
    pub filtered: Vec<usize>,
    /// Highlighted row within `filtered`.
    pub selected: usize,
    /// Char offset where the replaced identifier prefix starts — the popup anchor and the
    /// start of the range an accepted item replaces.
    pub anchor: usize,
    /// The identifier text typed between `anchor` and the caret; drives filtering.
    pub prefix: String,
}

impl CompletionState {
    /// Build a session and compute the initial filtered set.
    pub fn new(items: Vec<CompletionItem>, anchor: usize, prefix: String) -> CompletionState {
        let mut s = CompletionState {
            items,
            filtered: Vec::new(),
            selected: 0,
            anchor,
            prefix,
        };
        s.refilter();
        s
    }

    /// Recompute `filtered` from `prefix`: keep items whose label prefix-matches or contains
    /// the typed prefix as a subsequence, ranking prefix matches first. Resets the selection.
    pub fn refilter(&mut self) {
        let prefix = self.prefix.to_lowercase();
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| score(&it.label, &prefix).map(|s| (s, i)))
            .collect();
        // Higher score first; stable within a score so server order is preserved.
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
    }

    /// The item under the selection, if any.
    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    /// Move the selection by `delta`, wrapping around the filtered list.
    pub fn move_sel(&mut self, delta: isize) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let n = n as isize;
        self.selected = (((self.selected as isize + delta) % n + n) % n) as usize;
    }

    /// No candidate currently matches — the caller should dismiss the popup.
    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }
}

/// Match score for `label` against a lowercase `prefix`: exact prefix beats a subsequence hit;
/// `None` means no match. An empty prefix matches everything (score 0).
fn score(label: &str, prefix: &str) -> Option<i32> {
    if prefix.is_empty() {
        return Some(0);
    }
    let label = label.to_lowercase();
    if label.starts_with(prefix) {
        Some(100)
    } else if is_subsequence(prefix, &label) {
        Some(50)
    } else {
        None
    }
}

/// True when every char of `needle` appears in `haystack` in order (a fuzzy contains).
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    for nc in needle.chars() {
        if !hay.any(|hc| hc == nc) {
            return false;
        }
    }
    true
}

/// A short glyph/abbreviation for an LSP `CompletionItemKind` (plan §2.1 "show kind").
pub fn kind_label(kind: Option<u8>) -> &'static str {
    match kind {
        Some(2) | Some(3) => "ƒ", // Method / Function
        Some(4) => "ƒ",           // Constructor
        Some(5) => ".",           // Field
        Some(6) => "x",           // Variable
        Some(7) | Some(8) => "T", // Class / Interface
        Some(9) => "M",           // Module
        Some(10) => ".",          // Property
        Some(13) => "E",          // Enum
        Some(14) => "K",          // Keyword
        Some(15) => "S",          // Snippet
        Some(21) => "C",          // Constant
        Some(22) => "T",          // Struct
        _ => "•",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, kind: Option<u8>) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            detail: None,
            insert_text: label.to_string(),
            kind,
        }
    }

    fn items() -> Vec<CompletionItem> {
        vec![
            item("println", Some(3)),
            item("print", Some(3)),
            item("eprintln", Some(3)),
            item("format", Some(3)),
        ]
    }

    #[test]
    fn empty_prefix_matches_all_in_order() {
        let s = CompletionState::new(items(), 0, String::new());
        assert_eq!(s.filtered.len(), 4);
        assert_eq!(s.selected_item().unwrap().label, "println");
    }

    #[test]
    fn prefix_matches_rank_before_subsequence() {
        let s = CompletionState::new(items(), 0, "print".to_string());
        // "println" and "print" start with the prefix; "eprintln" only contains it (subseq).
        let labels: Vec<&str> = s
            .filtered
            .iter()
            .map(|&i| s.items[i].label.as_str())
            .collect();
        assert_eq!(&labels[..2], &["println", "print"]);
        assert!(labels.contains(&"eprintln"));
        assert!(!labels.contains(&"format"));
    }

    #[test]
    fn refilter_narrows_as_prefix_grows() {
        let mut s = CompletionState::new(items(), 0, "p".to_string());
        let before = s.filtered.len();
        s.prefix = "prin".to_string();
        s.refilter();
        assert!(s.filtered.len() <= before);
        assert!(s
            .filtered
            .iter()
            .all(|&i| s.items[i].label.contains("prin")));
    }

    #[test]
    fn selection_wraps() {
        let mut s = CompletionState::new(items(), 0, String::new());
        s.move_sel(-1);
        assert_eq!(s.selected, 3); // wrapped to last
        s.move_sel(1);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn no_match_is_empty() {
        let s = CompletionState::new(items(), 0, "zzz".to_string());
        assert!(s.is_empty());
    }
}
