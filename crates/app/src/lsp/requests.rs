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
