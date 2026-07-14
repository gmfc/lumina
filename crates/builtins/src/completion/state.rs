//! The pure completion matcher: the live [`CompletionState`] filter model, its scoring, and the
//! `CompletionItemKind` glyph table. Depends only on the primitive [`LspCompletionItem`], so it
//! unit-tests without a `Host`.

use editor_plugin::LspCompletionItem;

/// A short glyph/abbreviation for an LSP `CompletionItemKind` (plan §2.1 "show kind").
pub(crate) fn kind_label(kind: Option<u8>) -> &'static str {
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

/// A live completion session: the server's items, the subset matching what's been typed since the
/// trigger, and the current selection. Pure (only depends on the item type), so it unit-tests.
pub(crate) struct CompletionState {
    pub(crate) items: Vec<LspCompletionItem>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) selected: usize,
    /// Char offset where the replaced identifier prefix starts — the popup anchor.
    pub(crate) anchor: usize,
    /// The identifier text typed between `anchor` and the caret; drives filtering.
    pub(crate) prefix: String,
}

impl CompletionState {
    pub(crate) fn new(
        items: Vec<LspCompletionItem>,
        anchor: usize,
        prefix: String,
    ) -> CompletionState {
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

    /// Recompute `filtered` from `prefix`: prefix matches rank before subsequence hits. Resets the
    /// selection.
    pub(crate) fn refilter(&mut self) {
        let prefix = self.prefix.to_lowercase();
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| score(&it.label, &prefix).map(|s| (s, i)))
            .collect();
        scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
    }

    pub(crate) fn selected_item(&self) -> Option<&LspCompletionItem> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    pub(crate) fn move_sel(&mut self, delta: isize) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let n = n as isize;
        self.selected = (((self.selected as isize + delta) % n + n) % n) as usize;
    }

    pub(crate) fn is_empty(&self) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, kind: Option<u8>) -> LspCompletionItem {
        LspCompletionItem {
            label: label.to_string(),
            detail: None,
            insert_text: label.to_string(),
            kind,
            additional_edits: Vec::new(),
            is_snippet: false,
            data: None,
            command: None,
        }
    }

    fn items() -> Vec<LspCompletionItem> {
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
        assert_eq!(s.selected, 3);
        s.move_sel(1);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn no_match_is_empty() {
        let s = CompletionState::new(items(), 0, "zzz".to_string());
        assert!(s.is_empty());
    }
}
