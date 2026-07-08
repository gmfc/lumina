//! The `Workspace`: open documents (with stable ids), the tab order, and the project root.
//!
//! `DocId`s survive tab reorder and close/reopen churn (a `SlotMap` key), so plugins and
//! tabs can hold onto a document across UI changes.

use std::path::{Path, PathBuf};

use slotmap::{new_key_type, SlotMap};

use crate::document::Document;

new_key_type! {
    /// Stable handle to an open [`Document`].
    pub struct DocId;
}

/// Everything the editor has open, headless.
pub struct Workspace {
    pub documents: SlotMap<DocId, Document>,
    pub tabs: Vec<DocId>,
    pub active_tab: usize,
    pub root: PathBuf,
}

impl Workspace {
    pub fn new(root: PathBuf) -> Workspace {
        Workspace {
            documents: SlotMap::with_key(),
            tabs: Vec::new(),
            active_tab: 0,
            root,
        }
    }

    /// Insert a document and open it in a new tab, returning its id and making it active.
    pub fn open_document(&mut self, doc: Document) -> DocId {
        let id = self.documents.insert(doc);
        self.tabs.push(id);
        self.active_tab = self.tabs.len() - 1;
        id
    }

    /// If `path` is already open, focus its tab and return its id.
    pub fn find_by_path(&self, path: &Path) -> Option<DocId> {
        self.tabs.iter().copied().find(|&id| {
            self.documents
                .get(id)
                .and_then(|d| d.path.as_deref())
                .map(|p| p == path)
                .unwrap_or(false)
        })
    }

    pub fn active_doc(&self) -> Option<DocId> {
        self.tabs.get(self.active_tab).copied()
    }

    pub fn active_document(&self) -> Option<&Document> {
        self.active_doc().and_then(|id| self.documents.get(id))
    }

    pub fn active_document_mut(&mut self) -> Option<&mut Document> {
        let id = self.active_doc()?;
        self.documents.get_mut(id)
    }

    pub fn focus_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
        }
    }

    /// Move the tab at `from` to index `to` (drag-to-reorder), keeping the same document
    /// focused wherever it lands.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        let n = self.tabs.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let active_id = self.tabs.get(self.active_tab).copied();
        let id = self.tabs.remove(from);
        self.tabs.insert(to, id);
        if let Some(aid) = active_id {
            if let Some(pos) = self.tabs.iter().position(|&t| t == aid) {
                self.active_tab = pos;
            }
        }
    }

    pub fn focus_doc(&mut self, id: DocId) {
        if let Some(pos) = self.tabs.iter().position(|&t| t == id) {
            self.active_tab = pos;
        }
    }

    /// Close the tab at `idx`, removing its document. Returns the removed id.
    pub fn close_tab(&mut self, idx: usize) -> Option<DocId> {
        if idx >= self.tabs.len() {
            return None;
        }
        let id = self.tabs.remove(idx);
        self.documents.remove(id);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }
        Some(id)
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_focus_close() {
        let mut ws = Workspace::new(PathBuf::from("/tmp"));
        let a = ws.open_document(Document::from_str("a"));
        let b = ws.open_document(Document::from_str("b"));
        assert_eq!(ws.active_doc(), Some(b));
        ws.focus_doc(a);
        assert_eq!(ws.active_tab, 0);
        ws.close_tab(0);
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.active_doc(), Some(b));
    }

    #[test]
    fn move_tab_reorders_and_keeps_focus() {
        let mut ws = Workspace::new(PathBuf::from("/tmp"));
        let a = ws.open_document(Document::from_str("a"));
        let b = ws.open_document(Document::from_str("b"));
        let c = ws.open_document(Document::from_str("c"));
        ws.focus_doc(a); // active is the first tab
        ws.move_tab(0, 2); // move "a" to the end
        assert_eq!(ws.tabs, vec![b, c, a]);
        assert_eq!(ws.active_doc(), Some(a)); // focus follows the moved doc
    }
}
