//! Shared low-level rendering helpers: the chrome palette, cell/string buffer writers, and
//! the syntax/diagnostic resolution used by the editor and status renderers.

use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthChar;

use editor_syntax::HighlightSpan;

use crate::theme::Theme;

pub(super) const CLR_BG: Color = Color::Reset;
pub(super) const CLR_SEL: Color = Color::Rgb(50, 60, 90);
pub(super) const CLR_ACCENT: Color = Color::Rgb(90, 130, 210);

/// Resolve syntax spans into a per-char style buffer, **reusing the caller's `styles` and
/// `best_len` scratch** so the per-line render loop allocates nothing per line (finding #4).
/// Both buffers are cleared and resized to `line_len`; on return `styles[i]` holds the winning
/// style for char `i`.
///
/// For overlapping spans the **shortest** (most specific) wins. On a length *tie*, the later
/// span wins, matching tree-sitter's "later/more-specific pattern overrides" convention (e.g.
/// `@variable.builtin` captured over the same node as `@variable`, where the builtin pattern
/// comes later in the query).
pub(super) fn resolve_line_styles_into(
    spans: &[HighlightSpan],
    line_len: usize,
    theme: &Theme,
    styles: &mut Vec<Option<Style>>,
    best_len: &mut Vec<usize>,
) {
    styles.clear();
    styles.resize(line_len, None);
    best_len.clear();
    best_len.resize(line_len, usize::MAX);
    for span in spans {
        let Some(style) = theme.style_for(&span.capture) else {
            continue;
        };
        let len = span.end.saturating_sub(span.start);
        for i in span.start..span.end.min(line_len) {
            if len <= best_len[i] {
                best_len[i] = len;
                styles[i] = Some(style);
            }
        }
    }
}

/// Allocating twin of [`resolve_line_styles_into`] — kept only for the finding-#4 timing A/B
/// (it reproduces the pre-optimization "two Vecs per line" behavior). Production renders go
/// through `_into` with reused scratch.
#[cfg(test)]
pub(super) fn resolve_line_styles(
    spans: &[HighlightSpan],
    line_len: usize,
    theme: &Theme,
) -> Vec<Option<Style>> {
    let mut styles: Vec<Option<Style>> = Vec::new();
    let mut best_len: Vec<usize> = Vec::new();
    resolve_line_styles_into(spans, line_len, theme, &mut styles, &mut best_len);
    styles
}

pub(super) fn char_cells(ch: char, col: usize, tab_width: usize) -> usize {
    if ch == '\t' {
        let tw = tab_width.max(1);
        tw - (col % tw)
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(1).max(1)
    }
}

pub(super) fn cell_at(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
) -> Option<&mut ratatui::buffer::Cell> {
    if x < buf.area.right() && y < buf.area.bottom() && x >= buf.area.left() && y >= buf.area.top()
    {
        Some(&mut buf[(x, y)])
    } else {
        None
    }
}

pub(super) fn put_str(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    s: &str,
    style: Style,
    max_x: u16,
) {
    for (cx, ch) in (x..).zip(s.chars()) {
        if cx >= max_x {
            break;
        }
        if let Some(cell) = cell_at(buf, cx, y) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

pub(super) fn display_len(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(1).max(1))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_len_counts_wide_chars() {
        assert_eq!(display_len("ab"), 2);
        assert_eq!(display_len("世界"), 4); // two wide (2-cell) chars
    }

    fn sample_spans() -> Vec<HighlightSpan> {
        // A realistic ~60-char line's worth of syntax spans (keywords, idents, strings, …).
        [
            (0, 3, "keyword"),
            (4, 12, "function"),
            (12, 13, "punctuation"),
            (13, 20, "variable"),
            (21, 22, "operator"),
            (23, 35, "string"),
            (36, 40, "type"),
            (41, 55, "comment"),
        ]
        .iter()
        .map(|&(start, end, capture)| HighlightSpan {
            start,
            end,
            capture: capture.to_string(),
        })
        .collect()
    }

    /// The scratch-reusing `_into` must produce byte-for-byte the same per-char styles as the
    /// allocating wrapper (finding #4 correctness): same shortest-span-wins resolution, and the
    /// reused buffers must be fully reset between lines of differing length.
    #[test]
    fn resolve_into_matches_allocating_and_resets_scratch() {
        let theme = crate::theme::Theme::default_dark(true);
        let spans = sample_spans();
        let (mut styles, mut best) = (Vec::new(), Vec::new());
        for len in [60usize, 10, 80, 0, 42] {
            resolve_line_styles_into(&spans, len, &theme, &mut styles, &mut best);
            assert_eq!(
                styles,
                resolve_line_styles(&spans, len, &theme),
                "len {len}"
            );
            assert_eq!(styles.len(), len);
        }
    }

    /// Release-timing A/B for finding #4 (reuse style scratch buffers). Behind the `perfbench`
    /// feature (coverage build skips it) and ignored by default; run with
    /// `cargo test -p lumina --features perfbench --release -- --ignored --nocapture bench_resolve`.
    #[cfg(feature = "perfbench")]
    #[test]
    #[ignore = "timing harness; run explicitly with --ignored --nocapture"]
    fn bench_resolve_alloc_vs_scratch() {
        use std::hint::black_box;
        use std::time::Instant;

        let theme = crate::theme::Theme::default_dark(true);
        let spans = sample_spans();
        const LINES: usize = 60; // one viewport
        const REPS: usize = 5000; // frames

        // Before: two fresh Vecs per line (the pre-optimization behavior).
        let t0 = Instant::now();
        let mut sink = 0usize;
        for _ in 0..REPS {
            for _ in 0..LINES {
                let styles = resolve_line_styles(&spans, 60, &theme);
                sink += black_box(styles.len());
            }
        }
        let allocating = t0.elapsed();

        // After: reuse one pair of scratch buffers across every line and frame.
        let (mut styles, mut best) = (Vec::new(), Vec::new());
        let t1 = Instant::now();
        for _ in 0..REPS {
            for _ in 0..LINES {
                resolve_line_styles_into(&spans, 60, &theme, &mut styles, &mut best);
                sink += black_box(styles.len());
            }
        }
        let scratch = t1.elapsed();

        black_box(sink);
        let calls = (REPS * LINES) as u128;
        println!(
            "resolve_line_styles (alloc):   {allocating:?}  ({} ns/line)\n\
             resolve_line_styles_into (reuse): {scratch:?}  ({} ns/line)",
            allocating.as_nanos() / calls,
            scratch.as_nanos() / calls,
        );
    }
}
