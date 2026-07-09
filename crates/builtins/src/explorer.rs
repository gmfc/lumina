//! The file explorer — a **plugin**, not a hardcoded feature (CLAUDE.md invariant #3).
//!
//! It contributes a sidebar panel (`explorer.tree`) and commands (`explorer.*`), models the
//! directory tree lazily (children read only when a folder expands), and reaches the editor
//! only through [`Host`]. The `self_hosting` test proves disabling it removes exactly these
//! contributions.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use editor_plugin::contribution::PanelLocation;
use editor_plugin::event::Event;
use editor_plugin::host::PanelContent;
use editor_plugin::{Contributions, Host, PanelLine, Plugin, Span};
use ignore::WalkBuilder;

const PANEL: &str = "explorer.tree";

/// One visible row of the flattened tree (plan §6: keep a flat `Vec` for O(1) hit-testing).
struct Row {
    path: PathBuf,
    is_dir: bool,
    depth: usize,
    expanded: bool,
}

/// A Nerd Font glyph for a file, chosen by extension (requires a patched font; opt-in).
fn file_glyph(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "\u{e7a8}",                   //
        Some("py") => "\u{e606}",                   //
        Some("js" | "mjs" | "cjs") => "\u{e781}",   //
        Some("ts" | "tsx") => "\u{e628}",           //
        Some("json") => "\u{e60b}",                 //
        Some("toml" | "ini" | "cfg") => "\u{e615}", //
        Some("md" | "markdown") => "\u{e73e}",      //
        Some("c" | "h") => "\u{e61e}",              //
        Some("go") => "\u{e627}",                   //
        Some("lock") => "\u{f023}",                 //
        _ => "\u{f15b}",                            //
    }
}

pub struct ExplorerPlugin {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    visible: Vec<Row>,
    selected: usize,
    /// Draw Nerd Font glyphs instead of ASCII `▸ ▾` markers (user config).
    icons: bool,
}

impl Default for ExplorerPlugin {
    fn default() -> Self {
        ExplorerPlugin::new(false)
    }
}

impl Plugin for ExplorerPlugin {
    fn id(&self) -> &str {
        "explorer"
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .panel(PANEL, "Explorer", PanelLocation::Sidebar)
            .command("explorer.revealActiveFile", "Explorer: Reveal Active File")
            .command("explorer.up", "Explorer: Select Previous")
            .command("explorer.down", "Explorer: Select Next")
            .command("explorer.activate", "Explorer: Open / Toggle")
            .command("explorer.expand", "Explorer: Expand Folder")
            .command("explorer.collapse", "Explorer: Collapse Folder")
            .build()
    }

    fn activate(&mut self, host: &mut dyn Host) {
        self.root = host.root().to_path_buf();
        self.rebuild();
        self.render(host);
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "explorer.down" => self.move_selection(1),
            "explorer.up" => self.move_selection(-1),
            "explorer.activate" => self.activate_selected(host),
            "explorer.expand" => self.toggle_selected_dir(host, /* if_expanded */ false),
            "explorer.collapse" => self.toggle_selected_dir(host, /* if_expanded */ true),
            "explorer.revealActiveFile" => self.reveal_active_file(host),
            _ => return false,
        }
        self.render(host);
        true
    }

    fn on_panel_activate(&mut self, panel_id: &str, payload: &str, host: &mut dyn Host) {
        if panel_id != PANEL {
            return;
        }
        let path = PathBuf::from(payload);
        self.toggle_or_open(&path, host);
        self.render(host);
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        // Refresh when the filesystem-backed tree may have changed under us (Phase 8),
        // or when a file opens (so the selection can follow if revealed).
        if let Event::ExternalReload(_) = event {
            self.rebuild();
            self.render(host);
        }
    }
}

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
            // Resolve through symlinks so a symlinked directory is treated as a directory
            // (expandable) rather than a file. `file_type()` reports the link itself, not its
            // target; `path().is_dir()` follows it (and reports false for a broken link).
            let is_dir = match entry.file_type() {
                Some(t) if t.is_symlink() => path.is_dir(),
                Some(t) => t.is_dir(),
                None => false,
            };
            out.push((path, is_dir));
        }
        out.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.file_name().cmp(&b.0.file_name()))
        });
        out
    }

    /// Rebuild the flattened visible-row list from the expanded set.
    fn rebuild(&mut self) {
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

    fn render(&self, host: &mut dyn Host) {
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

    fn toggle_or_open(&mut self, path: &Path, host: &mut dyn Host) {
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

    fn move_selection(&mut self, delta: isize) {
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
    fn activate_selected(&mut self, host: &mut dyn Host) {
        if let Some(row) = self.visible.get(self.selected) {
            let path = row.path.clone();
            self.toggle_or_open(&path, host);
        }
    }

    /// Toggle the selected directory, but only when its current expanded state equals
    /// `if_expanded` (so `expand` is a no-op on an open dir, and `collapse` on a shut one).
    fn toggle_selected_dir(&mut self, host: &mut dyn Host, if_expanded: bool) {
        if let Some(row) = self.visible.get(self.selected) {
            if row.is_dir && row.expanded == if_expanded {
                let path = row.path.clone();
                self.toggle_or_open(&path, host);
            }
        }
    }

    /// Reveal the active document in the tree, if it has a path.
    fn reveal_active_file(&mut self, host: &mut dyn Host) {
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
