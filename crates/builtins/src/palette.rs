//! The command palette + quick-open, implemented **as a plugin** (invariant #3).
//!
//! Both open the app's generic fuzzy picker (the `>` switch flips files ⇄ commands). The plugin
//! sources the rows through [`Host`] only — [`Host::commands`] enumerates every command across
//! the split-borrow wall, [`Host::project_files`] walks the workspace (the app owns the `ignore`
//! policy) — publishes them via [`Host::open_picker`], and on activation runs the chosen command
//! or opens the chosen file. It owns no picker state; the app drives the overlay.

use editor_plugin::{Contributions, Host, PickerItem, PickerRequest, Plugin};

pub struct PalettePlugin;

impl PalettePlugin {
    const ID: &'static str = "palette";
    const TOKEN: &'static str = "open";
}

impl Plugin for PalettePlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("view.commandPalette", "Go: Show All Commands")
            .command("view.quickOpen", "Go to File…")
            .keybinding("ctrl+shift+p", "view.commandPalette")
            .keybinding("ctrl+p", "view.quickOpen")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        // Both commands build the unified files+commands picker so the `>` switch works either
        // way; only the entry mode differs.
        let start_in_commands = match command_id {
            "view.commandPalette" => true,
            "view.quickOpen" => false,
            _ => return false,
        };
        let root = host.root().to_path_buf();
        let items = host
            .project_files()
            .into_iter()
            .map(|e| {
                let label = e
                    .path
                    .strip_prefix(&root)
                    .unwrap_or(&e.path)
                    .to_string_lossy()
                    .into_owned();
                PickerItem::new(e.path.to_string_lossy().into_owned(), label)
            })
            .collect();
        let commands = host
            .commands()
            .into_iter()
            .map(|c| PickerItem::new(c.id, c.title))
            .collect();
        host.open_picker(PickerRequest {
            owner: Self::ID.to_string(),
            token: Self::TOKEN.to_string(),
            title: "Go to File".to_string(),
            items,
            commands,
            start_in_commands,
        });
        true
    }

    fn on_picker_activate(&mut self, _token: &str, item_id: &str, host: &mut dyn Host) {
        // A command row's id is in the enumerated command set; anything else is a file path.
        if host.commands().iter().any(|c| c.id == item_id) {
            host.execute(item_id);
        } else {
            host.open_path(std::path::Path::new(item_id));
        }
    }
}
