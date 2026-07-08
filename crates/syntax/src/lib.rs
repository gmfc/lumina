//! `editor_syntax` — tree-sitter parsing + highlight-query → capture-name spans.
//!
//! A per-document [`DocHighlighter`] parses the rope, then runs the language's highlights
//! query over **only the visible byte range** (plan §4 perf) and returns per-line spans
//! carrying capture names. The app maps capture names to colors via its theme — this crate
//! stays UI-free (no ratatui).
#![forbid(unsafe_code)]

use std::collections::HashMap;

use editor_core::SyntaxEdit;
use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    InputEdit, Language, Node, Parser, Point, Query, QueryCursor, TextProvider, Tree,
};

/// A highlighted span within a single line: `[start, end)` char offsets **within the line**,
/// carrying the tree-sitter capture name (e.g. `"keyword"`, `"function"`, `"string.special"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub capture: String,
}

/// Language id → grammar + highlights query. Returns `None` for unsupported languages.
///
/// Grammar crates are decoupled from the tree-sitter runtime version (they only provide a
/// `LanguageFn` + query text), so new languages are a table entry, not a version bump.
fn lang_config(id: &str) -> Option<(Language, String)> {
    let (lang, query): (Language, String) = match id {
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
        ),
        // A compact, version-independent JSON highlights query.
        "json" => (
            tree_sitter_json::LANGUAGE.into(),
            r#"
            (pair key: (string) @property)
            (string) @string
            (number) @number
            [(true) (false)] @constant.builtin
            (null) @constant.builtin
            (comment) @comment
            ["," ":"] @punctuation.delimiter
            ["{" "}" "[" "]"] @punctuation.bracket
            "#
            .to_string(),
        ),
        "python" => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
        ),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY.to_string(),
        ),
        // TypeScript's grammar is a JS superset; its highlights build on the JS query.
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
        ),
        "c" => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY.to_string(),
        ),
        "go" => (
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY.to_string(),
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_string(),
        ),
        // Markdown's grammar is split block/inline; we highlight the block layer.
        "markdown" => (
            tree_sitter_md::LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_string(),
        ),
        _ => return None,
    };
    Some((lang, query))
}

/// True if a language id has a grammar wired in.
pub fn is_supported(lang_id: &str) -> bool {
    lang_config(lang_id).is_some()
}

/// Per-document incremental highlighter: owns the parser + last tree, caches visible spans.
pub struct DocHighlighter {
    query: Query,
    parser: Parser,
    tree: Option<Tree>,
    /// Document revision the current `tree` reflects.
    tree_revision: u64,
    /// Cached spans keyed by line, valid for `[cached_first, cached_last]` at `cache_revision`.
    cache: HashMap<usize, Vec<HighlightSpan>>,
    cache_revision: u64,
    cached_first: usize,
    cached_last: usize,
    primed: bool,
}

impl DocHighlighter {
    /// Build a highlighter for `lang_id`, or `None` if unsupported.
    pub fn new(lang_id: &str) -> Option<DocHighlighter> {
        let (language, query_src) = lang_config(lang_id)?;
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let query = Query::new(&language, &query_src).ok()?;
        Some(DocHighlighter {
            query,
            parser,
            tree: None,
            tree_revision: u64::MAX,
            cache: HashMap::new(),
            cache_revision: u64::MAX,
            cached_first: 1,
            cached_last: 0,
            primed: false,
        })
    }

    /// Ensure spans for lines `[first, last]` are cached for document `revision`. Reparses
    /// only when the revision changed — **incrementally** when `edits` faithfully describe the
    /// change since the last parse (`edits_valid`), otherwise from scratch. Recomputes spans
    /// only when the range or revision moved.
    pub fn ensure(
        &mut self,
        rope: &Rope,
        revision: u64,
        edits: &[SyntaxEdit],
        edits_valid: bool,
        first: usize,
        last: usize,
    ) {
        if self.tree_revision != revision || !self.primed {
            if edits_valid && self.primed {
                // Age the old tree forward through each edit so tree-sitter can reuse subtrees.
                if let Some(tree) = &mut self.tree {
                    for e in edits {
                        tree.edit(&to_input_edit(e));
                    }
                }
            } else {
                self.tree = None; // force a full reparse
            }
            self.reparse(rope);
            self.tree_revision = revision;
        }
        let range_covered = self.cache_revision == revision
            && first >= self.cached_first
            && last <= self.cached_last;
        if !range_covered {
            // Recompute with a small margin so small scrolls reuse the cache.
            let margin = 32;
            let cfirst = first.saturating_sub(margin);
            let clast = (last + margin).min(rope.len_lines().saturating_sub(1));
            self.compute(rope, cfirst, clast);
            self.cache_revision = revision;
            self.cached_first = cfirst;
            self.cached_last = clast;
        }
    }

    /// Spans for `line` from the current cache (empty if none / out of cached range).
    pub fn line_spans(&self, line: usize) -> &[HighlightSpan] {
        self.cache.get(&line).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn reparse(&mut self, rope: &Rope) {
        // Reuse the previous (edit-aged) tree when present so tree-sitter reparses only the
        // changed subtrees; a `None` old tree yields a full parse.
        let old = self.tree.take();
        let tree = self.parser.parse_with_options(
            &mut |byte, _| {
                if byte >= rope.len_bytes() {
                    return &[][..];
                }
                let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
                &chunk.as_bytes()[byte - chunk_start..]
            },
            old.as_ref(),
            None,
        );
        self.tree = tree;
        self.primed = true;
        // Invalidate span cache.
        self.cache.clear();
        self.cache_revision = u64::MAX;
    }

    fn compute(&mut self, rope: &Rope, first: usize, last: usize) {
        self.cache.clear();
        let Some(tree) = &self.tree else {
            return;
        };
        let len_lines = rope.len_lines();
        if first >= len_lines {
            return;
        }
        let start_byte = rope.line_to_byte(first);
        let end_byte = rope.line_to_byte((last + 1).min(len_lines));

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);
        let names = self.query.capture_names();

        let mut it = cursor.matches(&self.query, tree.root_node(), RopeProvider(rope));
        while let Some(m) = it.next() {
            for cap in m.captures {
                let name = names[cap.index as usize];
                push_span(&mut self.cache, rope, cap.node, name, first, last);
            }
        }
    }
}

/// Convert a core [`SyntaxEdit`] into tree-sitter's `InputEdit`.
fn to_input_edit(e: &SyntaxEdit) -> InputEdit {
    InputEdit {
        start_byte: e.start_byte,
        old_end_byte: e.old_end_byte,
        new_end_byte: e.new_end_byte,
        start_position: Point::new(e.start_point.0, e.start_point.1),
        old_end_position: Point::new(e.old_end_point.0, e.old_end_point.1),
        new_end_position: Point::new(e.new_end_point.0, e.new_end_point.1),
    }
}

/// Split a node's byte range into per-line char spans and insert into the cache.
fn push_span(
    cache: &mut HashMap<usize, Vec<HighlightSpan>>,
    rope: &Rope,
    node: Node,
    capture: &str,
    first: usize,
    last: usize,
) {
    let r = node.byte_range();
    if r.start >= r.end {
        return;
    }
    let s_char = rope.byte_to_char(r.start);
    let e_char = rope.byte_to_char(r.end);
    let s_line = rope.char_to_line(s_char);
    let e_line = rope.char_to_line(e_char.saturating_sub(1).max(s_char));

    for line in s_line.max(first)..=e_line.min(last) {
        let line_start = rope.line_to_char(line);
        let line_slice = rope.line(line);
        let mut line_len = line_slice.len_chars();
        // Exclude trailing newline chars from the span's line extent.
        if line_len > 0 && line_slice.char(line_len - 1) == '\n' {
            line_len -= 1;
            if line_len > 0 && line_slice.char(line_len - 1) == '\r' {
                line_len -= 1;
            }
        }
        let line_end = line_start + line_len;
        let seg_start = s_char.max(line_start).saturating_sub(line_start);
        let seg_end = e_char.min(line_end).saturating_sub(line_start);
        if seg_start < seg_end {
            cache.entry(line).or_default().push(HighlightSpan {
                start: seg_start,
                end: seg_end,
                capture: capture.to_string(),
            });
        }
    }
}

/// Feeds rope chunks to tree-sitter's query cursor without allocating the whole document.
struct RopeProvider<'a>(&'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = ChunksBytes<'a>;
    fn text(&mut self, node: Node) -> Self::I {
        let slice = self.0.byte_slice(node.byte_range());
        ChunksBytes {
            chunks: slice.chunks(),
        }
    }
}

struct ChunksBytes<'a> {
    chunks: ropey::iter::Chunks<'a>,
}

impl<'a> Iterator for ChunksBytes<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        self.chunks.next().map(str::as_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_keyword_and_string() {
        let src = "fn main() {\n    let s = \"hi\";\n}\n";
        let rope = Rope::from_str(src);
        let mut h = DocHighlighter::new("rust").expect("rust supported");
        h.ensure(&rope, 1, &[], true, 0, rope.len_lines() - 1);
        // Line 0 contains the `fn` keyword.
        let l0 = h.line_spans(0);
        assert!(
            l0.iter().any(|s| s.capture.starts_with("keyword")),
            "expected a keyword on line 0, got {l0:?}"
        );
        // Line 1 contains the string literal "hi".
        let l1 = h.line_spans(1);
        assert!(
            l1.iter().any(|s| s.capture.starts_with("string")),
            "expected a string on line 1, got {l1:?}"
        );
    }

    #[test]
    fn unsupported_language_is_none() {
        assert!(DocHighlighter::new("cobol").is_none());
        assert!(is_supported("rust"));
        assert!(!is_supported("cobol"));
    }

    /// Every wired grammar must actually load (ABI-compatible with the tree-sitter runtime)
    /// and its highlights query must compile — otherwise `DocHighlighter::new` returns None.
    #[test]
    fn all_wired_grammars_load_and_highlight() {
        let cases = [
            (
                "python",
                "def f():\n    x = \"hi\"\n",
                "line 0 has a keyword",
            ),
            ("javascript", "const x = \"hi\";\n", "line 0 has a keyword"),
            (
                "typescript",
                "const x: number = 1;\n",
                "line 0 has a keyword",
            ),
            ("c", "int main() { return 0; }\n", "line 0 has a keyword"),
            ("go", "package main\n", "line 0 has a keyword"),
            ("toml", "[table]\nkey = 42\n", "line 1 has a number"),
            ("markdown", "# Title\n", "loads"),
        ];
        for (lang, src, _why) in cases {
            let mut h = DocHighlighter::new(lang)
                .unwrap_or_else(|| panic!("grammar `{lang}` failed to load / compile query"));
            let rope = Rope::from_str(src);
            h.ensure(&rope, 1, &[], true, 0, rope.len_lines() - 1);
            // At least one span somewhere proves the query ran against a real parse tree.
            let any = (0..rope.len_lines()).any(|l| !h.line_spans(l).is_empty());
            assert!(
                any,
                "grammar `{lang}` produced no highlight spans for: {src:?}"
            );
        }
    }

    #[test]
    fn incremental_matches_full_reparse() {
        // Parse once, then apply an incremental edit and confirm the spans match a highlighter
        // that parsed the edited text from scratch.
        let before = "fn main() {\n    let x = 1;\n}\n";
        let after = "fn main() {\n    let yy = 1;\n}\n";

        let mut inc = DocHighlighter::new("rust").unwrap();
        inc.ensure(&Rope::from_str(before), 1, &[], true, 0, 2);

        // Edit: replace "x" (line 1, col bytes 8..9) with "yy".
        let edit = SyntaxEdit {
            start_byte: 20,
            old_end_byte: 21,
            new_end_byte: 22,
            start_point: (1, 8),
            old_end_point: (1, 9),
            new_end_point: (1, 10),
        };
        inc.ensure(&Rope::from_str(after), 2, &[edit], true, 0, 2);

        let mut full = DocHighlighter::new("rust").unwrap();
        full.ensure(&Rope::from_str(after), 1, &[], true, 0, 2);

        for line in 0..3 {
            assert_eq!(
                inc.line_spans(line),
                full.line_spans(line),
                "line {line} spans diverged after incremental edit"
            );
        }
    }

    #[test]
    fn json_highlights_keys_and_numbers() {
        let src = "{\n  \"n\": 42\n}\n";
        let rope = Rope::from_str(src);
        let mut h = DocHighlighter::new("json").unwrap();
        h.ensure(&rope, 1, &[], true, 0, rope.len_lines() - 1);
        let l1 = h.line_spans(1);
        assert!(l1.iter().any(|s| s.capture.starts_with("property")));
        assert!(l1.iter().any(|s| s.capture.starts_with("number")));
    }
}
