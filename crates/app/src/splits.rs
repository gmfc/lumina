//! Editor split-pane tree: binary H/V splits, each leaf holding a [`DocId`] and an independent
//! [`ViewState`] (scroll). The focused leaf's view is mirrored onto the document so core motions
//! keep working unchanged; focusing another pane writes the doc's view back and loads the leaf's.

use editor_core::{view::ViewState, DocId};

/// Horizontal = side-by-side (split right); vertical = stacked (split down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

/// One leaf pane: the document it shows and its independent scroll state.
#[derive(Debug, Clone)]
pub struct Pane {
    pub doc: DocId,
    pub view: ViewState,
}

/// Binary tree of editor panes. A single [`Self::Leaf`] is the default (no splits).
#[derive(Debug, Clone)]
pub enum SplitTree {
    Leaf(Pane),
    Branch {
        dir: SplitDir,
        /// Fraction of space for the first child (0.05..=0.95).
        ratio: f32,
        first: Box<SplitTree>,
        second: Box<SplitTree>,
    },
}

impl SplitTree {
    pub fn single(doc: DocId, view: ViewState) -> Self {
        SplitTree::Leaf(Pane { doc, view })
    }

    /// Number of leaf panes.
    pub fn leaf_count(&self) -> usize {
        match self {
            SplitTree::Leaf(_) => 1,
            SplitTree::Branch { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// Focus path: a sequence of `false` = first child, `true` = second child, ending at a leaf.
    pub fn focused_pane(&self, path: &[bool]) -> &Pane {
        match self.walk(path) {
            SplitTree::Leaf(p) => p,
            SplitTree::Branch { first, .. } => first.focused_pane(&[]),
        }
    }

    pub fn focused_pane_mut(&mut self, path: &[bool]) -> &mut Pane {
        self.focused_pane_mut_inner(path)
    }

    fn focused_pane_mut_inner(&mut self, path: &[bool]) -> &mut Pane {
        if path.is_empty() {
            match self {
                SplitTree::Leaf(p) => return p,
                SplitTree::Branch { first, .. } => return first.focused_pane_mut_inner(&[]),
            }
        }
        match self {
            SplitTree::Leaf(p) => p,
            SplitTree::Branch { first, second, .. } => {
                if path[0] {
                    second.focused_pane_mut_inner(&path[1..])
                } else {
                    first.focused_pane_mut_inner(&path[1..])
                }
            }
        }
    }

    fn walk<'a>(&'a self, path: &[bool]) -> &'a SplitTree {
        let mut node = self;
        for &right in path {
            match node {
                SplitTree::Leaf(_) => break,
                SplitTree::Branch { first, second, .. } => {
                    node = if right { second } else { first };
                }
            }
        }
        node
    }

    /// Split the focused leaf in `dir`, putting the new pane (same doc, copied view) as the second
    /// child. Updates `path` to point at the new pane.
    pub fn split_focused(&mut self, path: &mut Vec<bool>, dir: SplitDir) {
        let pane = self.focused_pane(path).clone();
        let new_pane = Pane {
            doc: pane.doc,
            view: pane.view.clone(),
        };
        Self::replace_at(self, path, |old| SplitTree::Branch {
            dir,
            ratio: 0.5,
            first: Box::new(old),
            second: Box::new(SplitTree::Leaf(new_pane)),
        });
        path.push(true); // focus the new (second) pane
    }

    fn replace_at(node: &mut SplitTree, path: &[bool], f: impl FnOnce(SplitTree) -> SplitTree) {
        if path.is_empty() {
            let old = std::mem::replace(
                node,
                SplitTree::Leaf(Pane {
                    doc: DocId::default(),
                    view: ViewState::default(),
                }),
            );
            *node = f(old);
            return;
        }
        match node {
            SplitTree::Leaf(_) => {
                let old = std::mem::replace(
                    node,
                    SplitTree::Leaf(Pane {
                        doc: DocId::default(),
                        view: ViewState::default(),
                    }),
                );
                *node = f(old);
            }
            SplitTree::Branch { first, second, .. } => {
                let child = if path[0] {
                    second.as_mut()
                } else {
                    first.as_mut()
                };
                Self::replace_at(child, &path[1..], f);
            }
        }
    }

    /// Close the focused pane. No-op when only one leaf remains. Updates `path` to a remaining leaf.
    pub fn close_focused(&mut self, path: &mut Vec<bool>) -> bool {
        if self.leaf_count() <= 1 || path.is_empty() {
            // Closing the sole root pane is a no-op; if path is empty we're on a single leaf.
            if self.leaf_count() <= 1 {
                return false;
            }
            // path empty but multiple leaves — shouldn't happen; treat as close first leaf of root branch.
        }
        if path.is_empty() {
            return false;
        }
        let removed = Self::collapse_at(self, path);
        if removed {
            path.clear();
        }
        removed
    }

    /// Remove the leaf at `path` by collapsing its parent into the sibling.
    fn collapse_at(node: &mut SplitTree, path: &[bool]) -> bool {
        if path.is_empty() {
            return false;
        }
        match node {
            SplitTree::Leaf(_) => false,
            SplitTree::Branch { first, second, .. } => {
                if path.len() == 1 {
                    let sibling = if path[0] {
                        // Remove second → keep first.
                        std::mem::replace(
                            first.as_mut(),
                            SplitTree::Leaf(Pane {
                                doc: DocId::default(),
                                view: ViewState::default(),
                            }),
                        )
                    } else {
                        // Remove first → keep second.
                        std::mem::replace(
                            second.as_mut(),
                            SplitTree::Leaf(Pane {
                                doc: DocId::default(),
                                view: ViewState::default(),
                            }),
                        )
                    };
                    *node = sibling;
                    true
                } else {
                    let child = if path[0] {
                        second.as_mut()
                    } else {
                        first.as_mut()
                    };
                    Self::collapse_at(child, &path[1..])
                }
            }
        }
    }

    /// Collect all leaves in left-to-right / top-to-bottom order with their focus paths.
    pub fn leaves(&self) -> Vec<(Vec<bool>, &Pane)> {
        let mut out = Vec::new();
        self.collect_leaves(&mut Vec::new(), &mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, path: &mut Vec<bool>, out: &mut Vec<(Vec<bool>, &'a Pane)>) {
        match self {
            SplitTree::Leaf(p) => out.push((path.clone(), p)),
            SplitTree::Branch { first, second, .. } => {
                path.push(false);
                first.collect_leaves(path, out);
                path.pop();
                path.push(true);
                second.collect_leaves(path, out);
                path.pop();
            }
        }
    }

    /// Lay out leaf rectangles inside `area`.
    pub fn layout(&self, area: ratatui::layout::Rect) -> Vec<(Vec<bool>, ratatui::layout::Rect)> {
        let mut out = Vec::new();
        self.layout_inner(area, &mut Vec::new(), &mut out);
        out
    }

    fn layout_inner(
        &self,
        area: ratatui::layout::Rect,
        path: &mut Vec<bool>,
        out: &mut Vec<(Vec<bool>, ratatui::layout::Rect)>,
    ) {
        match self {
            SplitTree::Leaf(_) => out.push((path.clone(), area)),
            SplitTree::Branch {
                dir,
                ratio,
                first,
                second,
            } => {
                let r = ratio.clamp(0.05, 0.95);
                let (a, b) = match dir {
                    SplitDir::Horizontal => {
                        let w = ((area.width as f32) * r).round() as u16;
                        let w = w.clamp(1, area.width.saturating_sub(1).max(1));
                        let left = ratatui::layout::Rect {
                            x: area.x,
                            y: area.y,
                            width: w,
                            height: area.height,
                        };
                        let right = ratatui::layout::Rect {
                            x: area.x.saturating_add(w),
                            y: area.y,
                            width: area.width.saturating_sub(w),
                            height: area.height,
                        };
                        (left, right)
                    }
                    SplitDir::Vertical => {
                        let h = ((area.height as f32) * r).round() as u16;
                        let h = h.clamp(1, area.height.saturating_sub(1).max(1));
                        let top = ratatui::layout::Rect {
                            x: area.x,
                            y: area.y,
                            width: area.width,
                            height: h,
                        };
                        let bot = ratatui::layout::Rect {
                            x: area.x,
                            y: area.y.saturating_add(h),
                            width: area.width,
                            height: area.height.saturating_sub(h),
                        };
                        (top, bot)
                    }
                };
                path.push(false);
                first.layout_inner(a, path, out);
                path.pop();
                path.push(true);
                second.layout_inner(b, path, out);
                path.pop();
            }
        }
    }

    /// Advance focus to the next/previous leaf (cyclic).
    pub fn focus_delta(path: &mut Vec<bool>, leaf_paths: &[Vec<bool>], forward: bool) {
        if leaf_paths.is_empty() {
            return;
        }
        let cur = leaf_paths.iter().position(|p| p == path).unwrap_or(0);
        let next = if forward {
            (cur + 1) % leaf_paths.len()
        } else {
            (cur + leaf_paths.len() - 1) % leaf_paths.len()
        };
        *path = leaf_paths[next].clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{Document, Workspace};

    fn dummy_doc() -> DocId {
        let mut ws = Workspace::new(std::path::PathBuf::from("/tmp"));
        ws.open_document(Document::from_str(""))
    }

    #[test]
    fn split_and_close_round_trip() {
        let doc = dummy_doc();
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Horizontal);
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(path, vec![true]);
        assert!(tree.close_focused(&mut path));
        assert_eq!(tree.leaf_count(), 1);
        assert!(!tree.close_focused(&mut path));
    }

    #[test]
    fn layout_splits_area() {
        let doc = dummy_doc();
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Horizontal);
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        let laid = tree.layout(area);
        assert_eq!(laid.len(), 2);
        assert_eq!(laid[0].1.width + laid[1].1.width, 100);
    }

    #[test]
    fn vertical_layout_stacks_heights() {
        let doc = dummy_doc();
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Vertical);
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 40,
        };
        let laid = tree.layout(area);
        assert_eq!(laid.len(), 2);
        assert_eq!(laid[0].1.height + laid[1].1.height, 40);
        assert_eq!(laid[0].1.width, 80);
    }

    #[test]
    fn focus_delta_cycles_leaves() {
        let doc = dummy_doc();
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Horizontal);
        tree.split_focused(&mut path, SplitDir::Vertical); // 3 leaves
        let paths: Vec<Vec<bool>> = tree.leaves().into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths.len(), 3);
        let mut focus = paths[0].clone();
        SplitTree::focus_delta(&mut focus, &paths, true);
        assert_eq!(focus, paths[1]);
        SplitTree::focus_delta(&mut focus, &paths, true);
        assert_eq!(focus, paths[2]);
        SplitTree::focus_delta(&mut focus, &paths, true);
        assert_eq!(focus, paths[0]); // wrap
        SplitTree::focus_delta(&mut focus, &paths, false);
        assert_eq!(focus, paths[2]);
    }

    #[test]
    fn leaves_enumerate_in_order() {
        let doc = dummy_doc();
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Horizontal);
        let leaves = tree.leaves();
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].0, vec![false]);
        assert_eq!(leaves[1].0, vec![true]);
    }

    #[test]
    fn close_focused_keeps_sibling_first_or_second() {
        let doc = dummy_doc();
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Horizontal); // path → second
        assert!(tree.close_focused(&mut path));
        assert_eq!(tree.leaf_count(), 1);

        // Close the *first* child instead.
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Horizontal);
        path = vec![false];
        assert!(tree.close_focused(&mut path));
        assert_eq!(tree.leaf_count(), 1);
    }

    #[test]
    fn nested_close_collapses_inner_branch() {
        let doc = dummy_doc();
        let mut tree = SplitTree::single(doc, ViewState::default());
        let mut path = Vec::new();
        tree.split_focused(&mut path, SplitDir::Horizontal);
        tree.split_focused(&mut path, SplitDir::Vertical); // 3 leaves; path deep
        assert_eq!(tree.leaf_count(), 3);
        assert!(tree.close_focused(&mut path));
        assert_eq!(tree.leaf_count(), 2);
    }

    #[test]
    fn focused_pane_falls_back_on_stale_path() {
        let doc = dummy_doc();
        let tree = SplitTree::single(doc, ViewState::default());
        // Empty path on a leaf, and a bogus deep path, both resolve.
        let _ = tree.focused_pane(&[]);
        let _ = tree.focused_pane(&[true, false]);
    }

    #[test]
    fn focus_delta_noop_on_empty_leaf_list() {
        let mut path = vec![false];
        SplitTree::focus_delta(&mut path, &[], true);
        assert_eq!(path, vec![false]);
    }
}
