//! Undo/redo through the edit layer, installing each restored selection set.

use crate::document::Document;

/// Undo one revision; installs the restored selection set.
pub fn undo(doc: &mut Document) -> bool {
    let mut hist = std::mem::take(&mut doc.history);
    let restored = hist.undo(doc);
    doc.history = hist;
    if let Some(sel) = restored {
        doc.selections = sel;
        doc.dirty = true;
        true
    } else {
        false
    }
}

/// Redo one revision.
pub fn redo(doc: &mut Document) -> bool {
    let mut hist = std::mem::take(&mut doc.history);
    let restored = hist.redo(doc);
    doc.history = hist;
    if let Some(sel) = restored {
        doc.selections = sel;
        doc.dirty = true;
        true
    } else {
        false
    }
}
