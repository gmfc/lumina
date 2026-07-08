//! Tree-sitter `TextProvider` glue that streams rope chunks without allocating the whole document.

use ropey::Rope;
use tree_sitter::{Node, TextProvider};

/// Feeds rope chunks to tree-sitter's query cursor without allocating the whole document.
pub(crate) struct RopeProvider<'a>(pub(crate) &'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = ChunksBytes<'a>;
    fn text(&mut self, node: Node) -> Self::I {
        let slice = self.0.byte_slice(node.byte_range());
        ChunksBytes {
            chunks: slice.chunks(),
        }
    }
}

pub(crate) struct ChunksBytes<'a> {
    chunks: ropey::iter::Chunks<'a>,
}

impl<'a> Iterator for ChunksBytes<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        self.chunks.next().map(str::as_bytes)
    }
}
