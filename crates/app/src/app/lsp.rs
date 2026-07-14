//! LSP glue: notifying the server, issuing requests, and handling responses.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;
use std::path::Path;

mod convert;
mod events;
mod requests;

/// Quiet period after the last edit before a diagnostics pull fires (§5.1) — avoids a pull per
/// keystroke while typing.
const PULL_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

impl App {
    /// Notify the LSP of every open document's opens/changes (debounced by revision). The server
    /// mirror must know about **all** open buffers, not just the focused one, so cross-file
    /// features (references, rename, workspace edits) and crash-resync stay correct. Inert unless
    /// a server is configured.
    pub(super) fn update_lsp(&mut self) {
        if !self.lsp.is_enabled() {
            return;
        }
        // Snapshot every open, path-backed, language-detected document (id, path, lang, revision).
        let docs: Vec<(editor_core::DocId, PathBuf, String, u64)> = self
            .editor
            .workspace
            .documents
            .iter()
            .filter_map(|(id, d)| Some((id, d.path.clone()?, d.language.clone()?, d.revision)))
            .collect();
        for (id, path, lang, rev) in docs {
            // Kick each language's connection into starting (non-blocking) and wait for its
            // handshake before sending — didOpen/didChange are illegal before `initialized`.
            self.lsp.ensure_started(&lang);
            if !self.lsp.is_ready(&lang) {
                continue;
            }
            // On a real (re)sync, refresh the passive whole-doc features for this doc.
            if self.sync_document(id, &path, &lang, rev) {
                self.request_passive_features(id, &path, &lang);
            }
            self.poll_pull_diagnostics(id, &path, &lang, rev);
        }
        self.mirror_lsp_health();
    }

    /// Mirror the *active* file's server health onto the status line (the footer LSP indicator),
    /// via the shared `status_items` map since the renderer can't reach the private manager.
    fn mirror_lsp_health(&mut self) {
        let active_lang = self
            .editor
            .workspace
            .active_doc()
            .and_then(|id| self.editor.workspace.documents.get(id))
            .and_then(|d| d.language.clone());
        let tag = self.lsp.health_tag_for(active_lang.as_deref());
        self.editor
            .status_items
            .insert("lsp.health".into(), tag.to_string());
    }

    /// Send `didOpen`/`didChange` for a doc when its revision advanced (the rope is serialized only
    /// on a real change). Returns whether a notification was actually sent, recording the sent
    /// revision so the next tick is a cheap no-op.
    fn sync_document(&mut self, id: editor_core::DocId, path: &Path, lang: &str, rev: u64) -> bool {
        let sent = self.lsp_sent_revision.get(&id).copied();
        if sent == Some(rev) {
            return false; // unchanged since the last send
        }
        let Some(text) = self
            .editor
            .workspace
            .documents
            .get(id)
            .map(|d| d.to_string())
        else {
            return false;
        };
        let sent_ok = match sent {
            None => self.lsp.did_open(path, lang, &text),
            Some(_) => self.lsp.did_change(path, lang, &text),
        };
        if sent_ok {
            self.lsp_sent_revision.insert(id, rev);
        }
        sent_ok
    }

    /// Re-request the passive whole-doc features (semantic tokens §7.1, inlay hints §7.2, code lens
    /// §6.4, folding §7.3) for a just-synced doc — each gated so push/unsupported servers are inert
    /// and cancelable so a typing burst supersedes intermediate requests.
    fn request_passive_features(&mut self, id: editor_core::DocId, path: &Path, lang: &str) {
        if self.lsp.supports_semantic_tokens(lang) {
            self.lsp.request_semantic_tokens(path, lang);
        }
        if self.lsp.supports_inlay_hints(lang) {
            let end_line = self
                .editor
                .workspace
                .documents
                .get(id)
                .map(|d| d.len_lines() as u32)
                .unwrap_or(0);
            self.lsp.request_inlay_hints(path, lang, end_line);
        }
        if self.lsp.supports_code_lens(lang) {
            self.lsp.request_code_lens(path, lang);
        }
        if self.lsp.supports_folding(lang) {
            self.lsp.request_folding_ranges(path, lang);
        }
    }

    /// Debounced diagnostics pull (§5.1): only for pull servers, and only after the buffer has been
    /// quiet for [`PULL_DEBOUNCE`]. Re-arms the timer on each revision change; fires once quiet.
    fn poll_pull_diagnostics(&mut self, id: editor_core::DocId, path: &Path, lang: &str, rev: u64) {
        if !self.lsp.supports_pull(lang) || self.lsp_pulled_revision.get(&id).copied() == Some(rev)
        {
            return;
        }
        let now = std::time::Instant::now();
        match self.lsp_pull_deadline.get(&id).copied() {
            Some((armed_rev, fire_at)) if armed_rev == rev => {
                if now >= fire_at && self.lsp.request_pull_diagnostics(path, lang) {
                    self.lsp_pulled_revision.insert(id, rev);
                    self.lsp_pull_deadline.remove(&id);
                }
            }
            // First sight of this revision (or a stale earlier arm) → (re)arm the timer.
            _ => {
                self.lsp_pull_deadline
                    .insert(id, (rev, now + PULL_DEBOUNCE));
            }
        }
    }

    /// LSP position of the primary cursor: `(path, language, line, utf16_char)`.
    pub(super) fn lsp_position(&self) -> Option<(PathBuf, String, u32, u32)> {
        let doc = self.editor.active_document()?;
        let path = doc.path.clone()?;
        let lang = doc.language.clone()?;
        let head = doc.selections.primary().head;
        let (line, col) = doc.char_to_line_col(head);
        let text = doc.line_text(line);
        let text = text.trim_end_matches(['\n', '\r']);
        let char16 = editor_lsp::position::char_col_to_utf16(text, col);
        Some((path, lang, line as u32, char16))
    }
}
