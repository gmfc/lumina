//! Language-server request commands, implemented **as a plugin** (invariant #3).
//!
//! This owns the *request-issuing* half of the LSP feature: hover, go-to-definition /
//! implementation / type-definition, trigger-suggest (completion), find-references,
//! document-symbols, and rename. Each reaches the editor only through [`Host::lsp_request`] — the
//! app owns the transport, the UTF-16 cursor math, and all response handling (hover popup,
//! goto/rename application, completion widget, references/symbols picker). Diagnostic navigation
//! stays app-side (it reads the app-owned diagnostics map).

use editor_core::motion;
use editor_plugin::{
    Contributions, Host, Key, KeyCode, LspRequestKind, Plugin, Prompt, PromptField, PromptPlacement,
};

#[derive(Default)]
pub struct LspPlugin {
    /// The identifier typed into the rename prompt while it's open.
    rename_buf: String,
}

impl LspPlugin {
    const ID: &'static str = "lsp";
    const RENAME_PROMPT: &'static str = "lsp.rename";

    /// Open the rename prompt, seeded with the identifier under the caret.
    fn open_rename(&mut self, host: &mut dyn Host) {
        self.rename_buf = host
            .active_doc()
            .and_then(|id| host.workspace().documents.get(id))
            .map(|doc| {
                let head = doc.selections.primary().head;
                let (s, e) = motion::word_at(doc, head);
                doc.rope().slice(s..e).to_string()
            })
            .unwrap_or_default();
        host.set_prompt(self.rename_prompt());
    }

    fn rename_prompt(&self) -> Prompt {
        let mut p = Prompt::new(Self::ID, Self::RENAME_PROMPT, PromptPlacement::Center);
        p.title = Some("Rename Symbol".to_string());
        p.fields = vec![PromptField::new("Name", self.rename_buf.clone())];
        p.footer = Some("[Enter] Apply   [Esc] Cancel".to_string());
        p
    }
}

impl Plugin for LspPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("lsp.hover", "Go: Show Hover")
            .command("lsp.gotoDefinition", "Go: Go to Definition")
            .command("lsp.gotoImplementation", "Go: Go to Implementation")
            .command("lsp.gotoTypeDefinition", "Go: Go to Type Definition")
            .command("lsp.references", "Go: Find All References")
            .command("lsp.documentSymbols", "Go: Symbols in File…")
            .command("lsp.rename", "Refactor: Rename Symbol")
            .command("lsp.format", "Edit: Format Document")
            .keybinding("ctrl+k ctrl+i", "lsp.hover")
            .keybinding("f12", "lsp.gotoDefinition")
            .keybinding("ctrl+f12", "lsp.gotoImplementation")
            .keybinding("shift+f12", "lsp.references")
            .keybinding("ctrl+shift+o", "lsp.documentSymbols")
            .keybinding("f2", "lsp.rename")
            .keybinding("shift+alt+f", "lsp.format")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        // Nothing to talk to → cleanly no-op (but still claim our ids).
        if !host.lsp_enabled() {
            return matches!(
                command_id,
                "lsp.hover"
                    | "lsp.gotoDefinition"
                    | "lsp.gotoImplementation"
                    | "lsp.gotoTypeDefinition"
                    | "lsp.references"
                    | "lsp.documentSymbols"
                    | "lsp.rename"
                    | "lsp.format"
            );
        }
        let kind = match command_id {
            "lsp.hover" => LspRequestKind::Hover,
            "lsp.gotoDefinition" => LspRequestKind::Definition,
            "lsp.gotoImplementation" => LspRequestKind::Implementation,
            "lsp.gotoTypeDefinition" => LspRequestKind::TypeDefinition,
            "lsp.references" => LspRequestKind::References,
            "lsp.documentSymbols" => LspRequestKind::DocumentSymbols,
            "lsp.format" => LspRequestKind::Formatting,
            "lsp.rename" => {
                self.open_rename(host);
                return true;
            }
            _ => return false,
        };
        host.lsp_request(kind);
        true
    }

    fn on_prompt_key(&mut self, prompt_id: &str, key: Key, host: &mut dyn Host) -> bool {
        if prompt_id != Self::RENAME_PROMPT {
            return false;
        }
        match key.code {
            KeyCode::Esc => host.dismiss_prompt(),
            KeyCode::Enter => {
                host.dismiss_prompt();
                if !self.rename_buf.is_empty() {
                    host.lsp_request(LspRequestKind::Rename(self.rename_buf.clone()));
                }
            }
            KeyCode::Backspace => {
                self.rename_buf.pop();
                host.set_prompt(self.rename_prompt());
            }
            KeyCode::Char(c) if !key.ctrl && !key.alt => {
                self.rename_buf.push(c);
                host.set_prompt(self.rename_prompt());
            }
            _ => {}
        }
        true
    }
}
