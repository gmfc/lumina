//! LSP glue: notifying the server, issuing requests, and handling responses.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Notify the LSP of changes to the active document (debounced by revision), and open
    /// documents the server hasn't seen. Inert unless a server is configured.
    pub(super) fn update_lsp(&mut self) {
        if !self.lsp.is_enabled() {
            return;
        }
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(doc) = self.editor.workspace.documents.get(id) else {
            return;
        };
        let (Some(path), Some(lang)) = (doc.path.clone(), doc.language.clone()) else {
            return;
        };
        let rev = doc.revision;
        // Serialize the rope only when we actually have something to send. This runs every
        // frame; materializing a multi-MB document to a String on each unchanged tick would be
        // needless allocation churn.
        match self.lsp_sent_revision.get(&id).copied() {
            None => {
                let text = doc.to_string();
                self.lsp.did_open(&path, &lang, &text);
                self.lsp_sent_revision.insert(id, rev);
            }
            Some(sent) if sent != rev => {
                let text = doc.to_string();
                self.lsp.did_change(&path, &lang, &text);
                self.lsp_sent_revision.insert(id, rev);
            }
            _ => {}
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

    /// Act on a high-level LSP event (response or notification).
    pub(super) fn handle_lsp_event(&mut self, event: crate::lsp::LspEvent) {
        use crate::lsp::LspEvent;
        match event {
            LspEvent::Diagnostics(update) => {
                if let Some(path) = crate::lsp::path_from_uri(&update.uri) {
                    if let Some(id) = self.editor.workspace.find_by_path(&path) {
                        self.editor.diagnostics.insert(id, update.diagnostics);
                    }
                }
            }
            LspEvent::Hover(text) => {
                self.editor.overlay = Some(crate::editor::Overlay::Info(text));
            }
            LspEvent::Goto(loc) => self.goto_location(loc),
            LspEvent::Completion(items) => self.open_completion(items),
            LspEvent::Rename(edit) => self.apply_workspace_edit(edit),
            LspEvent::References(locs) => {
                let entries = locs
                    .into_iter()
                    .map(|l| {
                        let label = location_label(&l);
                        (l, label)
                    })
                    .collect();
                self.open_locations_picker(entries, "References");
            }
            LspEvent::DocumentSymbols(syms) => {
                let uri = self
                    .editor
                    .active_document()
                    .and_then(|d| d.path.as_ref())
                    .map(|p| crate::lsp::uri_for(p));
                let Some(uri) = uri else {
                    return;
                };
                let entries = syms
                    .into_iter()
                    .map(|s| {
                        let label = format!("{}{}", "  ".repeat(s.depth), s.name);
                        let loc = editor_lsp::Location {
                            uri: uri.clone(),
                            line: s.line,
                            character: s.character,
                            end_line: s.line,
                            end_character: s.character,
                        };
                        (loc, label)
                    })
                    .collect();
                self.open_locations_picker(entries, "Symbols");
            }
            LspEvent::Error(message) => {
                self.editor.status_message = Some(format!("LSP: {message}"));
            }
        }
    }

    /// Open a picker over LSP locations; selecting one jumps there (plan §2.3). The concrete
    /// `Location`s are parked on `EditorState::nav_locations`, indexed by the picker item id.
    pub(super) fn open_locations_picker(
        &mut self,
        entries: Vec<(editor_lsp::Location, String)>,
        title: &str,
    ) {
        if entries.is_empty() {
            self.editor.status_message = Some(format!("No {title}"));
            return;
        }
        let mut locs = Vec::with_capacity(entries.len());
        let items: Vec<crate::picker::PickerItem> = entries
            .into_iter()
            .enumerate()
            .map(|(i, (loc, label))| {
                locs.push(loc);
                crate::picker::PickerItem {
                    id: i.to_string(),
                    label,
                }
            })
            .collect();
        self.editor.nav_locations = locs;
        self.editor.picker = Some(crate::picker::Picker::new(
            crate::picker::PickerKind::Locations,
            title,
            items,
        ));
    }

    /// Open a definition location and place the cursor on it.
    pub(super) fn goto_location(&mut self, loc: editor_lsp::Location) {
        let Some(path) = crate::lsp::path_from_uri(&loc.uri) else {
            return;
        };
        self.open_path(&path);
        if let Some(doc) = self.editor.active_document_mut() {
            let off = lsp_pos_to_char(doc, loc.line, loc.character);
            doc.set_caret(off);
        }
    }

    /// Apply an LSP `WorkspaceEdit` (rename) across the affected documents as transactions.
    pub(super) fn apply_workspace_edit(&mut self, edit: editor_lsp::WorkspaceEdit) {
        let mut count = 0usize;
        for (uri, edits) in edit.changes {
            let Some(path) = crate::lsp::path_from_uri(&uri) else {
                continue;
            };
            if self.editor.workspace.find_by_path(&path).is_none() {
                self.open_path(&path);
            }
            let Some(id) = self.editor.workspace.find_by_path(&path) else {
                continue;
            };
            let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
                continue;
            };
            let mut changes: Vec<editor_core::transaction::Change> = edits
                .iter()
                .map(|te| {
                    let start = lsp_pos_to_char(doc, te.start_line, te.start_char16);
                    let end = lsp_pos_to_char(doc, te.end_line, te.end_char16);
                    let (lo, hi) = (start.min(end), start.max(end));
                    editor_core::transaction::Change {
                        at: lo,
                        removed: doc.rope().slice(lo..hi).to_string(),
                        inserted: te.new_text.clone(),
                    }
                })
                .collect();
            changes.sort_by_key(|c| c.at);
            let txn = editor_core::Transaction::from_changes(changes);
            if txn.is_empty() {
                continue;
            }
            let before = doc.selections.clone();
            let inverse = txn.apply(doc);
            doc.dirty = true;
            let after = doc.selections.clone();
            doc.history
                .record(txn, inverse, before, after, editor_core::GroupBreak::Force);
            count += 1;
        }
        if count > 0 {
            self.editor.status_message = Some(format!("Renamed across {count} file(s)"));
        }
    }

    /// Issue an LSP request for the primary cursor position, if one resolves. The three
    /// position-based requests (hover / definition / completion) share this lookup.
    pub(super) fn lsp_request(&mut self, req: LspRequest) {
        if let Some((p, l, line, ch)) = self.lsp_position() {
            match req {
                LspRequest::Hover => self.lsp.request_hover(&p, &l, line, ch),
                LspRequest::Definition => self.lsp.request_definition(&p, &l, line, ch),
                LspRequest::Implementation => self.lsp.request_implementation(&p, &l, line, ch),
                LspRequest::TypeDefinition => self.lsp.request_type_definition(&p, &l, line, ch),
                LspRequest::Completion => self.lsp.request_completion(&p, &l, line, ch),
                LspRequest::References => self.lsp.request_references(&p, &l, line, ch),
            };
        }
    }

    /// Request the symbols in the active document (no cursor position needed).
    pub(super) fn request_document_symbols(&mut self) {
        let info = self.editor.active_document().and_then(|d| {
            let path = d.path.clone()?;
            let lang = d.language.clone()?;
            Some((path, lang))
        });
        if let Some((path, lang)) = info {
            self.lsp.request_document_symbols(&path, &lang);
        }
    }
}

/// A `file:line:col` label for a location picker row (plan §2.3).
fn location_label(loc: &editor_lsp::Location) -> String {
    let file = crate::lsp::path_from_uri(&loc.uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| loc.uri.clone());
    format!("{file}:{}:{}", loc.line + 1, loc.character + 1)
}
