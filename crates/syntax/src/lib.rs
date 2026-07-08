//! `editor_syntax` — tree-sitter parsing + highlight-query → capture-name spans.
//!
//! A per-document [`DocHighlighter`] parses the rope, then runs the language's highlights
//! query over **only the visible byte range** (plan §4 perf) and returns per-line spans
//! carrying capture names. The app maps capture names to colors via its theme — this crate
//! stays UI-free (no ratatui).
#![forbid(unsafe_code)]

mod highlighter;
mod lang;
mod rope_provider;

#[cfg(test)]
mod tests;

pub use highlighter::DocHighlighter;
pub use lang::is_supported;

/// A highlighted span within a single line: `[start, end)` char offsets **within the line**,
/// carrying the tree-sitter capture name (e.g. `"keyword"`, `"function"`, `"string.special"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub capture: String,
}
