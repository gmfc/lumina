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
                // Translate to primitive diagnostics and broadcast to the `diagnostics` plugin,
                // which owns the model (transport stays here).
                let doc = crate::lsp::path_from_uri(&update.uri)
                    .and_then(|path| self.editor.workspace.find_by_path(&path));
                let diagnostics = update
                    .diagnostics
                    .into_iter()
                    .map(to_primitive_diag)
                    .collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspDiagnostics { doc, diagnostics });
            }
            LspEvent::Hover(text) => {
                self.editor.overlay = Some(crate::editor::Overlay::Info(text));
            }
            LspEvent::Goto(loc) => {
                // Hand a single navigation target to the `lsp-nav` plugin, which jumps via
                // `Host::open_location` (the app owns the actual open + caret placement on drain).
                if let Some(location) = to_primitive_location(&loc) {
                    self.editor
                        .pending_events
                        .push(editor_plugin::event::Event::LspGoto(location));
                }
            }
            LspEvent::Completion(items) => {
                // Broadcast to the `completion` plugin, which anchors + filters into a popup.
                let items = items.into_iter().map(to_primitive_completion).collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspCompletion(items));
            }
            LspEvent::Rename(edit) => self.apply_workspace_edit(edit),
            LspEvent::References(locs) => {
                let items = locs
                    .iter()
                    .filter_map(|l| {
                        Some(editor_plugin::LspNavItem {
                            location: to_primitive_location(l)?,
                            label: location_label(l),
                        })
                    })
                    .collect();
                self.push_locations("References", items);
            }
            LspEvent::DocumentSymbols(syms) => {
                // Every symbol is in the active document; resolve its path once.
                let Some(path) = self
                    .editor
                    .active_document()
                    .and_then(|d| d.path.clone())
                    .map(|p| p.to_string_lossy().into_owned())
                else {
                    return;
                };
                let items = syms
                    .into_iter()
                    .map(|s| editor_plugin::LspNavItem {
                        label: format!("{}{}", "  ".repeat(s.depth), s.name),
                        location: editor_plugin::LspLocation {
                            path: path.clone(),
                            line: s.line,
                            character: s.character,
                        },
                    })
                    .collect();
                self.push_locations("Symbols", items);
            }
            LspEvent::Error(message) => {
                self.editor.status_message = Some(format!("LSP: {message}"));
            }
        }
    }

    /// Hand a navigation location set (references / document symbols) to the `lsp-nav` plugin,
    /// which opens the picker and owns the jump. Shared by both response arms.
    fn push_locations(&mut self, title: &str, items: Vec<editor_plugin::LspNavItem>) {
        self.editor
            .pending_events
            .push(editor_plugin::event::Event::LspLocations {
                title: title.to_string(),
                items,
            });
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

    /// Forward a plugin-queued LSP request to the manager, resolving the primary cursor position
    /// app-side (the `lsp` plugin only expresses intent via `Host::lsp_request`). Symbols need no
    /// cursor; the rest share the `lsp_position` lookup.
    pub(super) fn dispatch_lsp_request(&mut self, kind: editor_plugin::LspRequestKind) {
        use editor_plugin::LspRequestKind as K;
        if let K::DocumentSymbols = kind {
            self.request_document_symbols();
            return;
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
            K::References => self.lsp.request_references(&p, &l, line, ch),
            K::Rename(name) => self.lsp.request_rename(&p, &l, line, ch, &name),
            K::DocumentSymbols => false, // handled above
        };
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

/// Translate an `editor-lsp` diagnostic into the kernel's primitive `LspDiagnostic` (so the
/// `diagnostics` plugin owns the model without depending on `editor-lsp`).
fn to_primitive_diag(d: editor_lsp::Diagnostic) -> editor_plugin::LspDiagnostic {
    use editor_lsp::Severity as S;
    use editor_plugin::LspSeverity as P;
    editor_plugin::LspDiagnostic {
        line: d.line,
        start_char16: d.start_char16,
        end_line: d.end_line,
        end_char16: d.end_char16,
        severity: match d.severity {
            S::Error => P::Error,
            S::Warning => P::Warning,
            S::Info => P::Info,
            S::Hint => P::Hint,
        },
        message: d.message,
    }
}

/// Translate an `editor-lsp` completion item into the kernel's primitive `LspCompletionItem`.
fn to_primitive_completion(it: editor_lsp::CompletionItem) -> editor_plugin::LspCompletionItem {
    editor_plugin::LspCompletionItem {
        label: it.label,
        detail: it.detail,
        insert_text: it.insert_text,
        kind: it.kind,
    }
}

/// Resolve an `editor-lsp` location's URI to a filesystem path and package it as the primitive
/// [`editor_plugin::LspLocation`] the `lsp-nav` plugin jumps to. `None` for a non-`file:` URI.
fn to_primitive_location(loc: &editor_lsp::Location) -> Option<editor_plugin::LspLocation> {
    let path = crate::lsp::path_from_uri(&loc.uri)?;
    Some(editor_plugin::LspLocation {
        path: path.to_string_lossy().into_owned(),
        line: loc.line,
        character: loc.character,
    })
}

/// A `file:line:col` label for a location picker row (plan §2.3).
fn location_label(loc: &editor_lsp::Location) -> String {
    let file = crate::lsp::path_from_uri(&loc.uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| loc.uri.clone());
    format!("{file}:{}:{}", loc.line + 1, loc.character + 1)
}
