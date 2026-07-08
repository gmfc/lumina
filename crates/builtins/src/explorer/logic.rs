//! The explorer's inherent behavior: tree walking, flattening, selection, and rendering.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use editor_plugin::host::PanelContent;
use editor_plugin::{Host, PanelLine, Span};
use ignore::WalkBuilder;

use super::model::{file_glyph, Row};
use super::{ExplorerPlugin, PANEL};

impl ExplorerPlugin {
    /// Build an explorer, optionally rendering Nerd Font glyphs.
    pub fn new(icons: bool) -> Self {
        ExplorerPlugin {
            root: PathBuf::new(),
            expanded: BTreeSet::new(),
            visible: Vec::new(),
            selected: 0,
            icons,
        }
    }

    /// List a directory's children, honoring `.gitignore` (plan §6: `ignore`-walked).
    /// Dotfiles are shown (VS Code-style), but ignored/`.git` content is hidden.
    fn list_children(dir: &Path) -> Vec<(PathBuf, bool)> {
        let mut out = Vec::new();
        let walker = WalkBuilder::new(dir)
            .max_depth(Some(1))
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.flatten() {
            if entry.depth() == 0 {
                continue; // the directory itself
            }
            let path = entry.path().to_path_buf();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            out.push((path, is_dir));
        }
        out.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.file_name().cmp(&b.0.file_name()))
        });
        out
    }

    /// Rebuild the flattened visible-row list from the expanded set.
    pub(super) fn rebuild(&mut self) {
        self.visible.clear();
        let root = self.root.clone();
        self.push_dir(&root, 0);
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
    }

    fn push_dir(&mut self, dir: &Path, depth: usize) {
        for (path, is_dir) in Self::list_children(dir) {
            let expanded = is_dir && self.expanded.contains(&path);
            self.visible.push(Row {
                path: path.clone(),
                is_dir,
                depth,
                expanded,
            });
            if expanded {
                self.push_dir(&path, depth + 1);
            }
        }
    }

    pub(super) fn render(&self, host: &mut dyn Host) {
        let lines = self
            .visible
            .iter()
            .map(|row| {
                let name = row
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let (marker, style) = if row.is_dir {
                    let m = if self.icons {
                        if row.expanded {
                            "\u{e5fe} \u{f07c} " //   open-folder
                        } else {
                            "\u{e5ff} \u{f07b} " //   folder
                        }
                    } else if row.expanded {
                        "▾ "
                    } else {
                        "▸ "
                    };
                    (m.to_string(), "dir")
                } else if self.icons {
                    (format!("  {} ", file_glyph(&row.path)), "file")
                } else {
                    ("  ".to_string(), "file")
                };
                PanelLine::new(vec![Span::new(format!("{marker}{name}"), style)])
                    .payload(row.path.to_string_lossy().into_owned())
                    .depth(row.depth)
            })
            .collect();
        host.set_panel(
            PANEL,
            PanelContent {
                lines,
                selected: self.selected,
            },
        );
    }

    pub(super) fn toggle_or_open(&mut self, path: &Path, host: &mut dyn Host) {
        let is_dir = self
            .visible
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.is_dir)
            .unwrap_or(false);
        if is_dir {
            if self.expanded.contains(path) {
                self.expanded.remove(path);
            } else {
                self.expanded.insert(path.to_path_buf());
            }
            self.rebuild();
        } else {
            host.open_path(path);
        }
        self.selected = self
            .visible
            .iter()
            .position(|r| r.path == path)
            .unwrap_or(self.selected);
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let n = self.visible.len() as isize;
        self.selected = (self.selected as isize + delta).clamp(0, n - 1) as usize;
    }

    /// Expand ancestors of `file` and select its row.
    fn reveal(&mut self, file: &Path) {
        let mut cur = file.parent();
        let mut ancestors = Vec::new();
        while let Some(dir) = cur {
            if dir.starts_with(&self.root) && dir != self.root {
                ancestors.push(dir.to_path_buf());
            }
            if dir == self.root {
                break;
            }
            cur = dir.parent();
        }
        for a in ancestors {
            self.expanded.insert(a);
        }
        self.rebuild();
        if let Some(pos) = self.visible.iter().position(|r| r.path == file) {
            self.selected = pos;
        }
    }

    /// Open or toggle the row under the cursor.
    pub(super) fn activate_selected(&mut self, host: &mut dyn Host) {
        if let Some(row) = self.visible.get(self.selected) {
            let path = row.path.clone();
            self.toggle_or_open(&path, host);
        }
    }

    /// Toggle the selected directory, but only when its current expanded state equals
    /// `if_expanded` (so `expand` is a no-op on an open dir, and `collapse` on a shut one).
    pub(super) fn toggle_selected_dir(&mut self, host: &mut dyn Host, if_expanded: bool) {
        if let Some(row) = self.visible.get(self.selected) {
            if row.is_dir && row.expanded == if_expanded {
                let path = row.path.clone();
                self.toggle_or_open(&path, host);
            }
        }
    }

    /// Reveal the active document in the tree, if it has a path.
    pub(super) fn reveal_active_file(&mut self, host: &mut dyn Host) {
        if let Some(id) = host.active_doc() {
            if let Some(path) = host
                .workspace()
                .documents
                .get(id)
                .and_then(|d| d.path.clone())
            {
                self.reveal(&path);
            }
        }
    }
}
