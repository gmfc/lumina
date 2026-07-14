//! The command palette + quick-open + goto-line, implemented **as a plugin** (invariant #3).
//!
//! The palette and quick-open open the app's generic fuzzy picker (the `>` switch flips
//! files ⇄ commands); goto-line opens the generic centered [`Prompt`]. The plugin sources rows
//! through [`Host`] only — [`Host::commands`] enumerates every command across the split-borrow
//! wall, [`Host::project_files`] walks the workspace (the app owns the `ignore` policy) — and on
//! activation runs the chosen command, opens the chosen file, or jumps to the entered line. It
//! owns only the goto-line digit buffer; the app drives the overlays.

use editor_core::{Selection, Selections};
use editor_plugin::{
    Contributions, Host, Key, KeyCode, PickerItem, PickerRequest, Plugin, Prompt, PromptField,
    PromptPlacement,
};

#[derive(Default)]
pub(crate) struct PalettePlugin {
    /// Digits typed into the goto-line prompt while it is open.
    goto_line: String,
}

impl PalettePlugin {
    const ID: &'static str = "palette";
    const TOKEN: &'static str = "open";
    const GOTO: &'static str = "gotoLine";

    /// Render the goto-line digit buffer into the generic centered prompt.
    fn goto_prompt(&self) -> Prompt {
        let mut p = Prompt::new(Self::ID, Self::GOTO, PromptPlacement::Center);
        p.title = Some("Go to Line".to_string());
        p.fields = vec![PromptField::new("Line", self.goto_line.clone())];
        p.footer = Some("[Enter] Go   [Esc] Cancel".to_string());
        p
    }
}

impl Plugin for PalettePlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("view.commandPalette", "Go: Show All Commands")
            .command("view.quickOpen", "Go to File…")
            .command("view.gotoLine", "Go to Line…")
            .keybinding("ctrl+shift+p", "view.commandPalette")
            .keybinding("ctrl+p", "view.quickOpen")
            .keybinding("ctrl+g", "view.gotoLine")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if command_id == "view.gotoLine" {
            self.goto_line.clear();
            host.set_prompt(self.goto_prompt());
            return true;
        }
        // The palette + quick-open build the unified files+commands picker so the `>` switch works
        // either way; only the entry mode differs.
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

    fn on_prompt_key(&mut self, prompt_id: &str, key: Key, host: &mut dyn Host) -> bool {
        if prompt_id != Self::GOTO {
            return false;
        }
        match key.code {
            KeyCode::Esc => host.dismiss_prompt(),
            KeyCode::Enter => {
                let target = self.goto_line.trim().parse::<usize>().ok();
                host.dismiss_prompt();
                if let (Some(line), Some(id)) = (target, host.active_doc()) {
                    // Clamp to the document; a caret at the line start (1-based input).
                    let off = host.workspace().documents.get(id).map(|doc| {
                        let l = line
                            .saturating_sub(1)
                            .min(doc.len_lines().saturating_sub(1));
                        doc.line_to_char(l)
                    });
                    if let Some(off) = off {
                        host.set_selections(id, Selections::single(Selection::caret(off)));
                    }
                }
            }
            KeyCode::Backspace => {
                self.goto_line.pop();
                host.set_prompt(self.goto_prompt());
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.goto_line.push(c);
                host.set_prompt(self.goto_prompt());
            }
            _ => {}
        }
        true
    }
}
