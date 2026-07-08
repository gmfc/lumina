//! `editor_syntax` — tree-sitter parsing + highlight-query → theme-key mapping.
//!
//! Phase 0 ships the data types the editor pane consumes; Phase 5 fills in the
//! tree-sitter engine (incremental, cached, viewport-only).
#![forbid(unsafe_code)]

/// A highlighted span within a single line: `[start_col, end_col)` in char offsets,
/// carrying a semantic capture name the theme maps to colors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub capture: String,
}

/// Trait for anything that can produce per-line highlight spans. The built-in
/// tree-sitter highlighter (Phase 5) implements this; until then a no-op suffices.
pub trait Highlighter {
    /// Highlight spans for `line_text` (a single logical line, no newline).
    fn highlight_line(&mut self, line: usize, line_text: &str) -> Vec<HighlightSpan>;
}

/// A highlighter that produces no spans (plain text). Used until Phase 5 lands.
#[derive(Default)]
pub struct PlainHighlighter;

impl Highlighter for PlainHighlighter {
    fn highlight_line(&mut self, _line: usize, _line_text: &str) -> Vec<HighlightSpan> {
        Vec::new()
    }
}
