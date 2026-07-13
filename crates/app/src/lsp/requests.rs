//! Position-based LSP requests. Each builds a JSON-RPC request for the active document and
//! records its [`Pending`] kind so the response can be interpreted in [`LspManager::poll`].

use super::*;
use editor_lsp::Cap;

impl LspManager {
    /// Send a request for the active document, recording its kind for response correlation.
    /// Starts the connection if needed and gates on the advertised capability — an unsupported
    /// request degrades silently (no `-32601` noise) and returns `false`.
    /// `line`/`character` are LSP coordinates (character is a UTF-16 column).
    fn send_request<F>(&mut self, language: &str, kind: Pending, cap: Cap, build: F) -> bool
    where
        F: FnOnce(&LspHandle) -> std::io::Result<i64>,
    {
        self.ensure_started(language);
        if !self.request_allowed(language, cap) {
            return false;
        }
        let Some(client) = self.clients.get(language) else {
            return false;
        };
        match build(client) {
            Ok(id) => {
                self.pending.insert((language.to_string(), id), kind);
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
        self.send_request(language, Pending::Hover, Cap::Hover, |c| {
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
        self.send_request(language, Pending::Definition, Cap::Definition, |c| {
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
        self.send_request(language, Pending::Definition, Cap::Implementation, |c| {
            c.implementation(&uri, line, character)
        })
    }

    pub fn request_type_definition(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, Pending::Definition, Cap::TypeDefinition, |c| {
            c.type_definition(&uri, line, character)
        })
    }

    pub fn request_completion(
        &mut self,
        path: &Path,
        language: &str,
        line: u32,
        character: u32,
    ) -> bool {
        let uri = uri_for(path);
        self.send_request(language, Pending::Completion, Cap::Completion, |c| {
            c.completion(&uri, line, character)
        })
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
        self.send_request(language, Pending::Rename, Cap::Rename, |c| {
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
        self.send_request(language, Pending::References, Cap::References, |c| {
            c.references(&uri, line, character)
        })
    }

    pub fn request_document_symbols(&mut self, path: &Path, language: &str) -> bool {
        let uri = uri_for(path);
        self.send_request(
            language,
            Pending::DocumentSymbols,
            Cap::DocumentSymbol,
            |c| c.document_symbols(&uri),
        )
    }
}
