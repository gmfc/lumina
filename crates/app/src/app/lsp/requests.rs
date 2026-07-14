//! Issuing LSP requests: plugin-queued client requests and answering server→client requests.

use super::*;

impl App {
    /// Forward a plugin-queued LSP request to the manager, resolving the primary cursor position
    /// app-side (the `lsp` plugin only expresses intent via `Host::lsp_request`). Symbols need no
    /// cursor; the rest share the `lsp_position` lookup.
    pub(in crate::app) fn dispatch_lsp_request(&mut self, kind: editor_plugin::LspRequestKind) {
        use editor_plugin::LspRequestKind as K;
        // Whole-file / workspace requests need no cursor position. Match by ref so the
        // owned-String `WorkspaceSymbols` variant isn't partially moved out of `kind`.
        match &kind {
            K::DocumentSymbols => {
                self.request_document_symbols();
                return;
            }
            K::Formatting => {
                self.request_formatting();
                return;
            }
            K::WorkspaceSymbols(query) => {
                // Query the active file's language server (one server per language).
                if let Some(lang) = self
                    .editor
                    .active_document()
                    .and_then(|d| d.language.clone())
                {
                    self.lsp.request_workspace_symbols(&lang, query);
                }
                return;
            }
            K::ResolveCompletion { label, data } => {
                if let Some((p, l)) = self.editor.active_document().and_then(|d| {
                    let p = d.path.clone()?;
                    let l = d.language.clone()?;
                    Some((p, l))
                }) {
                    self.lsp.request_resolve_completion(&p, &l, label, data);
                }
                return;
            }
            K::ExecuteCommand { command, arguments } => {
                // Client-command shim (§8.4): emulate a few VS Code built-ins client-side; send
                // everything else to the server if it declared the command.
                use editor_plugin::LspRequestKind as Kr;
                match command.as_str() {
                    "editor.action.triggerSuggest" => {
                        self.editor.pending_lsp_requests.push(Kr::Completion)
                    }
                    "editor.action.triggerParameterHints" => {
                        self.editor.pending_lsp_requests.push(Kr::SignatureHelp)
                    }
                    _ => {
                        if let Some(lang) = self
                            .editor
                            .active_document()
                            .and_then(|d| d.language.clone())
                        {
                            self.lsp.request_execute_command(&lang, command, arguments);
                        }
                    }
                }
                return;
            }
            _ => {}
        }
        let Some((p, l, line, ch)) = self.lsp_position() else {
            return;
        };
        match kind {
            K::Hover => self.lsp.request_hover(&p, &l, line, ch),
            K::Definition => self.lsp.request_definition(&p, &l, line, ch),
            K::Implementation => self.lsp.request_implementation(&p, &l, line, ch),
            K::TypeDefinition => self.lsp.request_type_definition(&p, &l, line, ch),
            K::Completion => self.lsp.request_completion(&p, &l, line, ch),
            K::SignatureHelp => self.lsp.request_signature_help(&p, &l, line, ch),
            K::DocumentHighlight => self.lsp.request_document_highlight(&p, &l, line, ch),
            K::CodeAction => self.lsp.request_code_action(&p, &l, line, ch),
            K::References => self.lsp.request_references(&p, &l, line, ch),
            K::Rename(name) => self.lsp.request_rename(&p, &l, line, ch, &name),
            // handled above (whole-document / no-cursor requests)
            K::DocumentSymbols
            | K::Formatting
            | K::WorkspaceSymbols(_)
            | K::ResolveCompletion { .. }
            | K::ExecuteCommand { .. } => false,
        };
    }

    /// Act on a server→client request that needs the editor (docs/UI) and answer it. Every arm
    /// MUST reply through `LspManager::respond` (§1.3): an unanswered request hangs the server.
    pub(in crate::app) fn handle_server_request(
        &mut self,
        lang: String,
        id: serde_json::Value,
        method: &str,
        params: serde_json::Value,
    ) {
        match method {
            "workspace/applyEdit" => {
                // A server-initiated edit (code actions / executeCommand results). Apply through
                // the same Transaction pipeline as rename (invariant #1) and report applied.
                let edit = params
                    .get("edit")
                    .map(editor_lsp::client::parse_workspace_edit)
                    .unwrap_or_default();
                let primitive = self.resolve_workspace_edit(edit);
                // Report what actually landed, not just what was requested (a stale/missing
                // target file changes nothing).
                let applied = self.apply_workspace_edit(primitive) > 0;
                self.lsp
                    .respond(&lang, &id, serde_json::json!({ "applied": applied }));
            }
            "window/showMessageRequest" => {
                // Surface the message; PR2 chooses no action (later: action buttons).
                if let Some(msg) = params.get("message").and_then(|m| m.as_str()) {
                    self.editor.status_message = Some(format!("LSP: {msg}"));
                }
                self.lsp.respond(&lang, &id, serde_json::Value::Null);
            }
            "window/showDocument" => {
                // Open a `file:` document in the editor unless the server asked for an external
                // (OS/browser) open, which we don't do here.
                let external = params
                    .get("external")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false);
                let path = params
                    .get("uri")
                    .and_then(|u| u.as_str())
                    .and_then(crate::lsp::path_from_uri);
                // Only claim success for a real openable file we actually show — not a directory
                // or a missing/external target.
                let opened = match (external, path) {
                    (false, Some(path)) if path.is_file() => {
                        self.open_path(&path);
                        true
                    }
                    _ => false,
                };
                self.lsp
                    .respond(&lang, &id, serde_json::json!({ "success": opened }));
            }
            // The manager only routes the three methods above; anything else still gets a reply
            // so a mis-route can never hang the server.
            _ => self.lsp.respond(&lang, &id, serde_json::Value::Null),
        }
    }

    /// Request the symbols in the active document (no cursor position needed).
    pub(in crate::app) fn request_document_symbols(&mut self) {
        let info = self.editor.active_document().and_then(|d| {
            let path = d.path.clone()?;
            let lang = d.language.clone()?;
            Some((path, lang))
        });
        if let Some((path, lang)) = info {
            self.lsp.request_document_symbols(&path, &lang);
        }
    }

    /// Request whole-document formatting for the active document, using the editor's indent
    /// settings as `FormattingOptions` (Lumina indents with spaces).
    pub(in crate::app) fn request_formatting(&mut self) {
        let info = self.editor.active_document().and_then(|d| {
            let path = d.path.clone()?;
            let lang = d.language.clone()?;
            Some((path, lang))
        });
        if let Some((path, lang)) = info {
            let tab_size = self.config.tab_width as u32;
            self.lsp.request_formatting(&path, &lang, tab_size, true);
        }
    }
}
