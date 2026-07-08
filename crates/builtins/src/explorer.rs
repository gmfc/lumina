//! The file explorer — a **plugin**, not a hardcoded feature (CLAUDE.md invariant #3).
//!
//! It contributes a sidebar panel (`explorer.tree`) and commands (`explorer.*`), models the
//! directory tree lazily (children read only when a folder expands), and reaches the editor
//! only through [`Host`]. The `self_hosting` test proves disabling it removes exactly these
//! contributions.

use std::collections::BTreeSet;
use std::path::PathBuf;

use editor_plugin::contribution::PanelLocation;
use editor_plugin::event::Event;
use editor_plugin::{Contributions, Host, Plugin};

mod logic;
mod model;

use model::Row;

const PANEL: &str = "explorer.tree";

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
