//! Position-based LSP requests. Each builds a JSON-RPC request for the active document and
//! records its [`Pending`] kind so the response can be interpreted in [`LspManager::poll`].

use super::*;
use editor_lsp::Cap;

impl LspManager {
    /// Send a request for `uri`, recording its kind + the buffer version it was asked against
    /// (for staleness detection). Starts the connection if needed and gates on the advertised
    /// capability — an unsupported request degrades silently (no `-32601` noise) and returns
    /// `false`. Supersedes any prior in-flight request of the same cancelable kind (§1.4).
    /// `line`/`character` are LSP coordinates (character is a UTF-16 column).
    fn send_request<F>(
        &mut self,
        language: &str,
        uri: &str,
        kind: Pending,
        cap: Cap,
        build: F,
    ) -> bool
    where
        F: FnOnce(&LspHandle) -> std::io::Result<i64>,
    {
        self.ensure_started(language);
        if !self.request_allowed(language, cap) {
            return false;
        }
        let version = self.versions.get(uri).copied().unwrap_or(0);
        // Cancel a prior in-flight request of this cancelable kind; its pending entry stays until
        // the (cancelled) response arrives, then drops as superseded.
        if is_cancelable(kind) {
            if let Some(&old) = self.inflight.get(&(language.to_string(), kind)) {
                if let Some(client) = self.clients.get(language) {
                    client.cancel(old);
                }
            }
        }
        let Some(client) = self.clients.get(language) else {
            return false;
        };
        match build(client) {
            Ok(id) => {
                self.pending.insert(
                    (language.to_string(), id),
                    PendingEntry {
                        kind,
                        uri: uri.to_string(),
                        version,
                    },
                );
                if is_cancelable(kind) {
                    self.inflight.insert((language.to_string(), kind), id);
                }
                true
            }
            Err(_) => false,
        }
    }

    pub fn request_hover(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, &uri, Pending::Hover, Cap::Hover, |c| {
            c.hover(&uri, line, character)
        })
    }

    pub fn request_definition(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, &uri, Pending::Definition, Cap::Definition, |c| {
            c.definition(&uri, line, character)
        })
    }

    pub fn request_implementation(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        // Reuses the Definition correlation: the response is location(s) we jump to.
        self.send_request(
            language,
            &uri,
            Pending::Definition,
            Cap::Implementation,
            |c| c.implementation(&uri, line, character),
        )
    }

    pub fn request_type_definition(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(
            language,
            &uri,
            Pending::Definition,
            Cap::TypeDefinition,
            |c| c.type_definition(&uri, line, character),
        )
    }

    pub fn request_completion(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, &uri, Pending::Completion, Cap::Completion, |c| {
            c.completion(&uri, line, character)
        })
    }

    pub fn request_resolve_completion(
        &mut self,
        path: &Path,
        language: &str,
        label: &str,
        data: &serde_json::Value,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(
            language,
            &uri,
            Pending::ResolveCompletion,
            Cap::Completion,
            |c| c.resolve_completion(label, data),
        )
    }

    pub fn request_signature_help(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(
            language,
            &uri,
            Pending::SignatureHelp,
            Cap::SignatureHelp,
            |c| c.signature_help(&uri, line, character),
        )
    }

    pub fn request_rename(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, &uri, Pending::Rename, Cap::Rename, |c| {
            c.rename(&uri, line, character, new_name)
        })
    }

    pub fn request_references(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, &uri, Pending::References, Cap::References, |c| {
            c.references(&uri, line, character)
        })
    }

    pub fn request_document_highlight(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(
            language,
            &uri,
            Pending::DocumentHighlight,
            Cap::DocumentHighlight,
            |c| c.document_highlight(&uri, line, character),
        )
    }

    /// Run a server-declared command (fire-and-forget: the result is ignored; effects arrive as
    /// `workspace/applyEdit`). Undeclared commands are dropped rather than eliciting `-32601`.
    pub fn request_execute_command(
        &mut self,
        language: &str,
        command: &str,
        arguments: &serde_json::Value,
    ) -> bool {
        self.ensure_started(language);
        if !self.can_execute(language, command) {
            return false;
        }
        if let Some(client) = self.clients.get(language) {
            return client.execute_command(command, arguments).is_ok();
        }
        false
    }

    pub fn request_code_action(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        // Cursor as an empty range (selection-range support is a later refinement). Echo the
        // diagnostics overlapping that point into the request context so the server can offer
        // their quickfixes (§6.1).
        let ctx = self.context_diagnostics(&uri, (line, character), (line, character));
        self.send_request(language, &uri, Pending::CodeAction, Cap::CodeAction, |c| {
            c.code_action(&uri, line, character, line, character, &ctx)
        })
    }

    /// Pull diagnostics for a document (§5.1), echoing the cached `previousResultId` (and the
    /// server's `diagnosticProvider.identifier`) so an unchanged report is cheap. Gated on
    /// `diagnosticProvider`; version-tracked like any request so a stale report is dropped.
    pub fn request_pull_diagnostics(&mut self, path: &Path, language: &str) -> bool {
        let uri = uri_for(path);
        let prev = self.diag_result_id.get(&uri).cloned();
        let identifier = match self.state.get(language) {
            Some(ClientState::Running(caps)) => caps.diagnostic_identifier.clone(),
            _ => None,
        };
        self.send_request(
            language,
            &uri,
            Pending::Diagnostic,
            Cap::PullDiagnostics,
            |c| c.diagnostic(&uri, identifier.as_deref(), prev.as_deref()),
        )
    }

    /// Request full-document semantic tokens (§7.1). Whole-file (no cursor); version-tracked so a
    /// response that arrives after an edit is dropped, and cancelable so a typing burst supersedes.
    pub fn request_semantic_tokens(&mut self, path: &Path, language: &str) -> bool {
        let uri = uri_for(path);
        self.send_request(
            language,
            &uri,
            Pending::SemanticTokens,
            Cap::SemanticTokens,
            |c| c.semantic_tokens_full(&uri),
        )
    }

    /// Request inlay hints for the whole document (§7.2) — `end_line` is the doc's line count, so
    /// the range covers everything (viewport-only fetching is a later refinement). Version-tracked
    /// + cancelable like semantic tokens.
    pub fn request_inlay_hints(&mut self, path: &Path, language: &str, end_line: u32) -> bool {
        let uri = uri_for(path);
        self.send_request(language, &uri, Pending::InlayHint, Cap::InlayHint, |c| {
            c.inlay_hint(&uri, 0, 0, end_line, 0)
        })
    }

    pub fn request_workspace_symbols(&mut self, language: &str, query: &str) -> bool {
        // Workspace symbols aren't tied to a document; tag with an empty uri (version 0).
        self.send_request(
            language,
            "",
            Pending::WorkspaceSymbols,
            Cap::WorkspaceSymbol,
            |c| c.workspace_symbols(query),
        )
    }

    pub fn request_document_symbols(&mut self, path: &Path, language: &str) -> bool {
        let uri = uri_for(path);
        self.send_request(
            language,
            &uri,
            Pending::DocumentSymbols,
            Cap::DocumentSymbol,
            |c| c.document_symbols(&uri),
        )
    }

    pub fn request_formatting(
        &mut self,
        path: &Path,
        language: &str,
        tab_size: u32,
        insert_spaces: bool,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, &uri, Pending::Formatting, Cap::Formatting, |c| {
            c.formatting(&uri, tab_size, insert_spaces)
        })
    }
}
