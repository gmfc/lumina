//! LSP code actions, implemented **as a plugin** (invariant #3).
//!
//! The `lsp.codeAction` command requests actions for the cursor through [`Host::lsp_request`]; the
//! app translates the server's `(Command | CodeAction)[]` into a primitive [`Event::LspCodeActions`]
//! (edit URIs resolved to paths). This plugin lists the actions in the generic picker and, on
//! selection, forwards the chosen action's edit through [`Host::apply_workspace_edit`]. The LSP
//! transport + the edit application (one atomic Transaction group) stay app-side.
//!
//! MVP scope: actions carrying an `edit` (refactor / source / edit-based quickfix). Command-only
//! actions (`workspace/executeCommand`) and diagnostic-context quickfixes are later refinements.

use editor_plugin::{
    Contributions, Event, Host, LspCodeAction, LspRequestKind, MenuGroup, MenuWhen, PickerItem,
    PickerRequest, Plugin,
};

#[derive(Default)]
pub(crate) struct CodeActionPlugin {
    /// The actions backing the current picker; a row's id is the index into this list.
    actions: Vec<LspCodeAction>,
}

impl CodeActionPlugin {
    const ID: &'static str = "code-action";
    const TOKEN: &'static str = "actions";
}

impl Plugin for CodeActionPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("lsp.codeAction", "Refactor: Code Action…")
            .keybinding("ctrl+.", "lsp.codeAction")
            .menu_item(
                "lsp.codeAction",
                "Code Action / Quick Fix…",
                MenuGroup::Refactor,
                MenuWhen::LspEnabled,
            )
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if command_id == "lsp.codeAction" {
            if host.lsp_enabled() {
                host.lsp_request(LspRequestKind::CodeAction);
            }
            return true;
        }
        false
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if let Event::LspCodeActions(actions) = event {
            if actions.is_empty() {
                self.actions.clear();
                host.notify("No code actions available".to_string());
                return;
            }
            self.actions = actions.clone();
            let items = actions
                .iter()
                .enumerate()
                .map(|(i, a)| PickerItem::new(i.to_string(), a.title.clone()))
                .collect();
            host.open_picker(PickerRequest {
                owner: Self::ID.to_string(),
                token: Self::TOKEN.to_string(),
                title: "Code Actions".to_string(),
                items,
                commands: Vec::new(),
                start_in_commands: false,
            });
        }
    }

    fn on_picker_activate(&mut self, token: &str, item_id: &str, host: &mut dyn Host) {
        if token != Self::TOKEN {
            return;
        }
        if let Some(action) = item_id
            .parse::<usize>()
            .ok()
            .and_then(|i| self.actions.get(i))
            .cloned()
        {
            // Execution order: apply the edit, then run the command (§6.3).
            if !action.edit.changes.is_empty() {
                host.apply_workspace_edit(action.edit);
            }
            if let Some((command, arguments)) = action.command {
                host.lsp_request(LspRequestKind::ExecuteCommand { command, arguments });
            }
        }
    }
}
