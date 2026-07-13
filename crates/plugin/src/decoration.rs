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

/// A run of non-document "virtual" text rendered inline in a document line — inlay hints (§7.2)
/// and code lens (§6.4). It anchors just before the character at `offset` (at a line's end offset
/// it trails that line), displacing the real text on screen without changing the buffer. `pad_left`
/// / `pad_right` add a surrounding space (LSP inlay `paddingLeft`/`paddingRight`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualText {
    /// Char offset the text anchors before.
    pub offset: usize,
    pub text: String,
    /// Semantic style key resolved by the app's theme (e.g. `"lsp.inlay.type"`, `"lsp.lens"`).
    pub style: String,
    pub pad_left: bool,
    pub pad_right: bool,
}

impl VirtualText {
    pub fn new(offset: usize, text: impl Into<String>, style: impl Into<String>) -> Self {
        VirtualText {
            offset,
            text: text.into(),
            style: style.into(),
            pad_left: false,
            pad_right: false,
        }
    }

    /// The rendered text including padding spaces, as it appears on screen.
    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.pad_left {
            s.push(' ');
        }
        s.push_str(&self.text);
        if self.pad_right {
            s.push(' ');
        }
        s
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
    /// Inline virtual text (inlay hints / code lens) anchored at char offsets.
    pub virtual_text: Vec<VirtualText>,
}

impl DecorationSet {
    /// A layer of char-range spans (no gutter marks or virtual text).
    pub fn spans(spans: Vec<Decoration>) -> Self {
        DecorationSet {
            spans,
            ..Default::default()
        }
    }

    /// A layer of inline virtual text only (inlay hints / code lens).
    pub fn virtual_text(virtual_text: Vec<VirtualText>) -> Self {
        DecorationSet {
            virtual_text,
            ..Default::default()
        }
    }

    /// True when the layer paints nothing.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty() && self.gutter.is_empty() && self.virtual_text.is_empty()
    }
}
