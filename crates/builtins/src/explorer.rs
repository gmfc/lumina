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
use editor_plugin::{
    Contributions, Host, Key, KeyCode, PanelLine, Plugin, Prompt, PromptField, PromptPlacement,
    Span,
};
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

/// The marker prefix for a directory row: an expand chevron plus, in Nerd Font `icons` mode, a
/// **single** folder glyph (open when `expanded`). Non-icon mode is the chevron alone. It carries
/// exactly one folder glyph — earlier it showed two side by side, which read as a duplicated icon.
fn dir_marker(expanded: bool, icons: bool) -> &'static str {
    match (icons, expanded) {
        (true, true) => "▾ \u{f07c} ",  // caret-down + open-folder
        (true, false) => "▸ \u{f07b} ", // caret-right + folder
        (false, true) => "▾ ",
        (false, false) => "▸ ",
    }
}

/// A file operation waiting on the user to name it (or confirm it).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingOp {
    /// Create a file inside `dir`.
    NewFile { dir: PathBuf },
    /// Create a directory inside `dir`.
    NewFolder { dir: PathBuf },
    /// Rename `path` (the field starts at its current name).
    Rename { path: PathBuf },
    /// Delete `path` — the field is a confirmation, not a name.
    Delete { path: PathBuf },
}

pub(crate) struct ExplorerPlugin {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    visible: Vec<Row>,
    selected: usize,
    /// Draw Nerd Font glyphs instead of ASCII `▸ ▾` markers (user config).
    icons: bool,
    /// The file operation the prompt is collecting input for, if any.
    pending: Option<(PendingOp, String)>,
}

impl Default for ExplorerPlugin {
    fn default() -> Self {
        ExplorerPlugin::new(false)
    }
}

impl Plugin for ExplorerPlugin {
    fn id(&self) -> &str {
        Self::ID
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
            .command("explorer.newFile", "Explorer: New File")
            .command("explorer.newFolder", "Explorer: New Folder")
            .command("explorer.rename", "Explorer: Rename")
            .command("explorer.delete", "Explorer: Delete")
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
            "explorer.newFile" => self.begin_new(host, false),
            "explorer.newFolder" => self.begin_new(host, true),
            "explorer.rename" => self.begin_rename(host),
            "explorer.delete" => self.begin_delete(host),
            _ => return false,
        }
        self.render(host);
        true
    }

    fn on_prompt_key(&mut self, prompt_id: &str, key: Key, host: &mut dyn Host) -> bool {
        if prompt_id != Self::PROMPT || self.pending.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.pending = None;
                host.dismiss_prompt();
            }
            KeyCode::Enter => self.commit_pending(host),
            KeyCode::Backspace => {
                if let Some((_, value)) = self.pending.as_mut() {
                    value.pop();
                }
                self.publish_prompt(host, None);
            }
            KeyCode::Char(c) if !key.ctrl && !key.alt => {
                if let Some((_, value)) = self.pending.as_mut() {
                    value.push(c);
                }
                self.publish_prompt(host, None);
            }
            _ => {}
        }
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
        // Rebuild whenever the tree may have moved under us: a reload of an open document, or a
        // path appearing/vanishing anywhere under the root. The latter is what makes a file
        // created or deleted outside the editor actually show up — the watcher used to emit
        // `DidChangeConfig` for it, which nothing consumed.
        if matches!(event, Event::ExternalReload(_) | Event::FilesChanged) {
            self.rebuild();
            self.render(host);
        }
    }
}

impl ExplorerPlugin {
    const ID: &'static str = "explorer";
    const PROMPT: &'static str = "explorer.fileop";

    /// Build an explorer, optionally rendering Nerd Font glyphs.
    pub(crate) fn new(icons: bool) -> Self {
        ExplorerPlugin {
            root: PathBuf::new(),
            expanded: BTreeSet::new(),
            visible: Vec::new(),
            selected: 0,
            icons,
            pending: None,
        }
    }

    // --- file operations --------------------------------------------------
    //
    // Each one collects a name (or a confirmation) through the generic `Prompt` port, then acts
    // through the `Host` filesystem ports. The plugin never touches `std::fs` itself: it owns the
    // tree model and the interaction, the app owns the IO policy (invariant #3).

    /// The directory a new entry should go into: the selected folder if one is selected, else
    /// the folder containing the selected file, else the root.
    fn target_dir(&self) -> PathBuf {
        match self.visible.get(self.selected) {
            Some(row) if row.is_dir => row.path.clone(),
            Some(row) => row
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    fn begin_new(&mut self, host: &mut dyn Host, folder: bool) {
        let dir = self.target_dir();
        let op = if folder {
            PendingOp::NewFolder { dir }
        } else {
            PendingOp::NewFile { dir }
        };
        self.pending = Some((op, String::new()));
        self.publish_prompt(host, None);
    }

    fn begin_rename(&mut self, host: &mut dyn Host) {
        let Some(row) = self.visible.get(self.selected) else {
            return;
        };
        let path = row.path.clone();
        let current = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.pending = Some((PendingOp::Rename { path }, current));
        self.publish_prompt(host, None);
    }

    fn begin_delete(&mut self, host: &mut dyn Host) {
        let Some(row) = self.visible.get(self.selected) else {
            return;
        };
        self.pending = Some((
            PendingOp::Delete {
                path: row.path.clone(),
            },
            String::new(),
        ));
        self.publish_prompt(host, None);
    }

    /// (Re)publish the prompt for the pending operation. `error` is shown emphasized so a failed
    /// attempt says why *in the box* rather than replacing it with a status message the user has
    /// to re-open the dialog to act on.
    fn publish_prompt(&self, host: &mut dyn Host, error: Option<String>) {
        let Some((op, value)) = &self.pending else {
            return;
        };
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned())
        };
        let (title, label, footer) = match op {
            PendingOp::NewFile { dir } => (
                format!("New file in {}", name(dir)),
                "Name",
                "[Enter] Create   [Esc] Cancel",
            ),
            PendingOp::NewFolder { dir } => (
                format!("New folder in {}", name(dir)),
                "Name",
                "[Enter] Create   [Esc] Cancel",
            ),
            PendingOp::Rename { path } => (
                format!("Rename {}", name(path)),
                "Name",
                "[Enter] Rename   [Esc] Cancel",
            ),
            PendingOp::Delete { path } => (
                format!("Delete {}?", name(path)),
                "Type the name to confirm",
                "[Enter] Delete   [Esc] Cancel",
            ),
        };
        let mut prompt = Prompt::new(Self::ID, Self::PROMPT, PromptPlacement::Center);
        prompt.title = Some(title);
        prompt.fields = vec![PromptField::new(label, value.clone())];
        prompt.footer = Some(footer.to_string());
        prompt.error = error;
        host.set_prompt(prompt);
    }

    /// Apply the pending operation. Leaves the prompt up with an error when it fails, so the
    /// user can fix the name rather than retype it from scratch.
    fn commit_pending(&mut self, host: &mut dyn Host) {
        let Some((op, value)) = self.pending.clone() else {
            return;
        };
        let trimmed = value.trim().to_string();
        let result = match &op {
            PendingOp::NewFile { dir } | PendingOp::NewFolder { dir } => {
                if trimmed.is_empty() {
                    Err("Give it a name".to_string())
                } else if trimmed.contains(['/', '\\']) {
                    // A path separator here would silently create somewhere else entirely.
                    Err("Names cannot contain a path separator".to_string())
                } else {
                    let target = dir.join(&trimmed);
                    match op {
                        PendingOp::NewFolder { .. } => host.create_dir(&target),
                        _ => host
                            .create_file(&target)
                            .inspect(|_| host.open_path(&target)),
                    }
                }
            }
            PendingOp::Rename { path } => {
                if trimmed.is_empty() {
                    Err("Give it a name".to_string())
                } else if trimmed.contains(['/', '\\']) {
                    Err("Names cannot contain a path separator".to_string())
                } else {
                    let to = path
                        .parent()
                        .map(|p| p.join(&trimmed))
                        .unwrap_or_else(|| PathBuf::from(&trimmed));
                    host.rename_path(path, &to)
                }
            }
            PendingOp::Delete { path } => {
                // Deleting is the one operation with no undo, so it asks for the name back
                // rather than a bare Enter — the same bar the editor sets elsewhere for
                // discarding work.
                let expected = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if trimmed != expected {
                    Err(format!("Type \"{expected}\" to confirm"))
                } else {
                    host.delete_path(path)
                }
            }
        };
        match result {
            Ok(()) => {
                // Expand the destination, so a file created inside a collapsed folder appears
                // instead of silently landing somewhere the user cannot see.
                if let PendingOp::NewFile { dir } | PendingOp::NewFolder { dir } = &op {
                    self.expanded.insert(dir.clone());
                }
                self.pending = None;
                host.dismiss_prompt();
                self.rebuild();
                self.render(host);
            }
            Err(msg) => self.publish_prompt(host, Some(msg)),
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
                    (dir_marker(row.expanded, self.icons).to_string(), "dir")
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

#[cfg(test)]
mod tests {
    use super::dir_marker;

    /// Regression: a directory shows exactly one folder glyph (it used to render two side by
    /// side), always led by an expand chevron.
    #[test]
    fn directory_marker_has_a_single_folder_icon() {
        let is_folder = |c: char| matches!(c, '\u{f07b}' | '\u{f07c}' | '\u{e5fe}' | '\u{e5ff}');
        for expanded in [true, false] {
            let m = dir_marker(expanded, true);
            let folders = m.chars().filter(|&c| is_folder(c)).count();
            assert_eq!(
                folders, 1,
                "expanded={expanded}: expected one folder glyph in {m:?}"
            );
            assert!(
                m.starts_with('▾') || m.starts_with('▸'),
                "icon marker should start with an expand chevron: {m:?}"
            );
        }
        // Open vs closed folder is distinguished by the glyph.
        assert!(dir_marker(true, true).contains('\u{f07c}')); // open
        assert!(dir_marker(false, true).contains('\u{f07b}')); // closed
                                                               // Non-icon mode is the chevron alone.
        assert_eq!(dir_marker(true, false), "▾ ");
        assert_eq!(dir_marker(false, false), "▸ ");
    }
}
