//! Document synchronization: forwarding `didOpen`/`didChange`/`didClose` for open buffers (full
//! text sync) and tracking the per-document version the server last saw.

use super::*;

impl LspManager {
    /// Notify the server that a document opened. Sends only once the connection is `Running`;
    /// returns whether the notification was actually sent (so the caller records the sent
    /// revision only on a real send).
    pub fn did_open(&mut self, path: &Path, language: &str, text: &str) -> bool {
        if !self.is_ready(language) {
            return false;
        }
        let uri = uri_for(path);
        self.versions.insert(uri.clone(), 1);
        if let Some(client) = self.clients.get(language) {
            let sent = client.did_open(&uri, language, 1, text).is_ok();
            if sent {
                self.open_docs
                    .entry(language.to_string())
                    .or_default()
                    .insert(uri);
            }
            return sent;
        }
        false
    }

    /// Notify the server that a document changed (full sync). Sends only once `Running`.
    pub fn did_change(&mut self, path: &Path, language: &str, text: &str) -> bool {
        if !self.is_ready(language) {
            return false;
        }
        let uri = uri_for(path);
        let version = self.versions.entry(uri.clone()).or_insert(1);
        *version += 1;
        let v = *version;
        if let Some(client) = self.clients.get(language) {
            return client.did_change(&uri, v, text).is_ok();
        }
        false
    }

    /// Notify the server that a document closed (§4.1): its truth reverts to disk. Sends
    /// `didClose` only for a document this session actually opened (no stray close after a
    /// crash/restart) and drops the doc's per-server bookkeeping.
    pub fn did_close(&mut self, path: &Path, language: &str) {
        let uri = uri_for(path);
        self.versions.remove(&uri);
        self.diag_raw.remove(&uri);
        self.diag_result_id.remove(&uri);
        self.code_lens.remove(&uri);
        if let Some(p) = self.published.get_mut(language) {
            p.remove(&uri);
        }
        let was_open = self
            .open_docs
            .get_mut(language)
            .is_some_and(|open| open.remove(&uri));
        if was_open && self.is_ready(language) {
            if let Some(client) = self.clients.get(language) {
                let _ = client.did_close(&uri);
            }
        }
    }

    /// The last document version synced to the server for `uri`, if any — the baseline a
    /// server-computed `WorkspaceEdit` must match to be safe to apply (§2.4).
    pub fn doc_version(&self, uri: &str) -> Option<i64> {
        self.versions.get(uri).copied()
    }
}
