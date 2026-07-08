use super::*;
use crate::document::Document;
use crate::history::GroupBreak;
use crate::pairs::PairTable;
use crate::selection::{Selection, Selections};

mod basic;
mod hygiene;
mod indent;
mod linewise;
mod pairs;

/// Collapse the document's selection set to carets at each of `positions`.
fn multi_caret(doc: &mut Document, positions: &[usize]) {
    let sels: Vec<Selection> = positions.iter().map(|&p| Selection::caret(p)).collect();
    doc.selections = Selections::from_iter(sels);
}

/// The default pair table used across the auto-pair/auto-indent tests.
fn pt() -> PairTable {
    PairTable::default()
}
