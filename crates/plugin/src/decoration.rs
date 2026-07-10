//! Primitive decoration types: styled char-range spans + gutter marks a plugin publishes per
//! document for the pure renderer to paint (invariant #8).
//!
//! Like panel [`crate::Span`]s, a decoration's `style` is a *semantic key* (e.g. `"find.match"`,
//! `"lsp.diag.error"`) the app's theme resolves to concrete colors — the kernel carries no
//! terminal/color types. A plugin recomputes and re-publishes its layer on the relevant events
//! (`DidChange`/`DidChangeCursor`); the app stores the layers and the renderer merges them, so
//! render stays a pure function of state.

/// A styled run over a half-open char range `[start, end)` of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoration {
    /// `(start, end)` char offsets, half-open.
    pub range: (usize, usize),
    /// Semantic style key resolved by the app's theme.
    pub style: String,
}

impl Decoration {
    pub fn new(range: (usize, usize), style: impl Into<String>) -> Self {
        Decoration {
            range,
            style: style.into(),
        }
    }
}

/// A glyph drawn in the gutter of a 0-based `line`, styled by a semantic `style` key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GutterMark {
    pub line: usize,
    pub glyph: char,
    pub style: String,
}

impl GutterMark {
    pub fn new(line: usize, glyph: char, style: impl Into<String>) -> Self {
        GutterMark {
            line,
            glyph,
            style: style.into(),
        }
    }
}

/// One named layer's decorations for a single document: char-range spans plus gutter marks.
/// Publishing a layer replaces its whole set; an empty set (or `clear_decorations`) removes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecorationSet {
    pub spans: Vec<Decoration>,
    pub gutter: Vec<GutterMark>,
}

impl DecorationSet {
    /// A layer of char-range spans (no gutter marks).
    pub fn spans(spans: Vec<Decoration>) -> Self {
        DecorationSet {
            spans,
            gutter: Vec::new(),
        }
    }

    /// True when the layer paints nothing.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty() && self.gutter.is_empty()
    }
}
