//! Per-document incremental highlighter and its span-computation helpers.

use std::collections::HashMap;

use editor_core::SyntaxEdit;
use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, Tree};

use crate::lang::lang_config;
use crate::rope_provider::RopeProvider;
use crate::HighlightSpan;

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
